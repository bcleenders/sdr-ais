//! Host-side plumbing for the sandboxed AIS API.
//!
//! Two jobs, run as two separate processes so that neither has the other's
//! privileges:
//!
//!   `tap`   — owns the RTL-SDR. Copies raw CU8 I/Q into the guest.
//!             Write-only: it never reads a byte the guest sends.
//!   `proxy` — owns no hardware. Bridges loopback TCP (which Tailscale Funnel
//!             serves) to the guest's vsock HTTP listener.
//!
//! Neither parses anything. All interpretation of radio data and of internet
//! requests happens inside the microVM.

mod log;
mod proxy;
mod tap;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "sdr-ais-host", about = "Host-side bridge for the sandboxed AIS API")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stream raw I/Q from the RTL-SDR into the guest over vsock.
    Tap {
        /// Firecracker vsock UDS path, without the port suffix. The guest dials
        /// out to host port `--port`, so Firecracker connects to
        /// `<uds-path>_<port>`, which is what this command listens on.
        #[arg(long)]
        uds_path: String,

        /// Host vsock port the guest dials out to.
        #[arg(long, default_value_t = 5000)]
        port: u32,

        /// RTL-SDR device index.
        #[arg(long, default_value_t = 0)]
        device: usize,

        /// Manual gain in dB. Defaults to 49.6, the maximum useful for AIS.
        #[arg(long)]
        gain: Option<f64>,

        /// Enable bias-tee power for an active antenna.
        #[arg(long)]
        bias_tee: bool,
    },

    /// Log all decoded positions from the guest to a SQLite database.
    Log {
        /// Firecracker vsock UDS path (the base path, no port suffix).
        #[arg(long)]
        uds_path: String,

        /// Guest vsock port the position log stream listens on.
        #[arg(long, default_value_t = 5001)]
        guest_port: u32,

        /// Path to the SQLite database file.
        #[arg(long, default_value = "positions.db")]
        db: String,
    },

    /// Bridge loopback TCP to the guest's vsock HTTP listener.
    Proxy {
        /// Firecracker vsock UDS path (the base path, no port suffix).
        #[arg(long)]
        uds_path: String,

        /// Guest vsock port the API listens on.
        #[arg(long, default_value_t = 8000)]
        guest_port: u32,

        /// Loopback address to listen on. Must stay on localhost: Tailscale
        /// Funnel connects from the same host, and binding wider would expose
        /// the API on the LAN without TLS.
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: String,
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Log {
            uds_path,
            guest_port,
            db,
        } => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(log::run(&uds_path, guest_port, &db))
        }

        Command::Tap {
            uds_path,
            port,
            device,
            gain,
            bias_tee,
        } => tap::run(&uds_path, port, device, gain, bias_tee),

        Command::Proxy {
            uds_path,
            guest_port,
            listen,
        } => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(proxy::run(&uds_path, guest_port, &listen))
        }
    }
}
