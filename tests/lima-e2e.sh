#!/usr/bin/env bash
# End-to-end test of the real device lifecycle. Needs Linux, root, and a
# working overlaybd install (scripts/install-overlaybd.sh).
#
#   sudo ./tests/lima-e2e.sh
#
# This is the part `cargo test` cannot cover: configfs, TCMU, mount(2) and the
# overlaybd binaries. It drives everything through `obdctl`, which is also how
# the PoC scripts are meant to consume this crate.
set -euo pipefail

OBDCTL="${OBDCTL:-$(dirname "$(dirname "$(readlink -f "$0")")")/target/debug/obdctl}"
WORK="${OBD_TEST_WORK:-/var/lib/obd-rs-test}"
MNT_A="${OBD_TEST_MNT_A:-/mnt/obd-test-a}"
MNT_B="${OBD_TEST_MNT_B:-/mnt/obd-test-b}"
BASELAYER="${OVERLAYBD_BASELAYER:-/opt/overlaybd/baselayers/ext4_64}"

pass() { printf '  [ok]   %s\n' "$1"; }
fail() { printf '  [FAIL] %s\n' "$1" >&2; exit 1; }
step() { printf '\n==> %s\n' "$1"; }

[[ "$(id -u)" -eq 0 ]] || fail "must run as root: sudo $0"
[[ -x "$OBDCTL" ]] || fail "obdctl not built at $OBDCTL (cargo build)"

trap '"$OBDCTL" cleanup --mount "$MNT_A" --mount "$MNT_B" >/dev/null 2>&1 || true' EXIT

step "preflight"
"$OBDCTL" preflight || fail "preflight failed"

step "starting from a clean slate"
"$OBDCTL" cleanup --mount "$MNT_A" --mount "$MNT_B"
rm -rf "$WORK"
mkdir -p "$WORK"

step "creating a sparse writable layer"
"$OBDCTL" layer create --data "$WORK/upper.data" --index "$WORK/upper.index" --size-gb 64
[[ -f "$WORK/upper.data" ]] || fail "upper.data missing"
pass "layer created"

# overlaybd-create opens outputs O_EXCL, so a second create must be refused
# rather than silently clobbering a layer that a device may still be using.
if "$OBDCTL" layer create --data "$WORK/upper.data" --index "$WORK/upper.index" --size-gb 64 \
    >/dev/null 2>&1; then
  fail "creating over an existing layer should be refused"
fi
pass "refuses to overwrite an existing layer"

step "device A: writable, baselayer + upper"
"$OBDCTL" config --out "$WORK/device-a.json" --result-file "$WORK/result-a" \
  --lower "$BASELAYER" --upper-data "$WORK/upper.data" --upper-index "$WORK/upper.index"
grep -q '"upper"' "$WORK/device-a.json" || fail "config has no upper"
pass "wrote device config"

UP_JSON="$("$OBDCTL" --json device up --name poc_e2ea --config "$WORK/device-a.json" \
  --result-file "$WORK/result-a" --naa-suffix 0091 --mount "$MNT_A" --subdir job-out)"
echo "$UP_JSON"
BLOCK_A="$(echo "$UP_JSON" | sed -n 's/.*"block_device": "\([^"]*\)".*/\1/p')"
[[ -b "$BLOCK_A" ]] || fail "no block device from device up"
pass "device up -> $BLOCK_A"

mountpoint -q "$MNT_A" || fail "$MNT_A is not mounted"
pass "mounted rw at $MNT_A"
[[ -d "$MNT_A/job-out" ]] || fail "--subdir did not create job-out"
pass "subdir created through the sandboxed handle"

step "writing into the device, then tearing it down"
echo "written by lima-e2e at $(date -Is)" > "$MNT_A/job-out/marker.txt"
sync
"$OBDCTL" device down --name poc_e2ea --naa-suffix 0091 --mount "$MNT_A"
mountpoint -q "$MNT_A" && fail "$MNT_A still mounted after device down"
pass "device torn down"

step "committing the writable layer"
"$OBDCTL" layer commit --data "$WORK/upper.data" --index "$WORK/upper.index" \
  --out "$WORK/job.commit" --message "lima-e2e"
[[ -s "$WORK/job.commit" ]] || fail "commit produced nothing"
pass "committed $(stat -c %s "$WORK/job.commit") bytes"

step "device B: read-only, baselayer + committed layer"
"$OBDCTL" config --out "$WORK/device-b.json" --result-file "$WORK/result-b" \
  --lower "$BASELAYER" --lower "$WORK/job.commit"
grep -q '"upper"' "$WORK/device-b.json" && fail "read-only config should have no upper"
pass "wrote read-only device config"

"$OBDCTL" --json device up --name poc_e2eb --config "$WORK/device-b.json" \
  --result-file "$WORK/result-b" --naa-suffix 0092 --mount "$MNT_B" --read-only

[[ -f "$MNT_B/job-out/marker.txt" ]] || fail "marker did not survive the commit"
grep -q "written by lima-e2e" "$MNT_B/job-out/marker.txt" || fail "marker contents wrong"
pass "marker.txt is visible through the committed layer"

if echo nope > "$MNT_B/job-out/should-fail.txt" 2>/dev/null; then
  fail "writes to the ro mount should be refused"
fi
pass "writes to the ro mount are refused"

"$OBDCTL" device down --name poc_e2eb --naa-suffix 0092 --mount "$MNT_B"
pass "device B torn down"

# tcm_loop recycles SCSI host:channel:target triples, so a device coming up
# immediately after one went down is exactly the case that used to race.
step "back-to-back reruns (exercises the tcm_loop recycling waits)"
for i in 1 2 3; do
  rm -rf "$WORK/rerun"
  mkdir -p "$WORK/rerun"
  "$OBDCTL" layer create --data "$WORK/rerun/u.data" --index "$WORK/rerun/u.index" --size-gb 64
  "$OBDCTL" config --out "$WORK/rerun/dev.json" --result-file "$WORK/rerun/result" \
    --lower "$BASELAYER" --upper-data "$WORK/rerun/u.data" --upper-index "$WORK/rerun/u.index"
  "$OBDCTL" device up --name poc_e2er --config "$WORK/rerun/dev.json" \
    --result-file "$WORK/rerun/result" --naa-suffix 0093 --mount "$MNT_A" >/dev/null
  mountpoint -q "$MNT_A" || fail "rerun $i: mount failed"
  touch "$MNT_A/rerun-$i"
  "$OBDCTL" device down --name poc_e2er --naa-suffix 0093 --mount "$MNT_A"
  pass "rerun $i ok"
done

step "cleanup sweeps leftovers"
"$OBDCTL" cleanup --mount "$MNT_A" --mount "$MNT_B"
# A second sweep must be a no-op rather than an error.
"$OBDCTL" cleanup >/dev/null || fail "cleanup is not idempotent"
pass "cleanup is idempotent"

[[ -d /sys/kernel/config/target/core/user_1 ]] && \
  fail "user_1 HBA left behind after cleanup"
pass "no configfs leftovers"

printf '\n### lima-e2e: PASS\n'
