#!/usr/bin/env bash
# Wire an installed overlaybd into the layout this tooling expects, and leave
# the daemon running.
#
#   obd-setup.sh [--assets DIR] [--daemon auto|systemd|none]
#
# Run as root. Idempotent: already-satisfied steps are skipped, so it is safe
# from a package postinst, from a container's start hook and by hand.
#
# It installs nothing from a package manager. `scripts/install-overlaybd.sh`
# does that first and then calls this; the obd-rs package declares
# containerd-overlaybd as a dependency and calls this from its postinst. Both
# paths share one implementation so the host ends up identical either way.
#
# What needs reconciling: the PMC containerd-overlaybd package installs its
# binaries under /usr/bin/overlaybd and ships nothing else - no
# /etc/overlaybd/overlaybd.json, no cred.json, no ext4_64 baselayer - while its
# systemd unit sets ExecStart=/opt/overlaybd/bin/overlaybd-tcmu, so a stock
# install fails with status=203/EXEC.
#
#   --daemon auto     start it however this host can, and fail if it will not
#                     start. The default, and what a human running the
#                     installer expects.
#   --daemon systemd  start it through systemd when systemd is running, and
#                     otherwise leave it alone. Best effort throughout: a
#                     package must install cleanly in a chroot, an image build
#                     and on a kernel without TCMU.
#   --daemon none     leave the daemon alone entirely.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The asset tree, laid out the same in the repository (lib/) and in the package
# (/usr/share/obd-rs/), which is what lets this script find it relative to
# itself in both.
ASSETS="${OBD_ASSET_DIR:-$(cd "$SCRIPT_DIR/.." && pwd)}"
DAEMON_MODE=auto

OVERLAYBD_PMC_BIN_DIR="${OVERLAYBD_PMC_BIN_DIR:-/usr/bin/overlaybd}"
OVERLAYBD_BIN_DIR="${OVERLAYBD_BIN_DIR:-/opt/overlaybd/bin}"
OVERLAYBD_BASELAYER="${OVERLAYBD_BASELAYER:-/opt/overlaybd/baselayers/ext4_64}"
# Where the daemon's stdio goes when there is no init system to capture it. Its
# own log is /var/log/overlaybd.log; this catches what dies before that opens.
OVERLAYBD_TCMU_LOG="${OVERLAYBD_TCMU_LOG:-/var/log/overlaybd-tcmu.out}"

log() { printf '\n==> %s\n' "$1"; }
info() { printf '    %s\n' "$1"; }
warn() { printf '    WARNING: %s\n' "$1" >&2; }
die() {
  printf '\nERROR: %s\n' "$1" >&2
  exit 1
}

usage() {
  cat >&2 <<'EOF'
usage: obd-setup.sh [--assets DIR] [--daemon auto|systemd|none]

  --assets DIR   asset tree holding overlaybd/, systemd/, modules-load/ and
                 shell/ (default: the parent of this script)
  --daemon MODE  auto (default), systemd or none
EOF
  exit 2
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
  --assets)
    ASSETS="${2:-}"
    [[ -n "$ASSETS" ]] || usage
    shift 2
    ;;
  --daemon)
    DAEMON_MODE="${2:-}"
    case "$DAEMON_MODE" in auto | systemd | none) ;; *) usage ;; esac
    shift 2
    ;;
  -h | --help) usage ;;
  *) usage ;;
  esac
done

[[ "$(id -u)" -eq 0 ]] || die "must run as root: sudo $0"
[[ "$(uname -s)" == "Linux" ]] || die "overlaybd needs Linux (configfs + target_core_user)"
[[ -d "$ASSETS/overlaybd" ]] || die "no asset tree at $ASSETS (expected $ASSETS/overlaybd/overlaybd.json); pass --assets"

# True when a live daemon is present in this PID namespace. Read from /proc
# rather than through pgrep, which procps-ng provides and a minimal image -
# Azure Linux 3.0's base/core, for one - does not install.
#
# A daemon that died where nothing reaps it - a container whose PID 1 is not an
# init - keeps its /proc/<pid>/comm and would otherwise count as running, so
# zombies are excluded. In /proc/<pid>/stat the second field is the command in
# parentheses and may itself contain spaces and parentheses, so the state
# follows the last ')' (proc_pid_stat(5), Linux man-pages 6.9).
daemon_running() {
  local comm_file line state
  for comm_file in /proc/[0-9]*/comm; do
    [[ -r "$comm_file" ]] || continue
    [[ "$(<"$comm_file")" == "overlaybd-tcmu" ]] || continue
    read -r line <"${comm_file%/comm}/stat" 2>/dev/null || continue
    line="${line##*) }"
    state="${line%% *}"
    [[ "$state" == Z ]] || return 0
  done
  return 1
}

# Copy an asset into place unless something is already there, so a config a
# host has edited is never overwritten.
seed() {
  local src="$1" dest="$2" mode="$3"
  if [[ -e "$dest" ]]; then
    info "$dest already present"
    return
  fi
  [[ -f "$src" ]] || die "missing asset $src"
  mkdir -p "$(dirname "$dest")"
  install -m "$mode" "$src" "$dest"
  info "seeded $dest"
}

# Symlink the PMC binaries into /opt/overlaybd/bin, which is both what the
# shipped systemd unit expects and what the rest of the overlaybd tooling
# assumes.
wire_layout() {
  if [[ -d "$OVERLAYBD_PMC_BIN_DIR" ]]; then
    mkdir -p "$OVERLAYBD_BIN_DIR"
    local linked=0 binary src
    for binary in overlaybd-tcmu overlaybd-create overlaybd-commit overlaybd-zfile \
      overlaybd-apply overlaybd-merge overlaybd-resize turboOCI-apply; do
      src="$OVERLAYBD_PMC_BIN_DIR/$binary"
      [[ -f "$src" ]] || continue
      # Never clobber a real binary from the upstream GitHub-release package.
      if [[ -e "$OVERLAYBD_BIN_DIR/$binary" && ! -L "$OVERLAYBD_BIN_DIR/$binary" ]]; then
        continue
      fi
      ln -sfn "$src" "$OVERLAYBD_BIN_DIR/$binary"
      linked=$((linked + 1))
    done
    [[ "$linked" -eq 0 ]] || info "symlinked $linked binaries into $OVERLAYBD_BIN_DIR"
  fi

  local binary
  for binary in overlaybd-tcmu overlaybd-create overlaybd-commit; do
    [[ -x "$OVERLAYBD_BIN_DIR/$binary" ]] ||
      die "$OVERLAYBD_BIN_DIR/$binary missing; install containerd-overlaybd first (scripts/install-overlaybd.sh)"
  done

  mkdir -p /opt/overlaybd/registry_cache /opt/overlaybd/gzip_cache
  seed "$ASSETS/overlaybd/overlaybd.json" /etc/overlaybd/overlaybd.json 644
  # 600: the daemon reads registry credentials from here.
  seed "$ASSETS/overlaybd/cred.json" /opt/overlaybd/cred.json 600
}

# Point the shipped unit at the binary the package actually installed. Skipped
# when the obd-rs package already ships the same drop-in under /usr/lib, and
# when there is no /usr/bin/overlaybd layout to correct - an upstream
# GitHub-release install has real binaries in /opt/overlaybd/bin and needs no
# drop-in.
install_unit_dropin() {
  local packaged=/usr/lib/systemd/system/overlaybd-tcmu.service.d/10-obd-rs.conf
  local dest=/etc/systemd/system/overlaybd-tcmu.service.d/10-obd-rs.conf
  [[ -x "$OVERLAYBD_PMC_BIN_DIR/overlaybd-tcmu" ]] || return 0
  [[ -d /etc/systemd/system ]] || return 0
  if [[ -f "$packaged" ]]; then
    info "unit drop-in already shipped at $packaged"
    return 0
  fi
  mkdir -p "$(dirname "$dest")"
  install -m 644 "$ASSETS/systemd/10-obd-rs.conf" "$dest"
  info "installed $dest (the shipped unit's ExecStart points into /opt/overlaybd/bin)"
}

install_baselayer() {
  OVERLAYBD_BASELAYER="$OVERLAYBD_BASELAYER" "$ASSETS/shell/obd-baselayer.sh"
}

# Best effort by design: a host whose kernel lacks TARGET_CORE_USER or
# LOOPBACK_TARGET cannot drive devices, but that is `obdctl preflight`'s
# finding to report, not a reason for an install to fail. In --daemon auto the
# daemon start below turns it into a failure anyway.
load_modules() {
  local module
  for module in target_core_user tcm_loop; do
    if [[ -d "/sys/module/$module" ]]; then
      info "$module already loaded"
    elif modprobe "$module" 2>/dev/null; then
      info "loaded $module"
    else
      warn "modprobe $module failed; this kernel needs TARGET_CORE_USER and LOOPBACK_TARGET to drive devices"
    fi
  done
  if [[ -d /usr/lib/modules-load.d && -f /usr/lib/modules-load.d/overlaybd.conf ]]; then
    info "both modules persisted via /usr/lib/modules-load.d/overlaybd.conf"
  elif [[ -d /etc/modules-load.d ]]; then
    install -m 644 "$ASSETS/modules-load/overlaybd.conf" /etc/modules-load.d/overlaybd.conf
    info "persisted both modules via /etc/modules-load.d/overlaybd.conf"
  fi
}

systemd_running() {
  command -v systemctl >/dev/null 2>&1 && [[ -d /run/systemd/system ]]
}

# Returns non-zero rather than dying, so the caller decides whether a daemon
# that will not start fails the run.
start_daemon_systemd() {
  if ! systemctl cat overlaybd-tcmu.service >/dev/null 2>&1; then
    # The upstream GitHub-release package leaves its unit here instead of in a
    # systemd directory; the PMC package installs it under /usr/lib/systemd.
    [[ -f /opt/overlaybd/overlaybd-tcmu.service ]] || {
      warn "no overlaybd-tcmu.service found; the overlaybd package did not ship one"
      return 1
    }
    install -m 644 /opt/overlaybd/overlaybd-tcmu.service /etc/systemd/system/overlaybd-tcmu.service
  fi
  systemctl daemon-reload
  systemctl enable --now overlaybd-tcmu || return 1
  sleep 2
  systemctl is-active --quiet overlaybd-tcmu || {
    warn "overlaybd-tcmu did not stay active; check 'journalctl -u overlaybd-tcmu' and /var/log/overlaybd.log"
    return 1
  }
  info "overlaybd-tcmu is active"
}

# Run the daemon without an init system. A container is the case that matters:
# the packaged unit file is present but nothing runs it, so the daemon is
# launched directly and restarted by whatever starts the container.
start_daemon_directly() {
  local log="$OVERLAYBD_TCMU_LOG"
  # setsid puts the daemon in its own session, so it outlives this script and
  # the terminal that ran it; without it, a hangup on that terminal reaches the
  # daemon (setsid(2), Linux man-pages 6.9).
  setsid "$OVERLAYBD_BIN_DIR/overlaybd-tcmu" >>"$log" 2>&1 </dev/null &

  # Startup failures - a missing /etc/overlaybd/overlaybd.json, a netlink
  # interface the kernel refuses - surface as an immediate exit rather than an
  # error on stderr, so liveness after a settling period is the signal.
  local waited=0
  while ! daemon_running; do
    [[ "$waited" -lt 10 ]] || {
      warn "overlaybd-tcmu did not start; check $log and /var/log/overlaybd.log"
      return 1
    }
    sleep 1
    waited=$((waited + 1))
  done
  sleep 2
  daemon_running || {
    warn "overlaybd-tcmu exited right after starting; check $log and /var/log/overlaybd.log"
    return 1
  }
  info "started overlaybd-tcmu directly (no init system here), logging to $log"
}

start_daemon() {
  case "$DAEMON_MODE" in
  none)
    info "leaving the daemon alone (--daemon none)"
    return 0
    ;;
  systemd)
    if systemd_running; then
      start_daemon_systemd ||
        warn "leaving overlaybd-tcmu stopped; 'obdctl preflight' reports what is missing"
    else
      info "systemd is not running here, so overlaybd-tcmu was not started"
      info "start it with: $SCRIPT_DIR/obd-setup.sh --daemon auto"
    fi
    return 0
    ;;
  esac

  if systemd_running; then
    start_daemon_systemd || die "overlaybd-tcmu did not start"
  elif daemon_running; then
    info "overlaybd-tcmu is already running (no systemd here)"
  else
    start_daemon_directly || die "overlaybd-tcmu did not start"
  fi
}

log "wiring the layout under /opt/overlaybd"
wire_layout
install_unit_dropin

log "installing the baselayer"
install_baselayer

log "loading kernel modules"
load_modules

log "starting the overlaybd-tcmu daemon"
start_daemon

log "done"
info "verify with: obdctl preflight"
