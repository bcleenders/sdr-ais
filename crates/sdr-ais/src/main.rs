//! Local development tool: RTL-SDR straight to a Leaflet map on localhost.
//!
//! No sandbox and no vsock — this binds the dongle directly and serves the web
//! UI. For the internet-facing deployment see `sdr-ais-host` (which owns the
//! dongle) and `sdr-ais-guest` (which decodes and serves inside a microVM).

use std::sync::atomic::{AtomicU64, Ordering};

use ais_core::{AIS_SAMPLE_RATE, OutlierFilter, extract_position, format_human};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::response::Html;
use axum::routing::get;
use clap::Parser;
use desperado::rtlsdr::DeviceSelector;
use rs162::sources::AisIqSource;
use tokio::sync::broadcast;

static COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Parser)]
#[command(name = "sdr-ais", about = "Log AIS ship positions from an RTL-SDR")]
struct Args {
    /// Output one JSON object per line instead of human-readable text
    #[arg(long)]
    json: bool,

    /// RTL-SDR device index
    #[arg(long, default_value_t = 0)]
    device: usize,

    /// Manual gain in dB (default: 49.6, max for AIS at 162 MHz)
    #[arg(long)]
    gain: Option<f64>,

    /// Enable bias-tee power (for active antennas)
    #[arg(long)]
    bias_tee: bool,

    /// Web UI port
    #[arg(long, default_value_t = 3000)]
    port: u16,
}

#[derive(Clone)]
struct AppState {
    tx: broadcast::Sender<String>,
}

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> axum::response::Response {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: AppState) {
    let mut rx = state.tx.subscribe();
    while let Ok(msg) = rx.recv().await {
        if socket.send(WsMessage::Text(msg.into())).await.is_err() {
            break;
        }
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    ctrlc::set_handler(|| {
        let count = COUNT.load(Ordering::Relaxed);
        eprintln!("\nReceived {count} position reports.");
        std::process::exit(0);
    })
    .expect("failed to set Ctrl+C handler");

    let (tx, _) = broadcast::channel::<String>(256);
    let tx_sdr = tx.clone();

    let json_output = args.json;
    let device = args.device;
    let gain = args.gain;
    let bias_tee = args.bias_tee;

    std::thread::spawn(move || {
        eprintln!("Connecting to RTL-SDR device {device}...");
        let source = match AisIqSource::from_rtlsdr(
            DeviceSelector::Index(device),
            AIS_SAMPLE_RATE,
            gain,
            bias_tee,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to open RTL-SDR: {e}");
                std::process::exit(1);
            }
        };
        eprintln!("Listening for AIS on 161.975 / 162.025 MHz...");

        let mut filter = OutlierFilter::new();

        for result in source {
            match result {
                Ok(demod) => {
                    if let Some(decoded) = demod.decode() {
                        if let Some(pos) = extract_position(
                            &decoded,
                            demod.timestamp,
                            demod.channel,
                            demod.signal_level,
                        ) {
                            if json_output {
                                if let Ok(json) = serde_json::to_string(&pos) {
                                    println!("{json}");
                                }
                            } else {
                                println!("{}", format_human(&pos));
                            }
                            COUNT.fetch_add(1, Ordering::Relaxed);

                            if filter.is_valid(&pos) {
                                if let Ok(json) = serde_json::to_string(&pos) {
                                    let _ = tx_sdr.send(json);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("SDR error: {e}");
                }
            }
        }
    });

    let state = AppState { tx };
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let port = args.port;
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .expect("failed to bind port");
    eprintln!("Map UI: http://localhost:{port}");
    axum::serve(listener, app).await.unwrap();
}
