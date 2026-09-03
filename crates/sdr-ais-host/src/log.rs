//! Position logger: connects to the guest's vsock log stream and writes to SQLite.

use anyhow::{Context, Result};
use rusqlite::Connection;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use ais_core::PositionData;

pub async fn run(uds_path: &str, guest_port: u32, db_path: &str) -> Result<()> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening database {db_path}"))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS positions (
             id           INTEGER PRIMARY KEY,
             timestamp    TEXT    NOT NULL,
             mmsi         INTEGER NOT NULL,
             lat          REAL,
             lon          REAL,
             speed_kn     REAL,
             course_deg   REAL,
             heading      INTEGER,
             nav_status   TEXT,
             ship_name    TEXT,
             signal_level REAL NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_positions_mmsi ON positions (mmsi);
         CREATE INDEX IF NOT EXISTS idx_positions_ts   ON positions (timestamp);",
    )?;

    eprintln!("log: database {db_path}");

    let vsock = connect_vsock(uds_path, guest_port).await?;
    eprintln!("log: connected to guest vsock port {guest_port}");

    let mut lines = BufReader::new(vsock).lines();
    let mut count: u64 = 0;

    while let Some(line) = lines.next_line().await? {
        let pos: PositionData = match serde_json::from_str(&line) {
            Ok(p) => p,
            Err(_) => continue,
        };

        conn.execute(
            "INSERT INTO positions (timestamp, mmsi, lat, lon, speed_kn, course_deg, heading, nav_status, ship_name, signal_level)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                pos.timestamp,
                pos.mmsi,
                pos.lat,
                pos.lon,
                pos.speed_kn,
                pos.course_deg,
                pos.heading,
                pos.nav_status,
                pos.ship_name,
                pos.signal_level,
            ],
        )?;

        count += 1;
        if count % 1000 == 0 {
            eprintln!("log: {count} positions recorded");
        }
    }

    Ok(())
}

async fn connect_vsock(uds_path: &str, guest_port: u32) -> Result<UnixStream> {
    let mut vsock = UnixStream::connect(uds_path)
        .await
        .with_context(|| format!("connecting to {uds_path}"))?;

    vsock
        .write_all(format!("CONNECT {guest_port}\n").as_bytes())
        .await
        .context("sending CONNECT")?;

    let mut reader = BufReader::new(vsock);
    let mut response = String::new();
    let n = reader
        .read_line(&mut response)
        .await
        .context("reading handshake response")?;

    if n == 0 {
        anyhow::bail!("guest closed the socket: nothing listening on port {guest_port}");
    }
    if !response.starts_with("OK ") {
        anyhow::bail!("unexpected handshake response: {:?}", response.trim());
    }

    Ok(reader.into_inner())
}
