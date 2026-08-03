#!/usr/bin/env bash
# Build the obd-rs system packages: a .deb, a .rpm, or both.
#
#   tools/package.sh [deb|rpm|all]
#
# Both land in target/packages/. What they contain and why is in Cargo.toml
# under [package.metadata.deb] and [package.metadata.generate-rpm].
#
# This is the automated form of the Installation section of README.md: the
# package depends on containerd-overlaybd and, from its postinst, performs the
# same reconciliation scripts/install-overlaybd.sh performs by hand.
#
# Not shipped - it builds the thing that ships, so it lives in tools/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${OBD_PACKAGE_OUT_DIR:-$REPO_ROOT/target/packages}"
WHAT="${1:-all}"

info() { printf '\n==> %s\n' "$1"; }
die() {
  printf '\nERROR: %s\n' "$1" >&2
  exit 1
}

case "$WHAT" in
deb | rpm | all) ;;
*) die "usage: tools/package.sh [deb|rpm|all]" ;;
esac

# A .deb and a .rpm both carry a Linux binary, and neither cargo-deb nor
# cargo-generate-rpm cross-compiles one for you. The Makefile sends this into
# the devcontainer from anywhere else; this is the check for a direct call.
[[ "$(uname -s)" == "Linux" ]] ||
  die "packaging needs a Linux host or the devcontainer: run 'make package', which dispatches there"

# Preflight the toolchain rather than failing halfway through a build.
# OBD_PACKAGE_INSTALL_TOOLS=1 installs what is missing instead of refusing,
# which is what CI wants: a hosted runner starts with neither, and the failure
# message below is advice no automation can act on.
missing=()
if [[ "$WHAT" == "deb" || "$WHAT" == "all" ]]; then
  cargo deb --version >/dev/null 2>&1 || missing+=("cargo-deb")
fi
if [[ "$WHAT" == "rpm" || "$WHAT" == "all" ]]; then
  cargo generate-rpm --version >/dev/null 2>&1 || missing+=("cargo-generate-rpm")
fi
if [[ "${#missing[@]}" -gt 0 ]]; then
  if [[ "${OBD_PACKAGE_INSTALL_TOOLS:-0}" == "1" ]]; then
    info "installing the packaging tools: ${missing[*]}"
    # --locked so a build months from now resolves the dependency versions the
    # tool was released with, rather than whatever is newest today.
    cargo install --locked "${missing[@]}"
  else
    die "missing packaging tools: ${missing[*]}
       install them with: cargo install --locked ${missing[*]}
       or set OBD_PACKAGE_INSTALL_TOOLS=1 to have this script do it"
  fi
fi

cd "$REPO_ROOT"
mkdir -p "$OUT_DIR"

info "building obdctl in release mode"
cargo build --release

# cargo-deb strips on its own, cargo-generate-rpm does not and says so in its
# README, so the binary is stripped here once for both.
if command -v strip >/dev/null 2>&1; then
  strip -s target/release/obdctl
  info "stripped target/release/obdctl ($(stat -c %s target/release/obdctl) bytes)"
fi

built=()

if [[ "$WHAT" == "deb" || "$WHAT" == "all" ]]; then
  info "building the deb"
  # --no-build: the release binary above is what goes in, and a rebuild here
  # would undo the strip.
  cargo deb --no-build --no-strip --output "$OUT_DIR" >/dev/null
  while IFS= read -r package; do built+=("$package"); done < <(find "$OUT_DIR" -name '*.deb' -newermt '-5 minutes')
fi

if [[ "$WHAT" == "rpm" || "$WHAT" == "all" ]]; then
  info "building the rpm"
  cargo generate-rpm --output "$OUT_DIR" >/dev/null
  while IFS= read -r package; do built+=("$package"); done < <(find "$OUT_DIR" -name '*.rpm' -newermt '-5 minutes')
fi

info "built"
for package in "${built[@]}"; do
  printf '    %s (%s bytes)\n' "$package" "$(stat -c %s "$package")"
done

cat <<'EOF'

    The package depends on containerd-overlaybd from packages.microsoft.com,
    so that repository has to be configured on the target host first:

      curl -fsSLO https://packages.microsoft.com/config/ubuntu/24.04/packages-microsoft-prod.deb
      sudo dpkg -i packages-microsoft-prod.deb && sudo apt-get update
      sudo apt-get install -y ./obd-rs_*.deb

    Then: obdctl preflight

    obdctl links glibc, so build on the distribution you are installing on: a
    package built here carries this host's glibc requirement, and an older
    target refuses it.
EOF
