//! Sandboxed AIS decoder + API server.
//!
//! Runs as PID 1 inside a Firecracker microVM that has:
//!   * no network interface (only virtio-vsock),
//!   * a read-only root filesystem,
//!   * no shell, no libc userland, no other executable.
//!
//! Raw 288 kHz CU8 I/Q arrives from the host over vsock; demodulation, AIS
//! decoding and HTTP all happen in here. See README.md for the full topology.

mod api;
mod boot;
mod listener;

use std::os::fd::OwnedFd;
use std::time::Duration;

use ais_core::{AIS_SAMPLE_RATE, OutlierFilter, extract_position};
use desperado::IqFormat;
use futures::StreamExt;
use rs162::sources::AisAsyncIqSource;
use tokio::io::AsyncWriteExt;
use tokio_vsock::{VsockAddr, VsockStream};

use crate::listener::VsockAxumListener;

/// Well-known context IDs from the AF_VSOCK address family.
const VMADDR_CID_HOST: u32 = 2;
const VMADDR_CID_ANY: u32 = 0xFFFF_FFFF;

const DEFAULT_IQ_PORT: u32 = 5000;
const DEFAULT_HTTP_PORT: u32 = 8000;

fn main() {
    // PID 1 has to bring up its own environment; there is no init system.
    boot::mount_proc();

    let allowed_origins = boot::kernel_param("sdr.origin")
        .unwrap_or_else(|| "https://amsterdam-vessel-map--bcleenders.replit.app".to_string());
    let iq_port = boot::kernel_param("sdr.iq_port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_IQ_PORT);
    let http_port = boot::kernel_param("sdr.http_port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_HTTP_PORT);

    let pipe_tx = boot::redirect_stdin_from_pipe().expect("failed to rewire stdin");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        // One worker for the DSP, one for the HTTP side. The VM is configured
        // with 2 vCPUs to match.
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    runtime.block_on(async move {
        let (state, _tx) = api::AppState::new(allowed_origins.clone());

        tokio::spawn(feed_iq_from_host(iq_port, pipe_tx));
        tokio::spawn(decode_loop(state.clone()));

        eprintln!("guest: serving API on vsock port {http_port}, origins {allowed_origins}");

        let listener = VsockAxumListener::bind(VMADDR_CID_ANY, http_port)
            .expect("failed to bind vsock listener");

        if let Err(e) = axum::serve(listener, api::router(state)).await {
            eprintln!("guest: server exited: {e}");
        }
    });

    // Returning from PID 1 panics the kernel. If the server ever stops, exit
    // deliberately so Firecracker's `panic=1 reboot=k` restarts us cleanly.
    eprintln!("guest: server loop ended, requesting restart");
    std::process::exit(1);
}

/// Pull raw I/Q from the host and push it into the pipe feeding our stdin.
///
/// Reconnects forever: the host-side tap may be restarted independently (for
/// example when the dongle is replugged) and the VM should recover without
/// intervention.
async fn feed_iq_from_host(port: u32, pipe_fd: OwnedFd) {
    let mut pipe = match tokio::net::unix::pipe::Sender::from_owned_fd(pipe_fd) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("guest: cannot open I/Q pipe: {e}");
            return;
        }
    };

    loop {
        match VsockStream::connect(VsockAddr::new(VMADDR_CID_HOST, port)).await {
            Ok(mut stream) => {
                eprintln!("guest: I/Q feed connected to host port {port}");
                let copied = tokio::io::copy(&mut stream, &mut pipe).await;

                // The stream is unframed interleaved CU8: two bytes per complex
                // sample, I then Q. If a connection dies mid-sample we have
                // delivered an orphan I byte, and every subsequent sample would
                // be read with I and Q transposed — which the demodulator does
                // not report as an error, it just silently stops decoding.
                // Emit one padding byte to restore alignment.
                match copied {
                    Ok(n) if n % 2 == 1 => {
                        let _ = pipe.write_all(&[0x80]).await;
                        eprintln!("guest: I/Q feed ended mid-sample, realigned");
                    }
                    Ok(_) => eprintln!("guest: I/Q feed closed by host"),
                    Err(e) => eprintln!("guest: I/Q feed error: {e}"),
                }
            }
            Err(e) => eprintln!("guest: I/Q connect failed: {e}"),
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Demodulate and decode whatever arrives on stdin, publishing valid positions.
async fn decode_loop(state: api::AppState) {
    let mut source = AisAsyncIqSource::from_stdin(AIS_SAMPLE_RATE, IqFormat::Cu8);
    let mut filter = OutlierFilter::new();

    while let Some(item) = source.next().await {
        let message = match item {
            Ok(m) => m,
            Err(e) => {
                eprintln!("guest: demod error: {e}");
                continue;
            }
        };

        let Some(decoded) = message.decode() else {
            continue;
        };

        let Some(pos) = extract_position(
            &decoded,
            message.timestamp,
            message.channel,
            message.signal_level,
        ) else {
            continue;
        };

        if filter.is_valid(&pos) {
            state.publish(pos);
        }
    }

    eprintln!("guest: decode loop ended");
}
