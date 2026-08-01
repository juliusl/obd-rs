# Ubuntu 24.04 matches lima-dev.yaml and the PMC package that
# scripts/install-overlaybd.sh pulls in (containerd-overlaybd is published per
# distro release: containerd-overlaybd_1.0.18-ubuntu24.04u2), so the container
# and the VM end up running the same overlaybd build.
FROM mcr.microsoft.com/devcontainers/base:ubuntu-24.04

# kmod       - modprobe target_core_user and tcm_loop
# procps     - the installer checks daemon liveness with pgrep and ps
# curl, ca-certificates - the installer fetches the PMC repo and the baselayer
# build-essential, pkg-config - cc for the Rust link step
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        kmod \
        procps \
        curl \
        ca-certificates \
        build-essential \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Where the devcontainer spec puts this workspace. Setting it here means a plain
# `docker exec obd-rs-dev ...` - how anything outside VS Code reaches the
# container - starts in the repo rather than at /.
WORKDIR /workspaces/obd-rs
