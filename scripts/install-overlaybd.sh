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
# on a stock install the daemon fails to start with status=203/EXEC.
#
# This script covers the half a package manager owns - the repository and the
# package - and hands the reconciliation to lib/shell/obd-setup.sh, which the
# obd-rs package runs from its own postinst. `make package` builds that package
# and is the shorter path on a host that can install one.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

OVERLAYBD_PACKAGE_NAME="${OVERLAYBD_PACKAGE_NAME:-containerd-overlaybd}"
OVERLAYBD_PMC_BIN_DIR="${OVERLAYBD_PMC_BIN_DIR:-/usr/bin/overlaybd}"
OVERLAYBD_BIN_DIR="${OVERLAYBD_BIN_DIR:-/opt/overlaybd/bin}"

log() { printf '\n==> %s\n' "$1"; }
info() { printf '    %s\n' "$1"; }
die() {
  printf '\nERROR: %s\n' "$1" >&2
  exit 1
}

[[ "$(id -u)" -eq 0 ]] || die "must run as root: sudo $0"
[[ "$(uname -s)" == "Linux" ]] || die "overlaybd needs Linux (configfs + target_core_user)"

install_pmc_repo() {
  [[ -r /etc/os-release ]] || die "cannot read /etc/os-release"
  # shellcheck disable=SC1091
  . /etc/os-release

  if command -v apt-get >/dev/null 2>&1; then
    case "${ID:-}" in
    ubuntu | debian) ;;
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
  mariner:2* | cbl-mariner:2*) repo_path="mariner/2.0" ;;
  rhel:8* | centos:8* | rocky:8* | almalinux:8*) repo_path="rhel/8" ;;
  rhel:9* | centos:9* | rocky:9* | almalinux:9*) repo_path="rhel/9" ;;
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

log "installing overlaybd from PMC ($OVERLAYBD_PACKAGE_NAME)"
install_overlaybd

# The rest is the reconciliation the package leaves undone, and is shared with
# the obd-rs package's postinst rather than duplicated here.
exec "$REPO_ROOT/lib/shell/obd-setup.sh" --assets "$REPO_ROOT/lib" --daemon auto
