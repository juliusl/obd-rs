//! The configfs side of the TCMU device lifecycle.
//!
//! A `target_core_user` backstore whose `dev_config` points at an overlaybd
//! JSON config, plus a `tcm_loop` nexus that turns it into a local `/dev/sdX`.
//!
//! Teardown is order sensitive - LUN symlink, `lun_0`, `tpgt_1`, `naa.*`,
//! backstore, HBA - and every step here is idempotent so the cleanup sweep can
//! safely run over a half-built device.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

pub fn configfs_root() -> PathBuf {
    PathBuf::from(CONFIGFS)
}

pub fn backstore_path(name: &str) -> PathBuf {
    configfs_root().join("core").join(CORE_HBA).join(name)
}

pub fn tpgt_path(naa: &str) -> PathBuf {
    configfs_root().join("loopback").join(naa).join("tpgt_1")
}

pub fn lun0_path(naa: &str) -> PathBuf {
    tpgt_path(naa).join("lun").join("lun_0")
}

pub fn lun_link_path(naa: &str, name: &str) -> PathBuf {
    lun0_path(naa).join(name)
}

/// Write a configfs attribute.
///
/// configfs expects exactly one `write(2)` per value with no trailing newline,
/// so this deliberately bypasses buffered IO. `enable` can return `EAGAIN`
/// while the daemon is still attaching the device, which is what the retries
/// are for.
pub fn write_attr(path: &Path, value: &str, retries: u32, delay: Duration) -> Result<()> {
    use std::io::Write;

    let mut last: Option<std::io::Error> = None;
    for _ in 0..retries.max(1) {
        match std::fs::OpenOptions::new().write(true).open(path) {
            Ok(mut file) => match file.write_all(value.as_bytes()) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let again = err.raw_os_error() == Some(11); // EAGAIN
                    last = Some(err);
                    if !again {
                        break;
                    }
                }
            },
            Err(err) => {
                let again = err.raw_os_error() == Some(11);
                last = Some(err);
                if !again {
                    break;
                }
            }
        }
        std::thread::sleep(delay);
    }
    Err(Error::io(
        format!("writing '{value}' to {}", path.display()),
        last.unwrap_or_else(|| std::io::Error::other("unknown configfs failure")),
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
    // Linux dev_t packing, as in makedev(3).
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
/// `tcm_loop` recycles SCSI host:channel:target triples, so a node can exist
/// with the right `dev_t` while the device behind it is still being set up (or
/// torn down), which `mount` reports as "not a valid block device".
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
/// `address` attribute; LUN 0 is the one we linked. The node itself is created
/// asynchronously by udev and SCSI names get recycled, so this waits until the
/// node exists, its `dev_t` matches what sysfs reports, *and* it is readable.
/// Without both waits, back-to-back runs intermittently fail with
/// `mount: /dev/sdb is not a valid block device`.
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
    let deadline = Instant::now() + timeout;
    let mut last_seen = String::new();

    while Instant::now() < deadline {
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
                if let Some(devt) = sysfs_devt(&block_dir.join(name).join("dev"))
                    && node_matches(&node, devt)
                    && node_readable(&node)
                {
                    return Ok((node, address));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }

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

/// Wait for the SCSI device to disappear before anyone reuses the triple.
///
/// `tcm_loop` recycles `host:channel:target`, so returning while the old device
/// is still being removed makes the *next* device resolve onto a node that is
/// about to vanish.
pub fn wait_for_scsi_removal(address: &str, timeout: Duration) {
    let target = PathBuf::from(format!("/sys/class/scsi_device/{address}:0"));
    let deadline = Instant::now() + timeout;
    while target.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Read the daemon's `resultFile`, waiting for it to appear.
///
/// overlaybd reports launch failures here rather than through the configfs
/// write, so this has to be checked explicitly after `enable`.
pub fn await_result(result_file: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(result_file) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    String::new()
}

/// Remove a symlink, ignoring "already gone".
pub fn rm_symlink(path: &Path) {
    if path.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(path);
    }
}

/// Remove a configfs directory, ignoring "already gone" and "not empty".
///
/// The shared HBA directory is expected to fail while other devices still live
/// under it, which is why this is deliberately infallible.
pub fn rmdir(path: &Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir(path);
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
