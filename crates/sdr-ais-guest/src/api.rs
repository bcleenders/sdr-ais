//! The public read-only API.
//!
//! Everything here is reachable from the open internet via Tailscale Funnel, so
//! the guiding rule is: accept nothing. There are no writes, no path
//! parameters, no query parameters, no request bodies and no upstream fetches.
//! The only input that reaches this process is radio traffic, and that is
//! parsed inside the same sandbox.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ais_core::{PositionData, VesselTable};
use axum::Router;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::broadcast;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;

/// Streaming clients allowed at once. Each holds a broadcast receiver and a
/// socket; this bounds guest memory against a connection-exhaustion attempt.
const MAX_WS_CLIENTS: usize = 64;

/// Broadcast backlog. A client that falls further behind than this is told it
/// lagged and resynchronises from the snapshot rather than stalling the sender.
const BROADCAST_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct AppState {
    tx: broadcast::Sender<String>,
    vessels: Arc<Mutex<VesselTable>>,
    ws_clients: Arc<AtomicUsize>,
    allowed_origins: Arc<Vec<String>>,
}

impl AppState {
    /// `allowed_origins` is a comma-separated list, as it arrives on the
    /// kernel command line. Production carries only the frontend origin;
    /// vm-up.sh can append a localhost origin for local development.
    pub fn new(allowed_origins: String) -> (Self, broadcast::Sender<String>) {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let origins: Vec<String> = allowed_origins
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let state = Self {
            tx: tx.clone(),
            vessels: Arc::new(Mutex::new(VesselTable::new())),
            ws_clients: Arc::new(AtomicUsize::new(0)),
            allowed_origins: Arc::new(origins),
        };
        (state, tx)
    }

    /// Record a freshly decoded position and fan it out to live clients.
    pub fn publish(&self, pos: PositionData) {
        if let Ok(json) = serde_json::to_string(&pos) {
            // Send before locking so a poisoned mutex cannot stall the stream.
            let _ = self.tx.send(json);
        }
        if let Ok(mut table) = self.vessels.lock() {
            table.insert(pos);
        }
    }
}

pub fn router(state: AppState) -> Router {
    let allowed = state.allowed_origins.clone();

    // Only the map frontend may read this from a browser. Note this is a
    // browser-side control, not authentication: it stops other origins from
    // embedding the feed, but any non-browser client can send whatever Origin
    // it likes. The data is public vessel positions, so that is acceptable.
    let origin_values: Vec<HeaderValue> = allowed
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origin_values))
        .allow_methods([Method::GET])
        .max_age(Duration::from_secs(600));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/vessels", get(vessels_handler))
        .route("/ws", get(ws_handler))
        .layer(cors)
        // No endpoint reads a body; reject anything that arrives with one.
        .layer(RequestBodyLimitLayer::new(0))
        // Bounds how long a request may take to produce a response. This does
        // not cap WebSocket lifetime: the /ws handler returns its 101 straight
        // away and hyper drives the upgraded stream separately, outside this
        // service future.
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn vessels_handler(State(state): State<AppState>) -> Response {
    let snapshot: Vec<PositionData> = match state.vessels.lock() {
        Ok(table) => table.snapshot(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    axum::Json(snapshot).into_response()
}

/// WebSocket upgrades bypass CORS entirely — browsers do not preflight them —
/// so the Origin header has to be checked by hand here.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());

    match origin {
        Some(o) if state.allowed_origins.iter().any(|a| a == o) => {}
        // Absent Origin means a non-browser client. Allowed: the feed is public
        // and this keeps `websocat`/curl debugging workable.
        None => {}
        Some(_) => return StatusCode::FORBIDDEN.into_response(),
    }

    if state.ws_clients.load(Ordering::Relaxed) >= MAX_WS_CLIENTS {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    ws.max_message_size(4 * 1024)
        .max_frame_size(4 * 1024)
        .on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: AppState) {
    // Claim a slot, rechecking under the fetch_add so concurrent upgrades
    // cannot overshoot the cap.
    let slot = state.ws_clients.fetch_add(1, Ordering::AcqRel);
    if slot >= MAX_WS_CLIENTS {
        state.ws_clients.fetch_sub(1, Ordering::AcqRel);
        return;
    }

    let mut rx = state.tx.subscribe();

    // Prime the client with current state so the map renders immediately
    // instead of staying blank until each ship next transmits.
    let snapshot = state.vessels.lock().ok().map(|t| t.snapshot());
    if let Some(snapshot) = snapshot {
        for pos in snapshot {
            if let Ok(json) = serde_json::to_string(&pos) {
                if socket.send(WsMessage::Text(json.into())).await.is_err() {
                    state.ws_clients.fetch_sub(1, Ordering::AcqRel);
                    return;
                }
            }
        }
    }

    loop {
        match rx.recv().await {
            Ok(msg) => {
                if socket.send(WsMessage::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            // Slow client: it missed messages but the connection is still good,
            // so keep going from the current position rather than dropping it.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }

    state.ws_clients.fetch_sub(1, Ordering::AcqRel);
}
