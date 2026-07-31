//! Build-time probe for the overlaybd toolchain.
//!
//! This only ever *warns*. The build host and the run host are not necessarily
//! the same machine - this crate is routinely developed on macOS and run on
//! Linux - so a missing binary here says nothing about whether the target can
//! run it. Failing the build would make `cargo check`, `clippy` and
//! rust-analyzer unusable on any developer machine without overlaybd
//! installed.
//!
//! The authoritative check is `obd::preflight()` at runtime, which is what
//! `obdctl preflight` calls.

use std::path::Path;

/// Binaries the library shells out to. Kept in sync with `src/tools.rs`.
const REQUIRED: &[&str] = &["overlaybd-create", "overlaybd-commit"];

/// Where the PMC package and the upstream release package put things.
const SEARCH_DIRS: &[&str] = &["/opt/overlaybd/bin", "/usr/bin/overlaybd"];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=OVERLAYBD_BIN_DIR");

    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some("linux".as_ref()) {
        println!(
            "cargo::warning=obd-rs targets Linux (configfs + TCMU); this build is for a \
             non-Linux target, so device operations will return UnsupportedPlatform"
        );
        return;
    }

    let mut missing = Vec::new();
    for binary in REQUIRED {
        if locate(binary).is_none() {
            missing.push(*binary);
        }
    }

    if missing.is_empty() {
        return;
    }

    println!(
        "cargo::warning=overlaybd binaries not found ({}). This is only a warning: \
         the build host need not be the run host.",
        missing.join(", ")
    );
    println!(
        "cargo::warning=to install on the run host: sudo ./scripts/install-overlaybd.sh \
         (adds the packages.microsoft.com repo, installs containerd-overlaybd, and wires \
         /opt/overlaybd). Then verify with: obdctl preflight"
    );
}

/// Look in the usual install locations first, then fall back to `PATH`.
fn locate(binary: &str) -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("OVERLAYBD_BIN_DIR") {
        let candidate = Path::new(&dir).join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in SEARCH_DIRS {
        let candidate = Path::new(dir).join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    which::which(binary).ok()
}
