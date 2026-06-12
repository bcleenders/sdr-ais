use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use clap::Parser;
use desperado::rtlsdr::DeviceSelector;
use rs162::decode::ais::Message;
use rs162::sources::AisIqSource;
use serde::Serialize;

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
}

#[derive(Serialize)]
struct PositionData {
    timestamp: String,
    channel: char,
    mmsi: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed_kn: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    course_deg: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    heading: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nav_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ship_name: Option<String>,
    signal_level: f32,
}

fn extract_position(
    decoded: &Message,
    timestamp: f64,
    channel: char,
    signal_level: f32,
) -> Option<PositionData> {
    let ts = DateTime::<Utc>::from_timestamp(timestamp as i64, 0)?
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    match decoded {
        Message::PositionReport1(r)
        | Message::PositionReport2(r)
        | Message::PositionReport3(r) => Some(PositionData {
            timestamp: ts,
            channel,
            mmsi: r.mmsi,
            lat: r.latitude,
            lon: r.longitude,
            speed_kn: r.speed,
            course_deg: r.course,
            heading: r.heading,
            nav_status: Some(format!("{:?}", r.status)),
            ship_name: None,
            signal_level,
        }),

        Message::ClassBPositionReport(r) => Some(PositionData {
            timestamp: ts,
            channel,
            mmsi: r.mmsi,
            lat: r.latitude,
            lon: r.longitude,
            speed_kn: r.speed,
            course_deg: r.course,
            heading: r.heading,
            nav_status: None,
            ship_name: None,
            signal_level,
        }),

        Message::ExtendedClassBPositionReport(r) => Some(PositionData {
            timestamp: ts,
            channel,
            mmsi: r.mmsi,
            lat: r.latitude,
            lon: r.longitude,
            speed_kn: r.speed,
            course_deg: r.course,
            heading: r.heading,
            nav_status: None,
            ship_name: Some(r.shipname.trim().to_string()),
            signal_level,
        }),

        Message::LongRangeAisBroadcastMessage(r) => Some(PositionData {
            timestamp: ts,
            channel,
            mmsi: r.mmsi,
            lat: r.latitude,
            lon: r.longitude,
            speed_kn: r.speed.map(|s| s as f32),
            course_deg: r.course.map(|c| c as f32),
            heading: None,
            nav_status: Some(format!("{:?}", r.status)),
            ship_name: None,
            signal_level,
        }),

        Message::SarAircraftPositionReport(r) => Some(PositionData {
            timestamp: ts,
            channel,
            mmsi: r.mmsi,
            lat: r.latitude,
            lon: r.longitude,
            speed_kn: r.speed.map(|s| s as f32),
            course_deg: r.course,
            heading: None,
            nav_status: None,
            ship_name: None,
            signal_level,
        }),

        _ => None,
    }
}

fn format_human(p: &PositionData) -> String {
    let lat = p
        .lat
        .map(|v| {
            let (abs, dir) = if v >= 0.0 { (v, 'N') } else { (-v, 'S') };
            format!("{abs:8.4}{dir}")
        })
        .unwrap_or_else(|| "   N/A   ".to_string());

    let lon = p
        .lon
        .map(|v| {
            let (abs, dir) = if v >= 0.0 { (v, 'E') } else { (-v, 'W') };
            format!("{abs:9.4}{dir}")
        })
        .unwrap_or_else(|| "    N/A    ".to_string());

    let speed = p
        .speed_kn
        .map(|s| format!("{s:5.1}kn"))
        .unwrap_or_else(|| "    N/A".to_string());

    let course = p
        .course_deg
        .map(|c| format!("{c:5.1}\u{00b0}"))
        .unwrap_or_else(|| "   N/A".to_string());

    let hdg = p
        .heading
        .map(|h| format!("hdg:{h:3}"))
        .unwrap_or_default();

    let status = p.nav_status.as_deref().unwrap_or("");

    let name = p
        .ship_name
        .as_ref()
        .map(|n| format!(" [{n}]"))
        .unwrap_or_default();

    format!(
        "{ts} | CH-{ch} | MMSI {mmsi:>9} | {lat} {lon} | {speed} {course} {hdg} | {status}{name}",
        ts = p.timestamp,
        ch = p.channel,
        mmsi = p.mmsi,
    )
}

fn main() {
    let args = Args::parse();

    ctrlc::set_handler(|| {
        let count = COUNT.load(Ordering::Relaxed);
        eprintln!("\nReceived {count} position reports.");
        std::process::exit(0);
    })
    .expect("failed to set Ctrl+C handler");

    eprintln!("Connecting to RTL-SDR device {}...", args.device);
    let source = match AisIqSource::from_rtlsdr(
        DeviceSelector::Index(args.device),
        288_000,
        args.gain,
        args.bias_tee,
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to open RTL-SDR: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("Listening for AIS on 161.975 / 162.025 MHz...");

    for result in source {
        match result {
            Ok(demod) => {
                if let Some(decoded) = demod.decode() {
                    if let Some(pos) =
                        extract_position(&decoded, demod.timestamp, demod.channel, demod.signal_level)
                    {
                        if args.json {
                            if let Ok(json) = serde_json::to_string(&pos) {
                                println!("{json}");
                            }
                        } else {
                            println!("{}", format_human(&pos));
                        }
                        COUNT.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Err(e) => {
                eprintln!("SDR error: {e}");
            }
        }
    }
}
