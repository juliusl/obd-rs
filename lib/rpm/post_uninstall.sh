#!/bin/bash
# RPM %postun scriptlet, run after the package's files are removed.
#
# $1 is the number of instances left behind: 0 on a final erase, 1 during the
# removal half of an upgrade, which must leave the host alone (rpm(8),
# "Triggerscripts and scriptlets").
#
# The cleanup below is inlined rather than delegated to the shipped scripts
# under /usr/share/obd-rs: rpm deletes those before it runs this, so there is
# nothing left to call.
set -e

[[ "${1:-0}" -eq 0 ]] || exit 0

# The unit drop-in this package shipped is gone by now, so systemd is holding a
# configuration that no longer exists on disk.
if [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload || true
fi

# The %post scriptlet symlinked the containerd-overlaybd binaries into
# /opt/overlaybd/bin. Left behind they become dangling the moment that package
# is removed, and a dangling symlink fails at exec time rather than at
# discovery time.
if [[ -d /opt/overlaybd/bin ]]; then
  for link in /opt/overlaybd/bin/*; do
    [[ -L "$link" ]] || continue
    case "$(readlink "$link")" in
    /usr/bin/overlaybd/*) rm -f "$link" ;;
    esac
  done
  rmdir --ignore-fail-on-non-empty /opt/overlaybd/bin
fi

# The baselayer is generated state, reproducible in a second from
# obd-baselayer.sh, so an erase takes it with everything else.
rm -f /opt/overlaybd/baselayers/ext4_64
[[ -d /opt/overlaybd/baselayers ]] &&
  rmdir --ignore-fail-on-non-empty /opt/overlaybd/baselayers

# Credentials are not package state. Only the untouched seed is removed; a
# cred.json someone has actually put registry auth into stays.
if [[ -f /opt/overlaybd/cred.json ]] &&
  [[ "$(cat /opt/overlaybd/cred.json)" == '{"auths":{}}' ]]; then
  rm -f /opt/overlaybd/cred.json
fi

# The cache directories obd-setup.sh created. Only if empty: a populated
# registry cache belongs to the daemon, which containerd-overlaybd still owns.
rmdir --ignore-fail-on-non-empty /opt/overlaybd/registry_cache \
  /opt/overlaybd/gzip_cache /opt/overlaybd 2>/dev/null || true

# rpm removes the files it owns but not the directories holding them, because
# cargo-generate-rpm declares no %dir entries for them. Empty ones are this
# package's to take.
rmdir --ignore-fail-on-non-empty \
  /usr/lib/systemd/system/overlaybd-tcmu.service.d \
  /usr/share/obd-rs/shell /usr/share/obd-rs/overlaybd /usr/share/obd-rs/systemd \
  /usr/share/obd-rs/modules-load /usr/share/obd-rs \
  /usr/share/doc/obd-rs /etc/overlaybd 2>/dev/null || true

exit 0
