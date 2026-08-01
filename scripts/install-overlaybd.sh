#!/usr/bin/env bash
# Install overlaybd from packages.microsoft.com (PMC) and wire it into the
# layout the tooling expects. Run as root:
#
#   sudo ./scripts/install-overlaybd.sh
#
# Idempotent: already-satisfied steps are skipped.
#
# Why this is more than "apt install": the PMC containerd-overlaybd package
# installs its binaries under /usr/bin/overlaybd and ships nothing else - no
# /etc/overlaybd/overlaybd.json, no cred.json and no ext4_64 baselayer. Its own
# systemd unit still hardcodes ExecStart=/opt/overlaybd/bin/overlaybd-tcmu, so
# on a stock install the daemon fails to start with status=203/EXEC. This
# script reconciles that.
set -euo pipefail

OVERLAYBD_PACKAGE_NAME="${OVERLAYBD_PACKAGE_NAME:-containerd-overlaybd}"
OVERLAYBD_PMC_BIN_DIR="${OVERLAYBD_PMC_BIN_DIR:-/usr/bin/overlaybd}"
OVERLAYBD_BIN_DIR="${OVERLAYBD_BIN_DIR:-/opt/overlaybd/bin}"
OVERLAYBD_BASELAYER="${OVERLAYBD_BASELAYER:-/opt/overlaybd/baselayers/ext4_64}"
# The baselayer is a checked-in artifact of the overlaybd source tree, not a
# build output, so it is fetched from a tag rather than a release asset.
OVERLAYBD_BASELAYER_REF="${OVERLAYBD_BASELAYER_REF:-v1.0.18}"
OVERLAYBD_BASELAYER_URL="${OVERLAYBD_BASELAYER_URL:-https://raw.githubusercontent.com/containerd/overlaybd/${OVERLAYBD_BASELAYER_REF}/baselayers/ext4_64.tar.gz}"
# Where the daemon's stdio goes when there is no init system to capture it. Its
# own log is /var/log/overlaybd.log; this catches what dies before that opens.
OVERLAYBD_TCMU_LOG="${OVERLAYBD_TCMU_LOG:-/var/log/overlaybd-tcmu.out}"

log() { printf '\n==> %s\n' "$1"; }
info() { printf '    %s\n' "$1"; }
die() { printf '\nERROR: %s\n' "$1" >&2; exit 1; }

# True when a live daemon is present. pgrep alone is not enough: a daemon that
# died where nothing reaps it - a container whose PID 1 is not an init - stays
# visible under its own name as a zombie, and pgrep matches it. ps reports
# those with state Z (ps(1), procps-ng 4.0.4, PROCESS STATE CODES).
daemon_running() {
  local pid
  for pid in $(pgrep -x overlaybd-tcmu 2>/dev/null); do
    [[ "$(ps -o stat= -p "$pid" 2>/dev/null)" == Z* ]] || return 0
  done
  return 1
}

[[ "$(id -u)" -eq 0 ]] || die "must run as root: sudo $0"
[[ "$(uname -s)" == "Linux" ]] || die "overlaybd needs Linux (configfs + target_core_user)"

install_pmc_repo() {
  [[ -r /etc/os-release ]] || die "cannot read /etc/os-release"
  # shellcheck disable=SC1091
  . /etc/os-release

  if command -v apt-get >/dev/null 2>&1; then
    case "${ID:-}" in
      ubuntu|debian) ;;
      *) die "unsupported apt distro '${ID:-unknown}'; install the PMC repo manually" ;;
    esac
    if dpkg -s packages-microsoft-prod >/dev/null 2>&1; then
      info "packages-microsoft-prod already installed"
    else
      local repo_url tmp_deb
      repo_url="https://packages.microsoft.com/config/${ID}/${VERSION_ID}/packages-microsoft-prod.deb"
      tmp_deb="$(mktemp /tmp/packages-microsoft-prod.XXXXXX.deb)"
      curl -fsSL -o "$tmp_deb" "$repo_url" ||
        die "failed to download $repo_url; install packages-microsoft-prod manually"
      dpkg -i "$tmp_deb"
      rm -f "$tmp_deb"
      info "installed packages-microsoft-prod for ${ID} ${VERSION_ID}"
    fi
    apt-get update
    return
  fi

  local repo_path
  case "${ID:-}:${VERSION_ID:-}" in
    azurelinux:3*) repo_path="azurelinux/3.0" ;;
    mariner:2*|cbl-mariner:2*) repo_path="mariner/2.0" ;;
    rhel:8*|centos:8*|rocky:8*|almalinux:8*) repo_path="rhel/8" ;;
    rhel:9*|centos:9*|rocky:9*|almalinux:9*) repo_path="rhel/9" ;;
    *) die "unsupported rpm distro '${ID:-unknown} ${VERSION_ID:-}'; install the PMC repo manually" ;;
  esac

  if rpm -q packages-microsoft-prod >/dev/null 2>&1; then
    info "packages-microsoft-prod already installed"
  else
    rpm -Uvh "https://packages.microsoft.com/config/${repo_path}/packages-microsoft-prod.rpm" ||
      die "failed to install the PMC repo rpm"
  fi
}

install_overlaybd() {
  if [[ -x "$OVERLAYBD_PMC_BIN_DIR/overlaybd-tcmu" || -x "$OVERLAYBD_BIN_DIR/overlaybd-tcmu" ]]; then
    info "overlaybd binaries already installed"
    return
  fi
  install_pmc_repo

  if command -v apt-get >/dev/null 2>&1; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y "$OVERLAYBD_PACKAGE_NAME"
  elif command -v dnf >/dev/null 2>&1; then
    dnf install -y "$OVERLAYBD_PACKAGE_NAME"
  elif command -v tdnf >/dev/null 2>&1; then
    tdnf install -y "$OVERLAYBD_PACKAGE_NAME"
  elif command -v yum >/dev/null 2>&1; then
    yum install -y "$OVERLAYBD_PACKAGE_NAME"
  else
    die "no supported package manager found (need apt-get, dnf, tdnf or yum)"
  fi
  info "installed $OVERLAYBD_PACKAGE_NAME"
}

# Symlink the PMC binaries into /opt/overlaybd/bin, which is both what the
# shipped systemd unit expects and what the rest of the overlaybd tooling
# assumes. Mirrors the compat layer acr-mirror uses for these same packages.
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
      die "$OVERLAYBD_BIN_DIR/$binary missing after installing $OVERLAYBD_PACKAGE_NAME"
  done

  mkdir -p /etc/overlaybd /opt/overlaybd/registry_cache /opt/overlaybd/gzip_cache
  if [[ -f /etc/overlaybd/overlaybd.json ]]; then
    info "/etc/overlaybd/overlaybd.json already present"
  else
    cat >/etc/overlaybd/overlaybd.json <<'OBDCFG'
{
    "logConfig": {
        "logLevel": 1,
        "logPath": "/var/log/overlaybd.log"
    },
    "cacheConfig": {
        "cacheType": "file",
        "cacheDir": "/opt/overlaybd/registry_cache",
        "cacheSizeGB": 4
    },
    "gzipCacheConfig": {
        "enable": true,
        "cacheDir": "/opt/overlaybd/gzip_cache",
        "cacheSizeGB": 4
    },
    "credentialConfig": {
        "mode": "file",
        "path": "/opt/overlaybd/cred.json"
    },
    "ioEngine": 0,
    "download": {
        "enable": false,
        "delay": 600,
        "delayExtra": 30,
        "maxMBps": 100
    },
    "p2pConfig": {
        "enable": false,
        "address": "localhost:19145/dadip2p"
    },
    "exporterConfig": {
        "enable": false,
        "uriPrefix": "/metrics",
        "port": 9863,
        "updateInterval": 60000000
    },
    "enableAudit": true,
    "auditPath": "/var/log/overlaybd-audit.log",
    "registryFsVersion": "v2"
}
OBDCFG
    info "seeded /etc/overlaybd/overlaybd.json (PMC ships no config)"
  fi

  if [[ ! -f /opt/overlaybd/cred.json ]]; then
    printf '{"auths":{}}\n' >/opt/overlaybd/cred.json
    chmod 600 /opt/overlaybd/cred.json
    info "seeded /opt/overlaybd/cred.json"
  fi
}

install_baselayer() {
  if [[ -f "$OVERLAYBD_BASELAYER" ]]; then
    info "baselayer $OVERLAYBD_BASELAYER already present"
    return
  fi
  command -v curl >/dev/null 2>&1 || die "curl is required to fetch the baselayer"
  local dir tmp
  dir="$(dirname "$OVERLAYBD_BASELAYER")"
  mkdir -p "$dir"
  tmp="$(mktemp -d)"
  curl -fsSL -o "$tmp/ext4_64.tar.gz" "$OVERLAYBD_BASELAYER_URL" || {
    rm -rf "$tmp"
    die "failed to download the baselayer from $OVERLAYBD_BASELAYER_URL"
  }
  tar -xzf "$tmp/ext4_64.tar.gz" -C "$tmp" || { rm -rf "$tmp"; die "baselayer archive is not a gzip tar"; }
  [[ -f "$tmp/ext4_64" ]] || { rm -rf "$tmp"; die "baselayer archive did not contain ext4_64"; }
  # Move into place atomically so a half-written baselayer is never observable.
  mv "$tmp/ext4_64" "$OVERLAYBD_BASELAYER.tmp"
  mv "$OVERLAYBD_BASELAYER.tmp" "$OVERLAYBD_BASELAYER"
  rm -rf "$tmp"
  info "installed baselayer $OVERLAYBD_BASELAYER ($(stat -c %s "$OVERLAYBD_BASELAYER") bytes)"
}

load_modules() {
  local module
  for module in target_core_user tcm_loop; do
    if [[ -d "/sys/module/$module" ]]; then
      info "$module already loaded"
    else
      modprobe "$module" ||
        die "modprobe $module failed; this kernel needs TARGET_CORE_USER and LOOPBACK_TARGET"
      info "loaded $module"
    fi
  done
  if [[ -d /etc/modules-load.d ]]; then
    printf 'target_core_user\ntcm_loop\n' >/etc/modules-load.d/overlaybd.conf
    info "persisted both modules via /etc/modules-load.d/overlaybd.conf"
  fi
}

start_daemon() {
  if command -v systemctl >/dev/null 2>&1 && [[ -d /run/systemd/system ]]; then
    if ! systemctl cat overlaybd-tcmu.service >/dev/null 2>&1; then
      [[ -f /opt/overlaybd/overlaybd-tcmu.service ]] ||
        die "overlaybd-tcmu.service not found after package install"
      cp /opt/overlaybd/overlaybd-tcmu.service /etc/systemd/system/overlaybd-tcmu.service
      systemctl daemon-reload
    fi
    systemctl enable --now overlaybd-tcmu
    sleep 2
    systemctl is-active --quiet overlaybd-tcmu ||
      die "overlaybd-tcmu did not start; check 'journalctl -u overlaybd-tcmu' and /var/log/overlaybd.log"
    info "overlaybd-tcmu is active"
  elif daemon_running; then
    info "overlaybd-tcmu is already running (no systemd here)"
  else
    start_daemon_directly
  fi
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
    [[ "$waited" -lt 10 ]] ||
      die "overlaybd-tcmu did not start; check $log and /var/log/overlaybd.log"
    sleep 1
    waited=$((waited + 1))
  done
  sleep 2
  daemon_running ||
    die "overlaybd-tcmu exited right after starting; check $log and /var/log/overlaybd.log"
  info "started overlaybd-tcmu directly (no init system here), logging to $log"
}

log "installing overlaybd from PMC ($OVERLAYBD_PACKAGE_NAME)"
install_overlaybd

log "wiring the layout under /opt/overlaybd"
wire_layout
install_baselayer

log "loading kernel modules"
load_modules

log "starting the overlaybd-tcmu daemon"
start_daemon

log "done"
info "verify with: obdctl preflight"
