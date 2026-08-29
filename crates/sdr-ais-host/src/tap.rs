//! RTL-SDR → vsock. The only host component that touches the radio.
//!
//! This is intentionally the dumbest program in the repository: it configures
//! the dongle, then copies raw bytes to a socket until the reader goes away. It
//! does not demodulate, does not decode, and never reads from the socket, so
//! there is no input from the guest — or from the air — that it interprets.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use anyhow::{Context, Result};
use rs_rtl::rtlsdr::{DeviceId, RtlSdr};

/// AIS occupies 161.975 MHz (channel A) and 162.025 MHz (channel B); tuning
/// midway lets one capture cover both.
const AIS_CENTER_FREQ: u32 = 162_000_000;

/// See `ais_core::AIS_SAMPLE_RATE` — 288 kHz divides exactly to the
/// demodulator's 96 kHz, so no resampling happens anywhere in the chain.
const AIS_SAMPLE_RATE: u32 = 288_000;

/// Maximum useful gain for AIS, in tenths of a dB as `rs-rtl` expects.
const DEFAULT_GAIN_TENTH_DB: i32 = 496;

pub fn run(
    uds_path: &str,
    port: u32,
    device: usize,
    gain: Option<f64>,
    bias_tee: bool,
) -> Result<()> {
    // Firecracker connects to `<uds_path>_<port>` when the guest dials out to
    // host vsock port `<port>`. That suffixed path is ours to own.
    let socket_path = format!("{uds_path}_{port}");
    let listener = bind_listener(&socket_path)?;

    eprintln!("tap: waiting for guest on {socket_path}");

    // Accept one guest at a time. There is exactly one VM, and serialising
    // means the dongle is only ever streaming when someone is listening.
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("tap: accept failed: {e}");
                continue;
            }
        };

        eprintln!("tap: guest connected");
        if let Err(e) = serve_one(stream, device, gain, bias_tee) {
            eprintln!("tap: session ended: {e:#}");
        } else {
            eprintln!("tap: guest disconnected");
        }
    }

    Ok(())
}

fn bind_listener(socket_path: &str) -> Result<UnixListener> {
    if let Some(parent) = Path::new(socket_path).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // A leftover socket file from an unclean shutdown would make bind fail.
    match std::fs::remove_file(socket_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("removing stale socket"),
    }

    let listener =
        UnixListener::bind(socket_path).with_context(|| format!("binding {socket_path}"))?;

    // Only this user may connect. Firecracker runs as the same user.
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
        .context("restricting socket permissions")?;

    Ok(listener)
}

/// Open the dongle, stream to this client, and release the hardware on exit.
///
/// The device is opened per session rather than once at startup so that
/// replugging the dongle recovers without restarting the tap.
fn serve_one(
    mut stream: UnixStream,
    device: usize,
    gain: Option<f64>,
    bias_tee: bool,
) -> Result<()> {
    let mut sdr = RtlSdr::open(DeviceId::Index(device))
        .with_context(|| format!("opening RTL-SDR index {device}"))?;

    sdr.set_sample_rate(AIS_SAMPLE_RATE)
        .context("setting sample rate")?;
    sdr.set_center_freq(AIS_CENTER_FREQ)
        .context("setting centre frequency")?;

    let gain_tenth_db = gain
        .map(|g| (g * 10.0).round() as i32)
        .unwrap_or(DEFAULT_GAIN_TENTH_DB);
    sdr.set_gain_manual(gain_tenth_db)
        .context("setting manual gain")?;

    if bias_tee {
        sdr.set_bias_t(true).context("enabling bias-tee")?;
    }

    let handle = sdr.start_streaming().context("starting USB streaming")?;
    eprintln!(
        "tap: streaming {AIS_SAMPLE_RATE} Hz CU8 at {:.1} dB gain",
        gain_tenth_db as f64 / 10.0
    );

    let mut last_dropped = 0u64;

    while let Some(chunk) = handle.recv() {
        if stream.write_all(&chunk).is_err() {
            // Guest went away. Normal on VM shutdown.
            break;
        }

        // If the USB thread outruns the socket we lose samples, which shows up
        // downstream as unexplained gaps in decoding. Make it visible.
        let dropped = handle.dropped_chunks();
        if dropped != last_dropped {
            eprintln!("tap: dropped {} USB chunks (total)", dropped);
            last_dropped = dropped;
        }
    }

    handle.stop();
    if bias_tee {
        let _ = sdr.set_bias_t(false);
    }

    Ok(())
}
