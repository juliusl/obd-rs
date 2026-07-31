//! Device lifecycle tests against a real overlaybd install.
//!
//! These drive the **library** API directly, which the `obdctl` tests do not:
//! the CLI always hands devices off with `persist()`, so the RAII paths
//! (`down()`, and teardown from `Drop`) are only covered here.
//!
//! Requires Linux, root, and a working overlaybd. Skipped otherwise, so
//! `cargo test` stays green on a developer machine:
//!
//! ```sh
//! sudo -E OBD_DEVICE_TESTS=1 cargo test --test linux_device -- --test-threads=1
//! ```
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use obd::{Device, DeviceConfig, Lower, Mode, tools};

/// These mutate global host state (configfs, mounts), so they must not run
/// concurrently; the harness is invoked with `--test-threads=1`.
fn enabled() -> bool {
    std::env::var_os("OBD_DEVICE_TESTS").is_some()
}

fn baselayer() -> PathBuf {
    std::env::var("OVERLAYBD_BASELAYER")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(tools::DEFAULT_BASELAYER))
}

fn work_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from("/var/lib/obd-rs-test").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the work directory");
    dir
}

/// Collects log lines so a test can assert on what was emitted.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<String>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn capture_logs<T>(body: impl FnOnce() -> T) -> (T, String) {
    let captured = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let value = tracing::subscriber::with_default(subscriber, body);
    let logged = captured.0.lock().unwrap().clone();
    (value, logged)
}

/// Build a writable device: fresh sparse upper over the baselayer.
fn writable_device(dir: &Path, name: &str, naa: &str) -> Device {
    let data = dir.join("u.data");
    let index = dir.join("u.index");
    tools::create_sparse_layer(&data, &index, tools::BASELAYER_SIZE_GB).expect("creating a layer");
    let config = DeviceConfig::new(dir.join("result"))
        .lower(Lower::file(baselayer()))
        .upper(&data, &index);
    Device::from_config(name, &config, dir.join("device.json"), naa).expect("building the device")
}

/// The whole happy path through the library, and the thing the CLI cannot
/// cover: an explicit `down()` must not also tear down from `Drop`.
#[test]
fn explicit_down_does_not_warn_or_tear_down_twice() {
    if !enabled() {
        eprintln!("skipping: set OBD_DEVICE_TESTS=1 and run as root");
        return;
    }
    let dir = work_dir("explicit-down");
    let mountpoint = PathBuf::from("/mnt/obd-rs-test-a");

    let ((), logged) = capture_logs(|| {
        let device = writable_device(&dir, "poc_rsdown", "0081");
        let mounted = device
            .up()
            .expect("launching")
            .mount(&mountpoint, Mode::Rw)
            .expect("mounting");

        // Sandboxed handle: confined to the mount.
        let out = mounted.create_subdir("job-out").expect("creating a subdir");
        out.write("marker.txt", b"written through cap-std")
            .expect("writing");
        assert!(out.read("../../etc/hostname").is_err(), "cap-std escaped");
        drop(out);

        mounted
            .unmount()
            .expect("unmounting")
            .down()
            .expect("tearing down");
    });

    // The RAII safety net must stay silent when the caller did the right thing.
    assert!(
        !logged.contains("from Drop"),
        "explicit down() still tore down from Drop:\n{logged}"
    );
    // And teardown must have happened exactly once.
    let teardowns = logged.matches("tore down an overlaybd device").count();
    assert_eq!(
        teardowns, 1,
        "expected one teardown, got {teardowns}:\n{logged}"
    );
}

/// Dropping a live device without calling `down()` must still clean up, and
/// must say so: leftover configfs entries otherwise need manual surgery.
#[test]
fn dropping_a_live_device_tears_it_down_and_warns() {
    if !enabled() {
        eprintln!("skipping: set OBD_DEVICE_TESTS=1 and run as root");
        return;
    }
    let dir = work_dir("implicit-drop");

    let ((), logged) = capture_logs(|| {
        let device = writable_device(&dir, "poc_rsdrop", "0082");
        let live = device.up().expect("launching");
        assert!(live.block_device().exists(), "no block device");
        // Deliberately no down(): the Drop impl has to cover for us.
        drop(live);
    });

    assert!(
        logged.contains("from Drop"),
        "dropping a live device should say the teardown was implicit:\n{logged}"
    );
    assert!(
        !Path::new("/sys/kernel/config/target/core/user_1/poc_rsdrop").exists(),
        "Drop left the backstore behind"
    );
}

/// The committed layer has to be readable by a device that stacks it, and that
/// device must refuse writes.
#[test]
fn committed_layer_restacks_read_only() {
    if !enabled() {
        eprintln!("skipping: set OBD_DEVICE_TESTS=1 and run as root");
        return;
    }
    let dir = work_dir("commit-restack");
    let mountpoint = PathBuf::from("/mnt/obd-rs-test-b");

    let device = writable_device(&dir, "poc_rscommit", "0083");
    let mounted = device
        .up()
        .expect("launching")
        .mount(&mountpoint, Mode::Rw)
        .expect("mounting");
    let out = mounted.create_subdir("job-out").expect("creating a subdir");
    out.write("marker.txt", b"survives the commit")
        .expect("writing");
    drop(out);
    mounted
        .unmount()
        .expect("unmounting")
        .down()
        .expect("tearing down");

    // Commit only once the device is gone: overlaybd-commit opens the data
    // file O_RDWR and would otherwise capture a torn filesystem.
    let commit = dir.join("job.commit");
    let size = tools::commit_layer(&dir.join("u.data"), &dir.join("u.index"), &commit, "test")
        .expect("committing");
    assert!(size > 0);

    let config = DeviceConfig::new(dir.join("result-b"))
        .lowers([Lower::file(baselayer()), Lower::file(&commit)]);
    let device = Device::from_config("poc_rsro", &config, dir.join("device-b.json"), "0084")
        .expect("building the read-only device");
    let mounted = device
        .up()
        .expect("launching")
        .mount(&mountpoint, Mode::Ro)
        .expect("mounting read-only");

    let dir_handle = mounted.dir();
    let job_out = dir_handle.open_dir("job-out").expect("opening job-out");
    assert_eq!(
        job_out.read("marker.txt").expect("reading the marker"),
        b"survives the commit"
    );
    assert!(
        job_out.write("nope.txt", b"x").is_err(),
        "a read-only chain accepted a write"
    );
    drop(job_out);

    mounted
        .unmount()
        .expect("unmounting")
        .down()
        .expect("tearing down");
}
