//! `obdctl` - command-line access to the overlaybd device lifecycle.
//!
//! This is the CLI form of the `obd` library, intended to replace what
//! `overlaybd_device.py` did for the PoC scripts.
//!
//! Commands are **stateless**: a device is identified by its name and nexus
//! suffix, both of which the caller already chose, so `device up` in one
//! process and `device down` in another need no state file. `device up`
//! deliberately leaves the device running - see `--json` output for what a
//! later teardown needs.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use obd::{Device, DeviceConfig, Lower, Mode, Result, tools};

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
    /// Base URL for streamed lowers, e.g. https://<registry>/v2/<repo>/blobs
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("obdctl: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Preflight => preflight(cli.json),
        Command::Layer(cmd) => layer(cmd, cli.json),
        Command::Config(args) => config(args, cli.json),
        Command::Device(cmd) => device(cmd, cli.json),
        Command::Cleanup(args) => cleanup(args, cli.json),
    }
}

fn preflight(json: bool) -> Result<()> {
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

    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn layer(cmd: &LayerCommand, json: bool) -> Result<()> {
    match cmd {
        LayerCommand::Create {
            data,
            index,
            size_gb,
        } => {
            tools::create_sparse_layer(data, index, *size_gb)?;
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
            let size = tools::commit_layer(data, index, out, message)?;
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

fn config(args: &ConfigArgs, json: bool) -> Result<()> {
    let mut config = DeviceConfig::new(&args.result_file);
    for path in &args.lowers {
        config = config.lower(Lower::file(path));
    }
    for spec in &args.remote_lowers {
        let (digest, size) = spec.split_once('=').ok_or_else(|| obd::Error::BadDigest {
            digest: format!("{spec} (expected DIGEST=SIZE)"),
        })?;
        let size: u64 = size.parse().map_err(|_| obd::Error::BadDigest {
            digest: format!("{spec} (size must be a number)"),
        })?;
        config = config.lower(Lower::remote(digest, size)?);
    }
    if let Some(url) = &args.repo_blob_url {
        config = config.repo_blob_url(url);
    }
    if let (Some(data), Some(index)) = (&args.upper_data, &args.upper_index) {
        config = config.upper(data, index);
    }

    config.write(&args.out)?;
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

fn device(cmd: &DeviceCommand, json: bool) -> Result<()> {
    match cmd {
        DeviceCommand::Up(args) => {
            let device = Device::new(
                &args.name,
                &args.config,
                &args.result_file,
                &args.naa_suffix,
            )?;
            let live = device.up()?;

            let persisted = match &args.mount {
                None => live.persist(),
                Some(mountpoint) => {
                    let mode = if args.read_only { Mode::Ro } else { Mode::Rw };
                    let mounted = live.mount(mountpoint, mode)?;
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
            obd::device::teardown_named(&args.name, &args.naa_suffix)?;

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

fn cleanup(args: &CleanupArgs, json: bool) -> Result<()> {
    let swept = obd::force_cleanup(&args.mounts)?;
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
