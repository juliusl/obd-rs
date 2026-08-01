//! The configfs side of the TCMU device lifecycle.
//!
//! A `target_core_user` backstore whose `dev_config` points at an overlaybd
//! JSON config, plus a `tcm_loop` nexus that turns it into a local `/dev/sdX`.
//!
//! Teardown is order sensitive - LUN symlink, `lun_0`, `tpgt_1`, `naa.*`,
//! backstore, HBA - and every step here is idempotent so the cleanup sweep can
//! safely run over a half-built device.
//!
//! # Tracing
//!
//! This is the crate's hot file: every wait in the lifecycle is a poll loop
//! here. The `trace` events are deliberately concentrated in it so that
//! enabling `obd::configfs=trace` gives timing for device attach, node
//! resolution and SCSI teardown without turning on anything else.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracing::{debug, error, instrument, trace, warn};

use crate::error::{Error, Result};

/// configfs root for the SCSI target subsystem.
pub const CONFIGFS: &str = "/sys/kernel/config/target";
/// The `target_core_user` HBA; the index is arbitrary but must be consistent.
pub const CORE_HBA: &str = "user_1";
/// Every backstore this crate creates starts with this.
pub const DEV_PREFIX: &str = "poc_";
/// Every `tcm_loop` nexus this crate creates starts with this.
pub const NAA_PREFIX: &str = "naa.5001405e0b0d";
/// Mirrors what accelerated-container-image's snapshotter uses.
pub const MAX_DATA_AREA_MB: u32 = 4;

/// Root of the SCSI target subsystem in configfs.
pub fn configfs_root() -> PathBuf {
    PathBuf::from(CONFIGFS)
}

/// Directory for a device's `target_core_user` backstore.
pub fn backstore_path(name: &str) -> PathBuf {
    configfs_root().join("core").join(CORE_HBA).join(name)
}

/// Target portal group for a `tcm_loop` nexus.
pub fn tpgt_path(naa: &str) -> PathBuf {
    configfs_root().join("loopback").join(naa).join("tpgt_1")
}

/// LUN 0 under a nexus; the only LUN this crate creates.
pub fn lun0_path(naa: &str) -> PathBuf {
    tpgt_path(naa).join("lun").join("lun_0")
}

/// The symlink that binds a backstore to LUN 0, which is what makes the
/// device appear as a `/dev/sdX`.
pub fn lun_link_path(naa: &str, name: &str) -> PathBuf {
    lun0_path(naa).join(name)
}

/// Write a configfs attribute.
///
/// The value is written with a single `write(2)` and no trailing newline:
/// "Configfs expects write(2) to store the entire buffer at once"
/// (kernel `Documentation/filesystems/configfs.rst`, "Normal attributes"),
/// which is why this bypasses buffered IO rather than using `fs::write`.
///
/// `retries` covers `EAGAIN`. Enabling a `target_core_user` backstore sends a
/// netlink event to the userspace handler, and the kernel fails that with
/// `-EAGAIN` while the interface is blocked - "Failing nl cmd %d on %s.
/// Interface is blocked." in `drivers/target/target_core_user.c` - which is the
/// window where the daemon has not finished attaching.
pub fn write_attr(path: &Path, value: &str, retries: u32, delay: Duration) -> Result<()> {
    use std::io::Write;

    let started = Instant::now();
    let mut last: Option<std::io::Error> = None;
    let mut attempts = 0u32;

    for _ in 0..retries.max(1) {
        attempts += 1;
        let outcome = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|mut file| file.write_all(value.as_bytes()));

        match outcome {
            Ok(()) => {
                trace!(
                    attempts,
                    elapsed_us = started.elapsed().as_micros() as u64,
                    "configfs-write"
                );
                if attempts > 1 {
                    // The EAGAIN retry is defensive: reaching here means the
                    // daemon was still attaching and the loop covered for it.
                    warn!(
                        attribute = %path.display(),
                        attempts,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "configfs accepted the write only after retrying EAGAIN; \
                         the overlaybd daemon was still attaching the device"
                    );
                }
                debug!(
                    "wrote '{value}' to the configfs attribute {} after {attempts} attempt(s)",
                    path.display()
                );
                return Ok(());
            }
            Err(err) => {
                let again = err.raw_os_error() == Some(11); // EAGAIN
                last = Some(err);
                if !again {
                    break;
                }
                trace!(attempts, "configfs-eagain");
            }
        }
        std::thread::sleep(delay);
    }

    let err = last.unwrap_or_else(|| std::io::Error::other("unknown configfs failure"));
    // Reasons this fires, cheapest to rule out first:
    //   1. Not running as root. configfs rejects the open with EACCES.
    //   2. `target_core_user` or `tcm_loop` is not loaded, so the attribute's
    //      parent directory does not exist: ENOENT.
    //   3. The overlaybd-tcmu daemon is not running, so nothing answers the
    //      `enable` write and the EAGAIN retries are exhausted.
    //   4. `dev_config` names a device config the daemon cannot open - wrong
    //      path, or a layer file that is missing or unreadable. The daemon logs
    //      the real cause to /var/log/overlaybd.log.
    //   5. The device name is already in use by a live backstore: EEXIST.
    error!(
        attribute = %path.display(),
        value,
        attempts,
        errno = err.raw_os_error().unwrap_or(-1),
        "configfs rejected the write: {err}"
    );
    Err(Error::io(
        format!("writing '{value}' to {}", path.display()),
        err,
    ))
}

/// Read a sysfs `dev` attribute ("major:minor").
fn sysfs_devt(attr: &Path) -> Option<(u32, u32)> {
    let text = std::fs::read_to_string(attr).ok()?;
    let (major, minor) = text.trim().split_once(':')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// True once the device node exists and refers to the expected `dev_t`.
#[cfg(target_os = "linux")]
fn node_matches(node: &Path, devt: (u32, u32)) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(node) else {
        return false;
    };
    let rdev = meta.rdev();
    // Linux packs dev_t as 12 major bits at 8, 20 more at 32, and the minor in
    // the gaps; see the glibc `makedev`/`major`/`minor` macros documented in
    // makedev(3) and defined in <sys/sysmacros.h>.
    let major = ((rdev >> 8) & 0xfff) as u32 | ((rdev >> 32) & !0xfffu64) as u32;
    let minor = (rdev & 0xff) as u32 | ((rdev >> 12) & !0xffu64) as u32;
    (major, minor) == devt
}

#[cfg(not(target_os = "linux"))]
fn node_matches(_node: &Path, _devt: (u32, u32)) -> bool {
    false
}

/// True once the node can actually be opened and read.
///
/// A node can exist with the right `dev_t` while the device behind it is still
/// being set up or torn down, which `mount` then reports as "not a valid block
/// device". Reading a sector is the cheapest way to tell the difference.
/// `tests/lima-e2e.sh` reruns a device back to back, which is the case that
/// exposes this.
fn node_readable(node: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(node) else {
        return false;
    };
    let mut buf = [0u8; 512];
    matches!(file.read(&mut buf), Ok(512))
}

/// Resolve `/dev/sdX` from the loopback nexus rather than guessing.
///
/// `tcm_loop` publishes the SCSI `host:channel:target` triple in the tpgt's
/// `address` attribute; LUN 0 is the one we linked. udev creates the node
/// asynchronously, and the kernel reuses SCSI addresses once a device is gone,
/// so this waits until the node exists, its `dev_t` matches what sysfs reports
/// for *this* device, and it is readable. Without all three, back-to-back runs
/// intermittently fail with `mount: /dev/sdb is not a valid block device`.
#[instrument(level = "debug", skip_all, fields(device = %device, naa = %naa))]
pub fn resolve_block_device(
    device: &str,
    naa: &str,
    timeout: Duration,
) -> Result<(PathBuf, String)> {
    let address_path = tpgt_path(naa).join("address");
    let address = std::fs::read_to_string(&address_path)
        .map_err(|e| Error::io(format!("reading {}", address_path.display()), e))?
        .trim()
        .to_string();

    let block_dir = PathBuf::from(format!("/sys/class/scsi_device/{address}:0/device/block"));
    debug!(
        "resolving the block device for {device} from SCSI address {address}, \
         watching {}",
        block_dir.display()
    );

    let started = Instant::now();
    let deadline = started + timeout;
    let mut last_seen = String::new();
    let mut polls = 0u32;
    // A node whose dev_t does not match sysfs is a *recycled* node: mounting it
    // would have reached the wrong device. Counted separately from a node that
    // simply is not readable yet, which is ordinary attach latency.
    let mut recycled = 0u32;
    let mut not_ready = 0u32;

    while Instant::now() < deadline {
        polls += 1;
        if block_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&block_dir)
        {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            if let Some(name) = names.first() {
                last_seen = name.clone();
                let node = PathBuf::from("/dev").join(name);
                if let Some(devt) = sysfs_devt(&block_dir.join(name).join("dev")) {
                    let matches = node_matches(&node, devt);
                    if matches && node_readable(&node) {
                        trace!(
                            polls,
                            recycled,
                            not_ready,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "node-resolved"
                        );
                        if recycled > 0 {
                            // Worth surfacing: without the dev_t check we would
                            // have mounted a node belonging to a torn-down
                            // device, which is the failure this guard exists for.
                            warn!(
                                device,
                                node = %node.display(),
                                recycled_polls = recycled,
                                "skipped a recycled device node before mounting; tcm_loop had \
                                 reused this SCSI address for a previous device"
                            );
                        }
                        debug!(
                            "resolved {device} to {} after {polls} poll(s) \
                             ({not_ready} not-ready, {recycled} recycled)",
                            node.display()
                        );
                        return Ok((node, address));
                    }
                    // Ordinary attach latency is flow information, not a
                    // warning: it happens on most launches.
                    if matches {
                        not_ready += 1;
                        trace!(polls, node = %node.display(), "node-not-readable");
                    } else {
                        recycled += 1;
                        trace!(polls, node = %node.display(), "node-recycled");
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Reasons this fires, cheapest to rule out first:
    //   1. udev is not running, so nothing creates /dev nodes for new devices.
    //      `last_seen` names the node sysfs expected; check whether it exists.
    //   2. The device attached but the daemon is wedged, so every read of the
    //      node fails and `not_ready` climbs for the whole timeout.
    //   3. The SCSI address was recycled faster than teardown released it, so
    //      `recycled` climbs: the node kept resolving to the previous device.
    //   4. tcm_loop failed to create the LUN at all, in which case the sysfs
    //      directory never appears and `last_seen` is <nothing>. dmesg carries
    //      the kernel-side reason.
    error!(
        device,
        expected_at = %block_dir.display(),
        last_seen = %if last_seen.is_empty() { "<nothing>" } else { &last_seen },
        polls,
        recycled,
        not_ready,
        timeout_s = timeout.as_secs(),
        "no usable block device appeared before the timeout"
    );
    Err(Error::NoBlockDevice {
        device: device.to_string(),
        path: block_dir,
        seen: if last_seen.is_empty() {
            "<nothing>".to_string()
        } else {
            last_seen
        },
    })
}

/// Wait for the SCSI device to disappear before anyone reuses the address.
///
/// The kernel reuses `host:channel:target` once a device is gone, so returning
/// while the old one is still being removed lets the *next* device resolve onto
/// a node that is about to vanish. `tests/lima-e2e.sh` reruns a device back to
/// back, which is the case that exposes this.
pub fn wait_for_scsi_removal(address: &str, timeout: Duration) {
    let target = PathBuf::from(format!("/sys/class/scsi_device/{address}:0"));
    let started = Instant::now();
    let deadline = started + timeout;
    let mut polls = 0u32;

    while target.exists() && Instant::now() < deadline {
        polls += 1;
        std::thread::sleep(Duration::from_millis(100));
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
    trace!(polls, elapsed_ms, "scsi-removal");
    if target.exists() {
        // Not fatal here - teardown has already removed the configfs entries -
        // but the next device on this address may race, so say so.
        warn!(
            scsi_address = address,
            elapsed_ms,
            "SCSI device did not disappear before the timeout; a device reusing this \
             address may fail to resolve its node"
        );
    } else {
        debug!("SCSI device {address}:0 disappeared after {elapsed_ms}ms");
    }
}

/// Read the daemon's `resultFile`, waiting for it to appear.
///
/// The configfs write succeeds even when the device fails to attach: overlaybd
/// writes `success` or the failure reason to this file instead (overlaybd
/// `src/image_service.cpp`, `set_result_file`), so it has to be checked
/// explicitly after `enable`.
pub fn await_result(result_file: &Path, timeout: Duration) -> String {
    let started = Instant::now();
    let deadline = started + timeout;
    let mut polls = 0u32;

    while Instant::now() < deadline {
        polls += 1;
        if let Ok(text) = std::fs::read_to_string(result_file) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                trace!(
                    polls,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "result-file"
                );
                debug!(
                    "overlaybd reported '{trimmed}' in {} after {polls} poll(s)",
                    result_file.display()
                );
                return trimmed.to_string();
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    trace!(
        polls,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "result-file-timeout"
    );
    debug!(
        "overlaybd wrote nothing to {} within {}s",
        result_file.display(),
        timeout.as_secs()
    );
    String::new()
}

/// Remove a symlink, ignoring "already gone".
pub fn rm_symlink(path: &Path) {
    if path.symlink_metadata().is_ok() {
        match std::fs::remove_file(path) {
            Ok(()) => debug!("removed the LUN symlink {}", path.display()),
            Err(err) => debug!("could not remove the LUN symlink {}: {err}", path.display()),
        }
    }
}

/// Remove a configfs directory, ignoring "already gone" and "not empty".
///
/// The shared HBA directory is expected to fail while other devices still live
/// under it, which is why this is deliberately infallible.
pub fn rmdir(path: &Path) {
    if path.is_dir() {
        match std::fs::remove_dir(path) {
            Ok(()) => debug!("removed the configfs directory {}", path.display()),
            // Expected for the shared HBA while other devices still live under
            // it, so this is flow information rather than a problem.
            Err(err) => debug!(
                "left the configfs directory {} in place: {err}",
                path.display()
            ),
        }
    }
}

/// Tail of the overlaybd log, for failure messages.
pub fn log_tail(log: &Path, lines: usize) -> String {
    match std::fs::read_to_string(log) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        }
        Err(_) => format!("(no {})", log.display()),
    }
}
