#!/usr/bin/env bash
# Generate the ext4 baselayer every overlaybd device stacks as its bottom
# lower, without a device and without the network.
#
#   obd-baselayer.sh [--out PATH] [--size-gb N] [--force]
#
# Idempotent: an existing baselayer is left alone unless --force is passed.
#
# Why generate rather than download: the PMC containerd-overlaybd package ships
# no baselayer, and upstream keeps ext4_64.tar.gz as a checked-in artifact of
# its source tree rather than a release asset, so fetching it means reaching
# into a git tag over the network at install time.
#
# It is two steps, neither of which needs TCMU, a loop device or a running
# daemon:
#
#   1. `overlaybd-create --mkfs` writes an empty ext4 into a fresh writable
#      layer. The filesystem is built in-process with libext2fs against the
#      LSMT file - overlaybd v1.0.18 src/tools/overlaybd-create.cpp:105 calls
#      photon's make_extfs, which formats through ext2fs_initialize
#      (PhotonLibOS fs/extfs/mkfs.cpp:94).
#   2. `overlaybd-commit -z` seals that writable layer into the read-only zfile
#      layer a device can stack as a lower.
#
# The result carries no journal: make_extfs enables extents, 64bit, flex_bg and
# friends but never has_journal (PhotonLibOS fs/extfs/mkfs.cpp:65-77). A
# lower-only device mounts `ro,noload` either way; what this changes is the
# writable case, where an unclean shutdown needs fsck rather than a replay.
set -euo pipefail

OVERLAYBD_BASELAYER="${OVERLAYBD_BASELAYER:-/opt/overlaybd/baselayers/ext4_64}"
# 64 GiB, matching obd::tools::BASELAYER_SIZE_GB. A writable layer stacked on
# top has to advertise the same virtual size.
OVERLAYBD_BASELAYER_SIZE_GB="${OVERLAYBD_BASELAYER_SIZE_GB:-64}"
FORCE=0

info() { printf '    %s\n' "$1"; }
die() {
  printf '\nERROR: %s\n' "$1" >&2
  exit 1
}

usage() {
  cat >&2 <<'EOF'
usage: obd-baselayer.sh [--out PATH] [--size-gb N] [--force]

  --out PATH     where to write the baselayer (default /opt/overlaybd/baselayers/ext4_64)
  --size-gb N    virtual size in GiB (default 64; must match the layers stacked on it)
  --force        regenerate even when the output already exists
EOF
  exit 2
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
  --out)
    OVERLAYBD_BASELAYER="${2:-}"
    [[ -n "$OVERLAYBD_BASELAYER" ]] || usage
    shift 2
    ;;
  --size-gb)
    OVERLAYBD_BASELAYER_SIZE_GB="${2:-}"
    [[ "$OVERLAYBD_BASELAYER_SIZE_GB" =~ ^[0-9]+$ ]] || usage
    shift 2
    ;;
  --force)
    FORCE=1
    shift
    ;;
  -h | --help) usage ;;
  *) usage ;;
  esac
done

# Same search order as obd::tools::find, so the script and the library agree on
# which install they are looking at.
find_binary() {
  local name="$1" dir
  for dir in ${OVERLAYBD_BIN_DIR:+"$OVERLAYBD_BIN_DIR"} /opt/overlaybd/bin /usr/bin/overlaybd; do
    if [[ -x "$dir/$name" ]]; then
      printf '%s' "$dir/$name"
      return 0
    fi
  done
  command -v "$name" 2>/dev/null && return 0
  return 1
}

if [[ -f "$OVERLAYBD_BASELAYER" && "$FORCE" -eq 0 ]]; then
  info "baselayer $OVERLAYBD_BASELAYER already present"
  exit 0
fi

create="$(find_binary overlaybd-create)" ||
  die "overlaybd-create not found in /opt/overlaybd/bin, /usr/bin/overlaybd or PATH; install containerd-overlaybd first"
commit="$(find_binary overlaybd-commit)" ||
  die "overlaybd-commit not found in /opt/overlaybd/bin, /usr/bin/overlaybd or PATH; install containerd-overlaybd first"

# --mkfs is what makes this a two-step build instead of a device lifecycle.
# Older overlaybd releases have no such flag, and CLI11 rejects the whole
# invocation rather than ignoring it, so the check is worth its line.
"$create" --help 2>&1 | grep -q -- '--mkfs' ||
  die "$create has no --mkfs flag; overlaybd 1.0.10 or newer is required to generate a baselayer"

tmp="$(mktemp -d)"
# Both tools log a screenful at INFO on success. Keep it for a failure and drop
# it otherwise.
log="$tmp/build.log"
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT

info "building a ${OVERLAYBD_BASELAYER_SIZE_GB}G ext4 layer with $create --mkfs"
if ! "$create" --mkfs "$tmp/base.data" "$tmp/base.index" "$OVERLAYBD_BASELAYER_SIZE_GB" \
  >"$log" 2>&1; then
  # overlaybd-create refused the work. Reasons, cheapest to rule out first:
  #   1. /tmp is full or too small for the formatted layer - an empty 64 GiB
  #      ext4 is a few MB, but a tmpfs /tmp on a small host can still be short.
  #   2. The build is running under a libext2fs too old for the features
  #      make_extfs enables, so ext2fs_initialize fails rather than the CLI.
  #   3. The binary is from an overlaybd old enough to accept --mkfs but not
  #      implement it for sparse layers.
  sed 's/^/      /' "$log" >&2
  die "overlaybd-create --mkfs failed; its output is above"
fi

info "sealing it into a read-only zfile layer with $commit -z"
if ! "$commit" -z -m "obd-rs baselayer: empty ext4, ${OVERLAYBD_BASELAYER_SIZE_GB}G virtual" \
  "$tmp/base.data" "$tmp/base.index" "$tmp/baselayer" >>"$log" 2>&1; then
  # The commit ran and rejected the work. Reasons, cheapest first:
  #   1. The temporary directory filled up between the two steps.
  #   2. The zfile compressor was built without the codec it defaults to (lz4),
  #      which shows up here and not in step 1.
  sed 's/^/      /' "$log" >&2
  die "overlaybd-commit failed; its output is above"
fi

dir="$(dirname "$OVERLAYBD_BASELAYER")"
mkdir -p "$dir"
# Same directory as the destination, so the rename is atomic and a half-written
# baselayer is never observable by a device that is coming up.
staged="$OVERLAYBD_BASELAYER.tmp.$$"
cp "$tmp/baselayer" "$staged"
chmod 644 "$staged"
mv -f "$staged" "$OVERLAYBD_BASELAYER"

info "installed baselayer $OVERLAYBD_BASELAYER ($(stat -c %s "$OVERLAYBD_BASELAYER") bytes)"
