//! Loopback TCP → guest vsock.
//!
//! Tailscale Funnel terminates TLS and forwards plaintext HTTP to a local port.
//! Firecracker exposes the guest's vsock listeners through a Unix socket with a
//! small text handshake, which no off-the-shelf proxy speaks — hence this.
//!
//! It is a byte pump. It does not parse HTTP; a request is opaque bytes until
//! it is inside the microVM.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixStream};
use tokio::sync::Semaphore;

/// Concurrent connections allowed through the bridge. Each one costs a guest
/// vsock connection, so this is the outermost bound on how much a flood of
/// internet requests can cost the VM.
const MAX_CONNECTIONS: usize = 128;

/// Firecracker answers the CONNECT handshake immediately when the guest is
/// listening; a hang means the guest is wedged, so fail fast.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run(uds_path: &str, guest_port: u32, listen: &str) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;

    eprintln!("proxy: {listen} -> vsock guest port {guest_port} via {uds_path}");

    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let uds_path: Arc<str> = Arc::from(uds_path);

    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("proxy: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };

        // Shed load rather than queueing unboundedly.
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            eprintln!("proxy: at connection limit, rejecting {peer}");
            drop(tcp);
            continue;
        };

        let uds_path = uds_path.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = bridge(tcp, &uds_path, guest_port).await {
                eprintln!("proxy: connection from {peer} ended: {e:#}");
            }
        });
    }
}

async fn bridge(mut tcp: tokio::net::TcpStream, uds_path: &str, guest_port: u32) -> Result<()> {
    let vsock = UnixStream::connect(uds_path)
        .await
        .with_context(|| format!("connecting to {uds_path}"))?;

    let mut vsock = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(vsock, guest_port))
        .await
        .context("vsock handshake timed out")??;

    // Neither side is interpreted; copy until one hangs up.
    tokio::io::copy_bidirectional(&mut tcp, &mut vsock)
        .await
        .context("relaying")?;

    Ok(())
}

/// Perform Firecracker's host-initiated vsock handshake.
///
/// The protocol is: write `CONNECT <guest_port>\n`, then read a line back.
/// `OK <assigned_port>` means the guest had a listener on that port; anything
/// else (usually an immediate EOF) means it did not.
async fn handshake(vsock: UnixStream, guest_port: u32) -> Result<UnixStream> {
    let mut vsock = BufReader::new(vsock);
    vsock
        .get_mut()
        .write_all(format!("CONNECT {guest_port}\n").as_bytes())
        .await
        .context("sending CONNECT")?;

    let mut response = String::new();
    let n = vsock
        .read_line(&mut response)
        .await
        .context("reading handshake response")?;

    if n == 0 {
        bail!("firecracker closed the socket: nothing listening on guest port {guest_port}");
    }
    if !response.starts_with("OK ") {
        bail!("unexpected handshake response: {:?}", response.trim());
    }

    // BufReader may have buffered bytes past the newline. It should not, since
    // the guest cannot speak before we send a request, but assert rather than
    // silently dropping part of the stream.
    if !vsock.buffer().is_empty() {
        bail!("guest sent unexpected data before the request");
    }

    Ok(vsock.into_inner())
}
