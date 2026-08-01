#!/usr/bin/env bash
# Bring the container up to the state a normal overlaybd host boots into. Runs
# on every container start, and is idempotent.
#
# A container inherits the Docker host's kernel but none of the per-boot setup
# an init system and udev perform, so three things have to be arranged here:
#
#   1. target_core_user and tcm_loop loaded - into the host's kernel, which the
#      container shares
#   2. configfs mounted at /sys/kernel/config
#   3. overlaybd installed and overlaybd-tcmu running
#
# Each step is best effort. A Docker host whose kernel lacks TARGET_CORE_USER or
# LOOPBACK_TARGET cannot drive devices at all, and the container is still worth
# having for `cargo build`, `cargo clippy` and `cargo test`, all of which run
# anywhere. Whatever is missing is named below, and `obdctl preflight` remains
# the authoritative check.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MISSING=()

log() { printf '\n==> %s\n' "$1"; }
info() { printf '    %s\n' "$1"; }
missing() {
  printf '    UNAVAILABLE: %s\n' "$1"
  MISSING+=("$1")
}

# postStartCommand runs as the remote user, which devcontainer.json sets to
# root. Re-exec through sudo so the script still works if that is changed.
if [[ "$(id -u)" -ne 0 ]]; then
  exec sudo -E "$0" "$@"
fi

log "loading the TCMU kernel modules"
for module in target_core_user tcm_loop; do
  if [[ -d "/sys/module/$module" ]]; then
    info "$module already loaded"
  elif modprobe "$module" 2>/dev/null; then
    info "loaded $module"
  else
    missing "modprobe $module. The module is loaded into the Docker host's kernel, so that kernel needs TARGET_CORE_USER and LOOPBACK_TARGET and /lib/modules has to be the host's tree."
  fi
done

log "mounting configfs"
if mountpoint -q /sys/kernel/config; then
  info "configfs already mounted at /sys/kernel/config"
elif mkdir -p /sys/kernel/config && mount -t configfs none /sys/kernel/config; then
  info "mounted configfs at /sys/kernel/config"
else
  missing "mount -t configfs none /sys/kernel/config. Needs a privileged container."
fi

log "installing overlaybd"
if "$REPO_ROOT/scripts/install-overlaybd.sh"; then
  info "install-overlaybd.sh completed"
else
  missing "scripts/install-overlaybd.sh. Re-run it by hand to see the failure."
fi

if [[ "${#MISSING[@]}" -eq 0 ]]; then
  log "ready"
  info "the full device lifecycle is available here"
  info "verify with: cargo build && ./target/debug/obdctl preflight"
  info "device state lives in the host kernel and outlives this container:"
  info "  ./target/debug/obdctl cleanup sweeps anything an earlier run left behind"
else
  log "ready for builds and unit tests only"
  for item in "${MISSING[@]}"; do
    printf '    - %s\n' "$item"
  done
  info "cargo build, cargo clippy and cargo test work; device operations will fail"
  info "check with: cargo build && ./target/debug/obdctl preflight"
fi
