#!/bin/bash
# RPM %post scriptlet, run after the package's files are installed.
#
# The same obd-setup.sh that scripts/install-overlaybd.sh calls, so a host
# wired by the package and a host wired by the script end up identical.
#
# --daemon systemd because a scriptlet must not spawn a daemon of its own: a
# package has to install cleanly in a chroot, in an image build, and on a
# kernel without TCMU. Where systemd is running the daemon is started properly;
# where it is not, `obdctl preflight` reports what is left to do.
#
# $1 is 1 on a fresh install and 2 on an upgrade; obd-setup.sh is idempotent,
# so both take the same path.
set -e

/usr/share/obd-rs/shell/obd-setup.sh --daemon systemd
