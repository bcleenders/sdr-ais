//! Shared AIS decoding, filtering and vessel-state logic.
//!
//! This crate is deliberately free of I/O and hardware dependencies so that it
//! can be compiled into the sandboxed guest (static musl, no USB, no network
//! stack) as well as into the local development CLI.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use rs162::decode::ais::Message;
use serde::Serialize;

/// Sample rate the host captures at. 288 kHz is the only rate in the RTL-SDR's
/// low window that divides exactly to the demodulator's 96 kHz, so the sample
/// rate adapter does no upsampling.
pub const AIS_SAMPLE_RATE: u32 = 288_000;

#[derive(Clone, Serialize)]
pub struct PositionData {
    pub timestamp: String,
    pub channel: char,
    pub mmsi: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_kn: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub course_deg: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nav_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ship_name: Option<String>,
    pub signal_level: f32,
}

pub fn extract_position(
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

pub fn format_human(p: &PositionData) -> String {
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

    let hdg = p.heading.map(|h| format!("hdg:{h:3}")).unwrap_or_default();

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

/// Rejects reports whose position is implausibly far from the running median of
/// recently received positions. AIS is received off-air, so a corrupted frame
/// that still passes CRC can carry a wild lat/lon.
pub struct OutlierFilter {
    lats: VecDeque<f64>,
    lons: VecDeque<f64>,
    median_lat: Option<f64>,
    median_lon: Option<f64>,
}

impl Default for OutlierFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl OutlierFilter {
    pub fn new() -> Self {
        Self {
            lats: VecDeque::new(),
            lons: VecDeque::new(),
            median_lat: None,
            median_lon: None,
        }
    }

    pub fn is_valid(&mut self, pos: &PositionData) -> bool {
        let (lat, lon) = match (pos.lat, pos.lon) {
            (Some(lat), Some(lon)) => (lat, lon),
            _ => return false,
        };

        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return false;
        }

        if let (Some(med_lat), Some(med_lon)) = (self.median_lat, self.median_lon) {
            if (lat - med_lat).abs() > 0.5 || (lon - med_lon).abs() > 0.5 {
                return false;
            }
        }

        const WINDOW: usize = 200;
        if self.lats.len() >= WINDOW {
            self.lats.pop_front();
            self.lons.pop_front();
        }
        self.lats.push_back(lat);
        self.lons.push_back(lon);

        if self.lats.len() >= 20 {
            let mut sorted_lats: Vec<f64> = self.lats.iter().copied().collect();
            let mut sorted_lons: Vec<f64> = self.lons.iter().copied().collect();
            sorted_lats.sort_by(|a, b| a.partial_cmp(b).unwrap());
            sorted_lons.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mid = sorted_lats.len() / 2;
            self.median_lat = Some(sorted_lats[mid]);
            self.median_lon = Some(sorted_lons[mid]);
        }

        true
    }
}

/// Most recent position per vessel, so a newly connected client can render
/// immediately instead of waiting for each ship's next transmission.
///
/// Bounded: a hostile or malfunctioning transmitter cannot grow this without
/// limit, since inserting past `MAX_VESSELS` evicts the least recently heard.
pub struct VesselTable {
    vessels: HashMap<u32, (PositionData, u64)>,
    seq: u64,
}

impl Default for VesselTable {
    fn default() -> Self {
        Self::new()
    }
}

impl VesselTable {
    /// Amsterdam sees a few hundred vessels; 4096 is generous headroom while
    /// still bounding the snapshot response to a few hundred kilobytes.
    pub const MAX_VESSELS: usize = 4096;

    pub fn new() -> Self {
        Self {
            vessels: HashMap::new(),
            seq: 0,
        }
    }

    pub fn insert(&mut self, pos: PositionData) {
        self.seq += 1;
        if self.vessels.len() >= Self::MAX_VESSELS && !self.vessels.contains_key(&pos.mmsi) {
            if let Some(&oldest) = self
                .vessels
                .iter()
                .min_by_key(|(_, (_, seq))| *seq)
                .map(|(mmsi, _)| mmsi)
            {
                self.vessels.remove(&oldest);
            }
        }
        self.vessels.insert(pos.mmsi, (pos, self.seq));
    }

    pub fn snapshot(&self) -> Vec<PositionData> {
        self.vessels.values().map(|(p, _)| p.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.vessels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vessels.is_empty()
    }
}
