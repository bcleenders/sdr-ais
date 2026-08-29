//! Teaches `axum::serve` to accept connections over AF_VSOCK.
//!
//! The guest has no network interface at all, so there is no TCP listener to
//! bind. Every byte of HTTP arrives over the virtio-vsock device.

use std::io;
use std::time::Duration;

use tokio_vsock::{VsockAddr, VsockListener, VsockStream};

pub struct VsockAxumListener {
    inner: VsockListener,
    addr: VsockAddr,
}

impl VsockAxumListener {
    pub fn bind(cid: u32, port: u32) -> io::Result<Self> {
        let addr = VsockAddr::new(cid, port);
        let inner = VsockListener::bind(addr)?;
        Ok(Self { inner, addr })
    }
}

impl axum::serve::Listener for VsockAxumListener {
    type Io = VsockStream;
    type Addr = VsockAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.inner.accept().await {
                Ok(pair) => return pair,
                // Mirrors axum's own TcpListener behaviour: a failed accept is
                // almost always a transient per-connection error (the peer went
                // away, or we are briefly out of file descriptors). Backing off
                // avoids spinning the CPU at 100% on a persistent failure.
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.addr)
    }
}
