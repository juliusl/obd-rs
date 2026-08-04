//! `obdctl` - command-line access to the overlaybd device lifecycle.
//!
//! This is the CLI form of the `obd` library: the whole device lifecycle,
//! reachable from a shell script or a job runner rather than from Rust.
//!
//! Commands are **stateless**: a device is identified by its name and nexus
//! suffix, both of which the caller already chose, so `device up` in one
//! process and `device down` in another need no state file. `device up`
//! deliberately leaves the device running - see `--json` output for what a
//! later teardown needs.
//!
//! # Diagnostics
//!
//! The library emits [`tracing`] events but installs no subscriber, so the
//! choice of renderer belongs here. This binary installs
//! [`color_eyre`] as the report handler and a `tracing-subscriber` fmt layer,
//! plus a [`tracing_error::ErrorLayer`] so a failure report carries the span
//! trace - which device, which operation - alongside the error chain.
//!
//! Verbosity: `-v` for debug, `-vv` for trace, or set `RUST_LOG` for full
//! control (`RUST_LOG=obd::configfs=trace` isolates the polling loops).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use color_eyre::eyre::WrapErr;
use obd::{Device, DeviceConfig, Lower, Mode, tools};
use tracing::instrument;
use tracing_error::ErrorLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

#[derive(Parser)]
#[command(
    name = "obdctl",
    about = "overlaybd device lifecycle: layers, device config, mount, teardown",
    long_about = None,
    version
)]
struct Cli {
    /// Emit machine-readable JSON on stdout.
    #[arg(long, global = true)]
    json: bool,

    /// Increase log verbosity: -v for debug, -vv for trace.
    ///
    /// RUST_LOG takes precedence when set.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Log timestamps and targets, for capturing output to a file.
    #[arg(long, global = true)]
    log_full: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that this host can drive overlaybd devices.
    Preflight,
    /// Create and commit overlaybd layers.
    #[command(subcommand)]
    Layer(LayerCommand),
    /// Write a device config JSON.
    Config(ConfigArgs),
    /// Launch, mount and tear down devices.
    #[command(subcommand)]
    Device(DeviceCommand),
    /// Unmount and remove leftover devices from an interrupted run.
    Cleanup(CleanupArgs),
}

#[derive(Subcommand)]
enum LayerCommand {
    /// Create a sparse writable (upper) layer.
    Create {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        index: PathBuf,
        /// Virtual size in GiB. Must match the baselayer.
        #[arg(long, default_value_t = tools::BASELAYER_SIZE_GB)]
        size_gb: u32,
    },
    /// Commit a writable layer into a read-only layer.
    ///
    /// The device must be torn down first: overlaybd-commit opens the data file
    /// O_RDWR and committing a live device captures a torn filesystem.
    Commit {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        index: PathBuf,
        /// Output layer file. Must not already exist.
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "obdctl commit")]
        message: String,
    },
}

#[derive(Args)]
struct ConfigArgs {
    /// Where to write the config JSON.
    #[arg(long)]
    out: PathBuf,
    /// Where overlaybd reports launch success or failure.
    #[arg(long)]
    result_file: PathBuf,
    /// A local lower layer file. Repeat for more; order is bottom-up.
    #[arg(long = "lower", value_name = "PATH")]
    lowers: Vec<PathBuf>,
    /// A streamed lower, as `sha256:...=SIZE`. Repeat for more.
    #[arg(long = "remote-lower", value_name = "DIGEST=SIZE")]
    remote_lowers: Vec<String>,
    /// Where overlaybd may persist the blocks it fetches for a streamed lower.
    ///
    /// Pairs positionally with `--remote-lower`: give it once per streamed
    /// lower, or not at all. An empty value leaves that one purely streamed.
    #[arg(long = "remote-lower-dir", value_name = "PATH")]
    remote_lower_dirs: Vec<PathBuf>,
    /// Base URL for streamed lowers, e.g. `https://REGISTRY/v2/REPO/blobs`
    #[arg(long)]
    repo_blob_url: Option<String>,
    /// Sparse writable layer data file.
    #[arg(long, requires = "upper_index")]
    upper_data: Option<PathBuf>,
    /// Sparse writable layer index file.
    #[arg(long, requires = "upper_data")]
    upper_index: Option<PathBuf>,
}

#[derive(Subcommand)]
enum DeviceCommand {
    /// Launch a device and optionally mount it, then leave it running.
    Up(UpArgs),
    /// Unmount and tear a device down.
    Down(DownArgs),
}

#[derive(Args)]
struct UpArgs {
    /// Backstore name. Must start with `poc_`.
    #[arg(long)]
    name: String,
    /// Device config JSON, as written by `obdctl config`.
    #[arg(long)]
    config: PathBuf,
    /// resultFile from that config.
    #[arg(long)]
    result_file: PathBuf,
    /// Nexus suffix; must be unique among live devices.
    #[arg(long)]
    naa_suffix: String,
    /// Mount here after launching.
    #[arg(long)]
    mount: Option<PathBuf>,
    /// Mount read-only (`ro,noload`). Required when the device has no upper.
    #[arg(long)]
    read_only: bool,
    /// Create this subdirectory under the mount and report its path.
    #[arg(long, requires = "mount")]
    subdir: Option<String>,
}

#[derive(Args)]
struct DownArgs {
    #[arg(long)]
    name: String,
    #[arg(long)]
    naa_suffix: String,
    /// Unmount this path first.
    #[arg(long)]
    mount: Option<PathBuf>,
}

#[derive(Args)]
struct CleanupArgs {
    /// Unmount these before sweeping configfs. Repeat for more.
    #[arg(long = "mount", value_name = "PATH")]
    mounts: Vec<PathBuf>,
}

fn main() -> color_eyre::Result<ExitCode> {
    let cli = Cli::parse();
    init_diagnostics(&cli)?;

    // `run` returns the exit code rather than an error for a soft failure, so
    // a preflight that merely found problems does not print an error report as
    // though the tool had crashed.
    run(&cli)
}

/// Install the error report handler and the log subscriber.
///
/// Logs go to stderr so `--json` output on stdout stays machine-readable.
fn init_diagnostics(cli: &Cli) -> color_eyre::Result<()> {
    color_eyre::install()?;

    // RUST_LOG wins; otherwise -v/-vv pick a sensible default. The library is
    // quiet at info, which is the audit trail.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(match cli.verbose {
            0 => "obd=info,obdctl=info",
            1 => "obd=debug,obdctl=debug",
            _ => "obd=trace,obdctl=trace",
        })
    });

    let layer = fmt::layer().with_writer(std::io::stderr);
    let layer = if cli.log_full {
        layer.with_target(true).boxed()
    } else {
        // Interactive default: the message and its fields, nothing else.
        layer
            .without_time()
            .with_target(false)
            .with_level(true)
            .boxed()
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        // Captures the span stack so color-eyre can show it in a report.
        .with(ErrorLayer::default())
        .init();

    Ok(())
}

fn run(cli: &Cli) -> color_eyre::Result<ExitCode> {
    match &cli.command {
        Command::Preflight => preflight(cli.json),
        Command::Layer(cmd) => layer(cmd, cli.json).map(|()| ExitCode::SUCCESS),
        Command::Config(args) => config(args, cli.json).map(|()| ExitCode::SUCCESS),
        Command::Device(cmd) => device(cmd, cli.json).map(|()| ExitCode::SUCCESS),
        Command::Cleanup(args) => cleanup(args, cli.json).map(|()| ExitCode::SUCCESS),
    }
}

fn preflight(json: bool) -> color_eyre::Result<ExitCode> {
    let checks = obd::preflight();
    let failed = checks.iter().filter(|c| !c.ok).count();

    if json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        for check in &checks {
            if check.ok {
                println!("  [ok]   {}", check.name);
            } else {
                println!("  [FAIL] {}", check.name);
                if let Some(hint) = &check.hint {
                    println!("         -> {hint}");
                }
            }
        }
        println!();
        if failed == 0 {
            println!("preflight: PASS");
        } else {
            println!("preflight: FAIL ({failed} blocking)");
        }
    }

    // A failed preflight is a finding, not a crash: report it through the exit
    // code rather than an error report with a backtrace.
    Ok(if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

#[instrument(level = "info", skip_all, name = "obdctl::layer")]
fn layer(cmd: &LayerCommand, json: bool) -> color_eyre::Result<()> {
    match cmd {
        LayerCommand::Create {
            data,
            index,
            size_gb,
        } => {
            tools::create_sparse_layer(data, index, *size_gb).wrap_err_with(|| {
                format!(
                    "creating a {size_gb}G sparse writable layer at {}",
                    data.display()
                )
            })?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "data": data, "index": index, "virtual_size_gb": size_gb
                    })
                );
            } else {
                println!(
                    "created sparse writable layer ({size_gb}G virtual): {} / {}",
                    data.display(),
                    index.display()
                );
            }
        }
        LayerCommand::Commit {
            data,
            index,
            out,
            message,
        } => {
            let size = tools::commit_layer(data, index, out, message).wrap_err_with(|| {
                format!(
                    "committing {} into the read-only layer {}",
                    data.display(),
                    out.display()
                )
            })?;
            if json {
                println!("{}", serde_json::json!({ "layer": out, "size": size }));
            } else {
                println!(
                    "committed read-only layer: {} ({size} bytes)",
                    out.display()
                );
            }
        }
    }
    Ok(())
}

/// Build the streamed lowers from `--remote-lower` and `--remote-lower-dir`.
///
/// The dirs pair positionally with the specs, so the rule is all or nothing:
/// anything else silently attaches a cache directory to the wrong layer.
fn remote_lowers(specs: &[String], dirs: &[PathBuf]) -> color_eyre::Result<Vec<Lower>> {
    if !dirs.is_empty() && dirs.len() != specs.len() {
        return Err(color_eyre::eyre::eyre!(
            "--remote-lower-dir must be given once per --remote-lower, or not at all \
             ({} dir(s) for {} streamed lower(s)); pass an empty value to leave one \
             purely streamed",
            dirs.len(),
            specs.len()
        ));
    }

    let mut lowers = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        let (digest, size) = spec
            .split_once('=')
            .ok_or_else(|| color_eyre::eyre::eyre!("--remote-lower must be DIGEST=SIZE: {spec}"))?;
        let size: u64 = size
            .parse()
            .wrap_err_with(|| format!("--remote-lower size must be a byte count: {spec}"))?;
        let mut lower = Lower::remote(digest, size)?;
        // An absent or empty dir keeps that layer purely streamed, which is
        // what overlaybd does when the key is missing.
        if let Some(dir) = dirs.get(index).filter(|dir| !dir.as_os_str().is_empty()) {
            lower = lower.with_cache_dir(dir);
        }
        lowers.push(lower);
    }
    Ok(lowers)
}

#[instrument(level = "info", skip_all, name = "obdctl::config")]
fn config(args: &ConfigArgs, json: bool) -> color_eyre::Result<()> {
    let mut config = DeviceConfig::new(&args.result_file);
    for path in &args.lowers {
        config = config.lower(Lower::file(path));
    }
    for lower in remote_lowers(&args.remote_lowers, &args.remote_lower_dirs)? {
        config = config.lower(lower);
    }
    if let Some(url) = &args.repo_blob_url {
        config = config.repo_blob_url(url);
    }
    if let (Some(data), Some(index)) = (&args.upper_data, &args.upper_index) {
        config = config.upper(data, index);
    }

    config
        .write(&args.out)
        .wrap_err_with(|| format!("writing the device config {}", args.out.display()))?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "config": args.out, "remote_lowers": !args.remote_lowers.is_empty() })
        );
    } else {
        println!("wrote device config {}", args.out.display());
    }
    Ok(())
}

#[instrument(level = "info", skip_all, name = "obdctl::device")]
fn device(cmd: &DeviceCommand, json: bool) -> color_eyre::Result<()> {
    match cmd {
        DeviceCommand::Up(args) => {
            let device = Device::new(
                &args.name,
                &args.config,
                &args.result_file,
                &args.naa_suffix,
            )?;
            let live = device.up().wrap_err_with(|| {
                format!(
                    "launching device {} from {}",
                    args.name,
                    args.config.display()
                )
            })?;

            let persisted = match &args.mount {
                None => live.persist(),
                Some(mountpoint) => {
                    let mode = if args.read_only { Mode::Ro } else { Mode::Rw };
                    let mounted = live.mount(mountpoint, mode).wrap_err_with(|| {
                        format!("mounting device {} at {}", args.name, mountpoint.display())
                    })?;
                    if let Some(subdir) = &args.subdir {
                        // Created through the sandboxed handle, then dropped so
                        // it cannot hold the mount busy after we exit.
                        drop(mounted.create_subdir(subdir)?);
                    }
                    // Deliberately leave it up: a later process does the work,
                    // and `obdctl device down` tears it back down.
                    mounted.persist()
                }
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&persisted)?);
            } else {
                println!(
                    "device {} is {}",
                    persisted.name,
                    persisted.block_device.display()
                );
                if let Some(mountpoint) = &persisted.mountpoint {
                    println!("mounted at {}", mountpoint.display());
                    if let Some(subdir) = &args.subdir {
                        println!("subdir {}", mountpoint.join(subdir).display());
                    }
                }
            }
        }
        DeviceCommand::Down(args) => {
            // Mount first: a mounted device cannot be torn down.
            let unmounted = args
                .mount
                .as_ref()
                .map(|m| obd::cleanup::unmount_path(m))
                .unwrap_or(false);
            // Targeted teardown, not a sweep: only this device goes away.
            obd::device::teardown_named(&args.name, &args.naa_suffix)
                .wrap_err_with(|| format!("tearing down device {}", args.name))?;

            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "name": args.name,
                        "naa_suffix": args.naa_suffix,
                        "unmounted": unmounted,
                    })
                );
            } else {
                if let (true, Some(mountpoint)) = (unmounted, &args.mount) {
                    println!("unmounted {}", mountpoint.display());
                }
                println!("device {} torn down", args.name);
            }
        }
    }
    Ok(())
}

#[instrument(level = "info", skip_all, name = "obdctl::cleanup")]
fn cleanup(args: &CleanupArgs, json: bool) -> color_eyre::Result<()> {
    let swept = obd::force_cleanup(&args.mounts).wrap_err("sweeping leftover overlaybd devices")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&swept)?);
    } else {
        report_swept(&swept);
    }
    Ok(())
}

fn report_swept(swept: &obd::Swept) {
    for path in &swept.unmounted {
        println!("unmounted {}", path.display());
    }
    for name in &swept.backstores {
        println!("removed backstore {name}");
    }
    for naa in &swept.nexuses {
        println!("removed nexus {naa}");
    }
    if swept.is_empty() {
        println!("nothing to clean up");
    }
}

#[cfg(test)]
mod tests {
    use super::remote_lowers;
    use obd::DeviceConfig;
    use std::path::PathBuf;

    fn specs(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    fn dirs(values: &[&str]) -> Vec<PathBuf> {
        values.iter().map(PathBuf::from).collect()
    }

    /// Render the lowers the way the daemon will read them.
    fn rendered(specs: &[String], dirs: &[PathBuf]) -> serde_json::Value {
        let mut config = DeviceConfig::new("/result").repo_blob_url("https://example/v2/x/blobs");
        for lower in remote_lowers(specs, dirs).expect("builds") {
            config = config.lower(lower);
        }
        serde_json::from_str(&config.to_json().expect("renders")).expect("valid json")
    }

    /// The dir has to land on the layer it was typed next to, so this pins the
    /// pairing rather than the count.
    #[test]
    fn a_cache_dir_pairs_with_the_lower_at_the_same_position() {
        let config = rendered(
            &specs(&["sha256:aa=10", "sha256:bb=20"]),
            &dirs(&["", "/cache/bb"]),
        );
        assert!(config["lowers"][0].get("dir").is_none(), "{config}");
        assert_eq!(config["lowers"][1]["dir"], serde_json::json!("/cache/bb"));
    }

    #[test]
    fn no_dirs_leaves_every_streamed_lower_alone() {
        let config = rendered(&specs(&["sha256:aa=10"]), &[]);
        assert!(config["lowers"][0].get("dir").is_none(), "{config}");
        assert_eq!(
            config["lowers"][0]["digest"],
            serde_json::json!("sha256:aa")
        );
    }

    #[test]
    fn a_partial_list_of_dirs_is_refused() {
        let err = remote_lowers(
            &specs(&["sha256:aa=10", "sha256:bb=20"]),
            &dirs(&["/cache/only-one"]),
        )
        .expect_err("ambiguous pairing must not be guessed");
        assert!(err.to_string().contains("once per --remote-lower"), "{err}");
    }

    #[test]
    fn a_spec_without_a_size_is_refused() {
        let err = remote_lowers(&specs(&["sha256:aa"]), &[]).expect_err("needs DIGEST=SIZE");
        assert!(err.to_string().contains("DIGEST=SIZE"), "{err}");
    }
}
