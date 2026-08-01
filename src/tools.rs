//! Wrappers around the `overlaybd-*` binaries, plus binary discovery.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use tracing::{debug, error, info, instrument, trace};

use crate::error::{Error, Result};

/// Install locations, in priority order. The upstream GitHub-release package
/// installs into `/opt/overlaybd/bin`; the PMC `containerd-overlaybd` package
/// installs into `/usr/bin/overlaybd` instead, which is why both are searched.
pub const SEARCH_DIRS: &[&str] = &["/opt/overlaybd/bin", "/usr/bin/overlaybd"];

/// The empty 64 GiB ext4 image every device stacks as its bottom lower.
pub const DEFAULT_BASELAYER: &str = "/opt/overlaybd/baselayers/ext4_64";

/// Virtual size of the baselayer, in GiB. A writable layer stacked on top has
/// to advertise the same virtual size.
pub const BASELAYER_SIZE_GB: u32 = 64;

/// Locate an overlaybd binary.
///
/// `OVERLAYBD_BIN_DIR` wins, then the well-known install directories, then
/// `PATH`.
pub fn find(binary: &'static str) -> Result<PathBuf> {
    let mut searched = Vec::new();

    if let Some(dir) = std::env::var_os("OVERLAYBD_BIN_DIR") {
        let candidate = Path::new(&dir).join(binary);
        if candidate.is_file() {
            debug!(
                "found {binary} at {} via OVERLAYBD_BIN_DIR",
                candidate.display()
            );
            return Ok(candidate);
        }
        searched.push(candidate);
    }

    for dir in SEARCH_DIRS {
        let candidate = Path::new(dir).join(binary);
        if candidate.is_file() {
            debug!("found {binary} at {}", candidate.display());
            return Ok(candidate);
        }
        searched.push(candidate);
    }

    if let Ok(path) = which_on_path(binary) {
        debug!("found {binary} on PATH at {}", path.display());
        return Ok(path);
    }

    debug!(
        "{binary} is not in any known install directory: {}",
        searched
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Err(Error::ToolMissing {
        name: binary,
        searched,
    })
}

/// Minimal `PATH` lookup. `which` is a build-dependency only, so this keeps it
/// out of the runtime dependency graph.
fn which_on_path(binary: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(Error::ToolMissing {
        name: "",
        searched: vec![],
    })
}

/// Run a command, capturing combined output, failing on a non-zero exit.
pub(crate) fn run(command: &mut Command) -> Result<String> {
    let rendered = format!("{command:?}");
    debug!("running {rendered}");

    let started = Instant::now();
    let output = command.output().map_err(|source| {
        // Spawn failed, so the binary never ran. Reasons, cheapest first:
        //   1. The file was found by `find` but is not executable, or lost its
        //      +x bit: EACCES.
        //   2. It is a dangling symlink - the install script links
        //      /opt/overlaybd/bin -> /usr/bin/overlaybd, so an uninstall of the
        //      package leaves the links behind: ENOENT.
        //   3. The binary is for a different architecture: ENOEXEC.
        //   4. The process is out of memory or hit RLIMIT_NPROC.
        error!("could not spawn {rendered}: {source}");
        Error::io(format!("could not run {rendered}"), source)
    })?;
    trace!(
        duration_ms = started.elapsed().as_millis() as u64,
        "tool-exec"
    );

    if !output.status.success() {
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        let combined = combined.trim().to_string();
        let code = output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string());
        // The binary ran and rejected the work. Reasons, cheapest to rule out
        // first, with the tool's own output in `combined`:
        //   1. An output path already exists. Both tools open their outputs
        //      O_EXCL, so a rerun over a previous run's directory fails here.
        //   2. An input path is missing or unreadable, usually the data or
        //      index of a layer that was never created.
        //   3. The data file is still open by a live device, so a commit reads
        //      a filesystem that is being written underneath it.
        //   4. The filesystem holding the output is full, or the layer exceeds
        //      a size limit the tool enforces.
        error!(command = %rendered, code, "overlaybd tool failed: {combined}");
        return Err(Error::ToolFailed {
            command: rendered,
            code,
            output: combined,
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Create a **sparse** writable (upper) layer.
///
/// `-s` selects the sparse-file layer. The default log-structured layer is
/// append-only and tuned for image conversion, not for a runtime workload that
/// creates and deletes files.
///
/// Both paths must not already exist: the tool opens its outputs with
/// `O_RDWR | O_EXCL | O_CREAT` (overlaybd `src/tools/overlaybd-create.cpp`).
/// Checking up front turns a terse tool error into a clear one.
#[instrument(level = "debug", skip_all, fields(data = %data.display(), vsize_gb))]
pub fn create_sparse_layer(data: &Path, index: &Path, vsize_gb: u32) -> Result<()> {
    for path in [data, index] {
        if path.exists() {
            debug!(
                "refusing to create a layer over the existing file {}",
                path.display()
            );
            return Err(Error::LayerExists {
                path: path.to_path_buf(),
            });
        }
    }
    if let Some(parent) = data.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::io(format!("creating {}", parent.display()), e))?;
    }

    let binary = find("overlaybd-create")?;
    run(Command::new(binary)
        .arg("-s")
        .arg(data)
        .arg(index)
        .arg(vsize_gb.to_string()))?;

    info!(
        data = %data.display(),
        index = %index.display(),
        vsize_gb,
        "created a sparse writable overlaybd layer"
    );
    Ok(())
}

/// Turn a writable layer into a read-only overlaybd layer.
///
/// The device must be **torn down** first. `overlaybd-commit` opens the data
/// file `O_RDWR` (overlaybd `src/tools/overlaybd-commit.cpp`) and does not
/// check whether the daemon still has it open, so committing a live device
/// captures a torn filesystem rather than failing.
#[instrument(level = "debug", skip_all, fields(out = %out.display()))]
pub fn commit_layer(data: &Path, index: &Path, out: &Path, message: &str) -> Result<u64> {
    if out.exists() {
        debug!(
            "refusing to commit over the existing file {}",
            out.display()
        );
        return Err(Error::LayerExists {
            path: out.to_path_buf(),
        });
    }
    let binary = find("overlaybd-commit")?;
    let started = Instant::now();
    run(Command::new(binary)
        .arg("-m")
        .arg(message)
        .arg(data)
        .arg(index)
        .arg(out))?;
    trace!(
        duration_ms = started.elapsed().as_millis() as u64,
        "layer-commit"
    );

    let size = std::fs::metadata(out)
        .map_err(|e| Error::io(format!("stat {}", out.display()), e))?
        .len();

    info!(
        layer = %out.display(),
        size_bytes = size,
        message,
        "committed a read-only overlaybd layer"
    );
    Ok(size)
}
