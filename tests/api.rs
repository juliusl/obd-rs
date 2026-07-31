//! Tests that run anywhere, including macOS: config rendering, validation and
//! cap-std confinement. The device lifecycle itself needs Linux and is
//! exercised by `tests/lima-e2e.sh` against a real overlaybd install.

use obd::{Device, DeviceConfig, Lower};
use serde_json::{Value, json};

fn parse(config: &DeviceConfig) -> Value {
    serde_json::from_str(&config.to_json().expect("renders")).expect("valid json")
}

/// The shape `overlaybd_device.py::write_device_config` produced for a
/// writable device: one local lower plus an upper, and no repoBlobUrl.
#[test]
fn writable_device_config_matches_python() {
    let config = DeviceConfig::new("/var/lib/poc/result-a")
        .lower(Lower::file("/opt/overlaybd/baselayers/ext4_64"))
        .upper("/var/lib/poc/upper.data", "/var/lib/poc/upper.index");

    assert_eq!(
        parse(&config),
        json!({
            "lowers": [{ "file": "/opt/overlaybd/baselayers/ext4_64" }],
            "upper": {
                "index": "/var/lib/poc/upper.index",
                "data": "/var/lib/poc/upper.data"
            },
            "resultFile": "/var/lib/poc/result-a"
        })
    );
}

/// A read-only device stacking a committed layer, as job 2 uses.
#[test]
fn readonly_device_config_has_no_upper() {
    let config = DeviceConfig::new("/var/lib/poc/result-b").lowers([
        Lower::file("/opt/overlaybd/baselayers/ext4_64"),
        Lower::file("/var/lib/poc/job1-layer.commit"),
    ]);

    assert_eq!(
        parse(&config),
        json!({
            "lowers": [
                { "file": "/opt/overlaybd/baselayers/ext4_64" },
                { "file": "/var/lib/poc/job1-layer.commit" }
            ],
            "resultFile": "/var/lib/poc/result-b"
        })
    );
}

/// A streamed lower: digest and size, with repoBlobUrl at the top level.
#[test]
fn remote_lower_renders_digest_and_size() {
    let config = DeviceConfig::new("/var/lib/poc/result-b")
        .lower(Lower::file("/opt/overlaybd/baselayers/ext4_64"))
        .lower(Lower::remote("sha256:04f7bec5", 167936).expect("valid digest"))
        .repo_blob_url("https://reg.azurecr.io/v2/repo/blobs/");

    assert_eq!(
        parse(&config),
        json!({
            "repoBlobUrl": "https://reg.azurecr.io/v2/repo/blobs",
            "lowers": [
                { "file": "/opt/overlaybd/baselayers/ext4_64" },
                { "digest": "sha256:04f7bec5", "size": 167936 }
            ],
            "resultFile": "/var/lib/poc/result-b"
        })
    );
}

#[test]
fn remote_lower_can_cache_to_a_directory() {
    let config = DeviceConfig::new("/r")
        .lower(
            Lower::remote("sha256:abc", 10)
                .unwrap()
                .with_cache_dir("/cache"),
        )
        .repo_blob_url("https://example/v2/x/blobs");
    assert_eq!(parse(&config)["lowers"][0]["dir"], json!("/cache"));
}

/// overlaybd refuses a remote lower with an empty repoBlobUrl, so the config is
/// rejected before it can reach the daemon.
#[test]
fn remote_lower_without_repo_blob_url_is_rejected() {
    let config = DeviceConfig::new("/r").lower(Lower::remote("sha256:abc", 10).unwrap());
    assert!(matches!(
        config.to_json(),
        Err(obd::Error::MissingRepoBlobUrl)
    ));
}

#[test]
fn digest_must_be_sha256() {
    assert!(matches!(
        Lower::remote("md5:abc", 1),
        Err(obd::Error::BadDigest { .. })
    ));
    assert!(Lower::remote("sha256:abc", 1).is_ok());
}

/// The cleanup sweep finds devices by name, so the prefix is mandatory.
#[test]
fn device_name_must_carry_the_sweep_prefix() {
    assert!(matches!(
        Device::new("wrong", "/c.json", "/r", "0021"),
        Err(obd::Error::BadDeviceName { .. })
    ));
    let device = Device::new("poc_a", "/c.json", "/r", "0021").expect("accepted");
    assert_eq!(device.name(), "poc_a");
    assert_eq!(device.naa(), "naa.5001405e0b0d0021");
}

#[test]
fn config_writes_to_disk_with_a_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/device.json");
    DeviceConfig::new("/r")
        .lower(Lower::file("/base"))
        .write(&path)
        .expect("writes, creating parents");

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("}\n"), "config should end with a newline");
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["lowers"][0]["file"],
        json!("/base")
    );
}

/// The sandboxing guarantee the public API leans on: a handle to the mount
/// cannot be used to reach outside it.
#[test]
fn cap_std_dir_confines_to_its_root() {
    use cap_std::ambient_authority;
    use cap_std::fs::Dir;

    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("job-out")).unwrap();
    let root = Dir::open_ambient_dir(tmp.path(), ambient_authority()).unwrap();
    let job_out = root.open_dir("job-out").unwrap();

    job_out.write("result.json", b"{}").unwrap();
    assert_eq!(job_out.read("result.json").unwrap(), b"{}");

    // Escapes must fail rather than silently resolve.
    assert!(job_out.read("../../etc/hosts").is_err(), "`..` escaped");
    assert!(job_out.read("/etc/hosts").is_err(), "absolute path escaped");
}

/// Off Linux the types still exist, so the crate can be developed anywhere, but
/// the device operations refuse to run rather than doing something wrong.
#[cfg(not(target_os = "linux"))]
#[test]
fn device_operations_refuse_to_run_off_linux() {
    let device = Device::new("poc_a", "/c.json", "/r", "0021").unwrap();
    assert!(matches!(
        device.up(),
        Err(obd::Error::UnsupportedPlatform { .. })
    ));
}

/// Preflight is honest about the platform it is running on.
#[test]
fn preflight_reports_the_platform() {
    let checks = obd::preflight();
    let linux = checks
        .iter()
        .find(|c| c.name == "linux host")
        .expect("checked");
    assert_eq!(linux.ok, cfg!(target_os = "linux"));
}
