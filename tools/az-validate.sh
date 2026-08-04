#!/usr/bin/env bash
# Validate a published obd-rs package on a fresh Azure VM of the distribution
# it was built for.
#
#   tools/az-validate.sh [--distro azurelinux3|ubuntu24] [--version vX.Y.Z] [--keep]
#
# A package is the one artifact this repository cannot validate on the machine
# that builds it: obdctl carries the glibc of its build host, the systemd unit
# drop-in only means anything where systemd is running, and the postinst has to
# find a kernel with TCMU. This creates a VM, installs the release asset the
# way a user would, runs the canonical device suite against the *packaged*
# obdctl, removes the package, and deletes everything.
#
# Everything lands in one resource group, so teardown is a single delete and
# runs even when the validation fails.
#
# Not shipped: it validates the thing that ships, so it lives in tools/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DISTRO="azurelinux3"
VERSION=""
KEEP=0
LOCATION="${OBD_AZ_LOCATION:-westus3}"
SIZE="${OBD_AZ_SIZE:-Standard_D2s_v5}"
RG="${OBD_AZ_RG:-obd-rs-validate-rg}"
REPO="${OBD_RS_REPO:-juliusl/obd-rs}"
RELEASE="${OBD_RS_RELEASE:-1}"
ADMIN="azureuser"

log() { printf '\n==> %s\n' "$1"; }
info() { printf '    %s\n' "$1"; }
die() {
  printf '\nERROR: %s\n' "$1" >&2
  exit 1
}

usage() {
  cat >&2 <<'EOF'
usage: tools/az-validate.sh [--distro DISTRO] [--version vX.Y.Z] [--keep]

  --distro   azurelinux3 (the rpm) or ubuntu24 (the deb). Default azurelinux3.
  --version  release tag to validate. Defaults to the crate version.
  --keep     leave the resource group behind for inspection.
EOF
  exit 2
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
  --distro)
    DISTRO="${2:-}"
    case "$DISTRO" in azurelinux3 | ubuntu24) ;; *) usage ;; esac
    shift 2
    ;;
  --version)
    VERSION="${2:-}"
    [[ -n "$VERSION" ]] || usage
    shift 2
    ;;
  --keep)
    KEEP=1
    shift
    ;;
  -h | --help) usage ;;
  *) usage ;;
  esac
done

command -v az >/dev/null 2>&1 || die "the az CLI is required: https://aka.ms/azure-cli"
az account show >/dev/null 2>&1 || die "not logged in: run 'az login'"

if [[ -z "$VERSION" ]]; then
  VERSION="v$(cd "$REPO_ROOT" && cargo pkgid | sed 's/.*[#@]//')"
fi
PLAIN_VERSION="${VERSION#v}"
# A pre-release wears three spellings: 0.1.2-rc2 in the manifest, 0.1.2~rc2
# inside the package because that is what sorts before 0.1.2, and 0.1.2+rc2 in
# the asset name because GitHub rewrites '~' but not '+'. Only the last of the
# three appears in a download URL.
ASSET_VERSION="${PLAIN_VERSION//-/+}"

case "$DISTRO" in
azurelinux3)
  IMAGE="MicrosoftCBLMariner:azure-linux-3:azure-linux-3:latest"
  ASSET="obd-rs-${ASSET_VERSION}-${RELEASE}.x86_64.rpm"
  # Azure Linux is published from packages.microsoft.com already, so
  # containerd-overlaybd resolves without adding a repository.
  ADD_REPO="true"
  INSTALL="tdnf install -y"
  REMOVE="tdnf remove -y obd-rs"
  QUERY_INSTALLED="rpm -q obd-rs"
  ;;
ubuntu24)
  IMAGE="Canonical:ubuntu-24_04-lts:server:latest"
  ASSET="obd-rs_${ASSET_VERSION}-${RELEASE}_amd64.deb"
  ADD_REPO="curl -fsSLO https://packages.microsoft.com/config/ubuntu/24.04/packages-microsoft-prod.deb &&
    dpkg -i packages-microsoft-prod.deb && apt-get update -qq"
  INSTALL="DEBIAN_FRONTEND=noninteractive apt-get install -y"
  REMOVE="DEBIAN_FRONTEND=noninteractive apt-get purge -y obd-rs"
  QUERY_INSTALLED="dpkg -s obd-rs"
  ;;
esac

VM="obd-rs-validate-$DISTRO"
ASSET_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"

# The suite that runs against the packaged binary is the repository's own, so a
# release is held to the same bar as a working tree.
E2E="$REPO_ROOT/tests/lima-e2e.sh"
[[ -f "$E2E" ]] || die "missing $E2E"

cleanup() {
  if [[ "$KEEP" -eq 1 ]]; then
    log "leaving $RG behind (--keep); delete it with: az group delete -n $RG --yes"
    return
  fi
  log "deleting resource group $RG"
  az group delete -n "$RG" --yes --no-wait -o none 2>/dev/null || true
  info "deletion started"
}
trap cleanup EXIT

remote() {
  az vm run-command invoke -g "$RG" -n "$VM" --command-id RunShellScript \
    --scripts "$1" --query "value[0].message" -o tsv
}

log "validating $VERSION on $DISTRO ($ASSET)"
info "resource group $RG in $LOCATION, vm $VM ($SIZE)"

az group create -n "$RG" -l "$LOCATION" -o none

# An ephemeral key, thrown away with the VM: run-command needs no inbound SSH,
# and az vm create insists on some credential.
KEYDIR="$(mktemp -d)"
ssh-keygen -t ed25519 -f "$KEYDIR/key" -N "" -q -C "obd-rs-validate"

log "creating $VM from $IMAGE"
az vm create -g "$RG" -n "$VM" \
  --image "$IMAGE" --size "$SIZE" \
  --admin-username "$ADMIN" --ssh-key-values "$KEYDIR/key.pub" \
  --public-ip-sku Standard --os-disk-size-gb 32 \
  --nic-delete-option Delete --os-disk-delete-option Delete \
  -o none
rm -rf "$KEYDIR"

log "installing $ASSET the way a user would"
remote "set -e
$ADD_REPO
curl -fsSL -o /tmp/$ASSET '$ASSET_URL'
$INSTALL /tmp/$ASSET
$QUERY_INSTALLED >/dev/null && echo 'package installed'
obdctl --version
echo '--- the daemon the shipped unit could not start ---'
systemctl is-enabled overlaybd-tcmu
systemctl is-active overlaybd-tcmu
systemctl show overlaybd-tcmu -p ExecStart --no-pager | head -1
echo '--- preflight ---'
obdctl preflight"

log "running the device suite against the packaged obdctl"
# The suite is shipped over as base64 so this works without inbound SSH.
E2E_B64="$(base64 <"$E2E" | tr -d '\n')"
remote "set -e
echo '$E2E_B64' | base64 -d > /tmp/lima-e2e.sh
chmod +x /tmp/lima-e2e.sh
OBDCTL=/usr/bin/obdctl /tmp/lima-e2e.sh 2>&1 | tail -25"

log "removing the package"
remote "set -e
$REMOVE >/dev/null 2>&1
echo '--- what the package left behind ---'
for path in /usr/bin/obdctl /etc/overlaybd /opt/overlaybd/baselayers /usr/share/obd-rs \
  /usr/lib/systemd/system/overlaybd-tcmu.service.d; do
  if [ -e \"\$path\" ]; then printf 'STILL THERE: %s\n' \"\$path\"; ls -A \"\$path\" 2>/dev/null | sed 's/^/      /'; else printf 'gone: %s\n' \"\$path\"; fi
done"

log "validation complete for $VERSION on $DISTRO"
