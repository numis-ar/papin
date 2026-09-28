use crate::auth::auth_middleware;
use crate::bootstrap::BootstrapEngine;
use crate::config::{Catalog, SharedTokenStore};
use crate::manager::{AgentState, ConnectionManager};
use crate::provider::AgentProvider;
use crate::provider::AgentSpec;
use crate::GatewayError;
use crate::Result;
use acp_wire::{method, Envelope};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::stream::StreamExt;
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// WireGuard enrollment settings (§7.1).
#[derive(Clone, Debug, Default)]
pub struct EnrollConfig {
    pub peers_dir: PathBuf,
    pub gateway_ip: String,
    pub client_pool: String,
    pub helper: PathBuf,
}

impl EnrollConfig {
    pub fn configured(&self) -> bool {
        !self.peers_dir.as_os_str().is_empty() && !self.helper.as_os_str().is_empty()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub manager: Arc<ConnectionManager>,
    pub provider: Arc<dyn AgentProvider>,
    pub bootstrap: Arc<dyn BootstrapEngine>,
    pub catalog: Catalog,
    pub tokens: SharedTokenStore,
    pub enroll: EnrollConfig,
}

pub fn router(state: Arc<AppState>) -> Router {
    let acp = Router::new()
        .route("/acp/agents/{id}", get(ws_acp).post(post_acp))
        .route("/api/v1/agents", get(list_agents).post(create_agent))
        .route("/api/v1/agents/{id}", delete(delete_agent))
        .route("/api/v1/config-catalog", get(config_catalog))
        .route("/api/v1/status", get(status))
        .route("/api/v1/enroll", post(enroll))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(acp)
        .with_state(state)
}

pub async fn serve(state: Arc<AppState>, listen: &str) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    serve_on(state, listener).await
}

pub async fn serve_on(state: Arc<AppState>, listener: tokio::net::TcpListener) -> Result<()> {
    axum::serve(listener, router(state)).await?;
    Ok(())
}

/// Build a gateway AppState from its parts (used by tests and `main`).
pub fn make_state(
    provider: Arc<dyn AgentProvider>,
    bootstrap: Arc<dyn BootstrapEngine>,
    catalog: Catalog,
    idle_timeout: Duration,
    tokens: SharedTokenStore,
) -> Arc<AppState> {
    make_state_full(
        provider,
        bootstrap,
        catalog,
        idle_timeout,
        tokens,
        EnrollConfig::default(),
    )
}

pub fn make_state_full(
    provider: Arc<dyn AgentProvider>,
    bootstrap: Arc<dyn BootstrapEngine>,
    catalog: Catalog,
    idle_timeout: Duration,
    tokens: SharedTokenStore,
    enroll: EnrollConfig,
) -> Arc<AppState> {
    let manager = Arc::new(ConnectionManager::new(Arc::clone(&provider), idle_timeout));
    Arc::new(AppState {
        manager,
        provider,
        bootstrap,
        catalog,
        tokens,
        enroll,
    })
}

/// Backwards-compatible helper for M1-style tests: FakeProvider-backed state
/// without a bootstrap engine (no registry REST).
pub fn fake_state(
    provider: Arc<dyn AgentProvider>,
    idle_timeout: Duration,
    tokens: SharedTokenStore,
) -> Arc<AppState> {
    make_state(
        provider,
        Arc::new(crate::bootstrap::FakeBootstrap::new(
            "/nonexistent",
            "/nonexistent",
        )),
        Catalog::default(),
        idle_timeout,
        tokens,
    )
}

/// GET /api/v1/agents → records + gateway-side state (§5.1).
async fn list_agents(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.provider.list().await {
        Ok(records) => {
            let out: Vec<Value> = records
                .into_iter()
                .map(|r| {
                    let mut v = serde_json::to_value(&r).unwrap_or_default();
                    v["state"] = json!(state.manager.state(&r.id));
                    v
                })
                .collect();
            Json(json!(out)).into_response()
        }
        Err(e) => internal_error(&e),
    }
}

/// POST /api/v1/agents → validate vs catalog, write metadata, bootstrap rootfs.
async fn create_agent(State(state): State<Arc<AppState>>, body: String) -> impl IntoResponse {
    let Ok(spec) = serde_json::from_str::<AgentSpec>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_spec", "message": "body must be {id?, name?, config: {base, seed?, env?}}"})),
        )
            .into_response();
    };
    let record = match state.provider.create(spec).await {
        Ok(r) => r,
        Err(GatewayError::InvalidInput(m)) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid_spec", "message": m})),
            )
                .into_response();
        }
        Err(GatewayError::AlreadyExists(m)) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "already_exists", "message": m})),
            )
                .into_response();
        }
        Err(e) => return internal_error(&e),
    };
    if let Err(e) = state.bootstrap.bootstrap(&record).await {
        // Roll back the metadata record; the agent must not half-exist.
        let _ = state.provider.remove(&record.id, false).await;
        return internal_error(&e);
    }
    let mut v = serde_json::to_value(&record).unwrap_or_default();
    v["state"] = json!(AgentState::Stopped);
    (StatusCode::CREATED, Json(v)).into_response()
}

#[derive(Debug, Deserialize)]
struct DeleteQuery {
    force: Option<bool>,
}

/// DELETE /api/v1/agents/{id} → 409 while attached unless ?force=true.
async fn delete_agent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> impl IntoResponse {
    let records = match state.provider.list().await {
        Ok(rs) => rs,
        Err(e) => return internal_error(&e),
    };
    if !records.iter().any(|r| r.id == id) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not_found", "message": format!("agent {id} does not exist")})),
        )
            .into_response();
    }
    let attached = state.manager.attached_clients(&id);
    if !attached.is_empty() && q.force != Some(true) {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "agent_attached",
                "message": format!("agent {id} has {} attached connection(s); retry with ?force=true", attached.len())
            })),
        )
            .into_response();
    }
    if q.force == Some(true) {
        state.manager.detach_all(&id);
    }
    if let Err(e) = state.bootstrap.remove(&id).await {
        return internal_error(&e);
    }
    match state.provider.remove(&id, true).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(GatewayError::NotFound(m)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not_found", "message": m})),
        )
            .into_response(),
        Err(e) => internal_error(&e),
    }
}

/// GET /api/v1/config-catalog → what clients may select (§5.1).
async fn config_catalog(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::to_value(&state.catalog).unwrap_or_default())
}

/// GET /api/v1/status (§5.1 ops): versions, protocol v1, agent counts, WG
/// interface state (best-effort, no privileges needed).
async fn status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let records = state.provider.list().await.unwrap_or_default();
    let active = records
        .iter()
        .filter(|r| state.manager.state(&r.id) == AgentState::Active)
        .count();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": 1,
        "agents": {"total": records.len(), "active": active},
        "wireguard": {"iface": "wg0", "state": wg_iface_state()},
    }))
}

/// Best-effort WireGuard interface state: /sys if present, else unknown.
fn wg_iface_state() -> &'static str {
    for path in [
        "/sys/class/net/wg0/operstate",
        "/sys/class/net/wg0/statistics/rx_packets",
    ] {
        if PathBuf::from(path).exists() {
            return if path.ends_with("operstate") {
                std::fs::read_to_string(path)
                    .map(|s| match s.trim() {
                        "up" => "up",
                        "down" => "down",
                        _ => "unknown",
                    })
                    .unwrap_or("unknown")
            } else {
                "present"
            };
        }
    }
    "unknown"
}

/// POST /api/v1/enroll (§7.1): bind the client's WireGuard pubkey to the
/// bearer token, render a peers file, and install it via the setuid helper.
async fn enroll(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    if !state.enroll.configured() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "enroll_not_configured", "message": "WireGuard enrollment is not configured on this gateway"})),
        )
            .into_response();
    }
    let token = bearer(&headers).unwrap_or_default();
    if !state.tokens.validate(&token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized", "message": "device token unknown or expired; re-run add-peer on the server to reissue"})),
        )
            .into_response();
    }
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_body", "message": "body must be {\"client_pubkey\": \"<base64>\"}"})),
        )
            .into_response();
    };
    let Some(pubkey) = body["client_pubkey"].as_str() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_body", "message": "client_pubkey is required"})),
        )
            .into_response();
    };
    if !papin_enroll_helper::validate_public_key(pubkey) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_pubkey", "message": "client_pubkey is not a valid WireGuard public key"})),
        )
            .into_response();
    }

    // Token → IP binding; re-enrollment (re-keying) overwrites the pubkey and
    // keeps the assigned address (§7.1).
    let entry = match state.tokens.entry(&token) {
        Some(e) => e,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(
                    json!({"error": "unauthorized", "message": "device token unknown or expired"}),
                ),
            )
                .into_response();
        }
    };
    let assigned_ip = match entry.assigned_ip.clone() {
        Some(ip) => ip,
        None => match state
            .tokens
            .allocate_ip(&state.enroll.client_pool, &state.enroll.gateway_ip)
        {
            Some(ip) => ip.to_string(),
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": "pool_exhausted", "message": "no free addresses in the client pool"})),
                )
                    .into_response();
            }
        },
    };

    // Render the complete peers file into the helper's directory.
    let peers_file = state
        .enroll
        .peers_dir
        .join(format!("{}.conf", &token[..token.len().min(16)]));
    let conf = build_peer_conf(pubkey, &assigned_ip);
    if let Err(e) = std::fs::create_dir_all(&state.enroll.peers_dir)
        .and_then(|_| std::fs::write(&peers_file, &conf))
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                json!({"error": "peers_file", "message": format!("cannot write peers file: {e}")}),
            ),
        )
            .into_response();
    }

    // The ONLY root-capable step in the system: the dedicated setuid helper.
    let output = tokio::process::Command::new(&state.enroll.helper)
        .arg("apply")
        .arg(&peers_file)
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "wg_syncconf", "message": format!("papin-enroll-helper failed: {stderr}")})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "helper_unavailable", "message": format!("cannot run papin-enroll-helper (see operations.md): {e}")})),
            )
                .into_response();
        }
    }

    if let Err(e) = state.tokens.bind(&token, pubkey, &assigned_ip) {
        return internal_error(&e);
    }
    Json(json!({
        "assigned_ip": assigned_ip,
        "gateway_ip": state.enroll.gateway_ip,
    }))
    .into_response()
}

fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
}

/// One-peer wg-quick stanza; must pass `papin-enroll-helper` validation.
pub fn build_peer_conf(pubkey: &str, assigned_ip: &str) -> String {
    let prefix = if assigned_ip.contains(':') { 128 } else { 32 };
    format!("[Peer]\nPublicKey = {pubkey}\nAllowedIPs = {assigned_ip}/{prefix}\n")
}

fn internal_error(e: &GatewayError) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": "internal", "message": e.to_string()})),
    )
        .into_response()
}

async fn ws_acp(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_acp_session(state, agent_id, socket))
}

async fn ws_acp_session(state: Arc<AppState>, agent_id: String, socket: WebSocket) {
    let Ok(attachment) = state.manager.attach(&agent_id) else {
        return;
    };
    let client_id = attachment.client_id;
    let mut agent_rx = attachment.rx;
    let (mut ws_tx, mut ws_rx) = socket.split();

    let to_client = tokio::spawn(async move {
        while let Some(env) = agent_rx.recv().await {
            if ws_tx
                .send(Message::Text(
                    serde_json::to_string(&env).unwrap_or_default().into(),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Text(text) => match serde_json::from_str::<Envelope>(&text) {
                Ok(env) => {
                    if state
                        .manager
                        .send_from_client(&agent_id, client_id, env)
                        .is_err()
                    {
                        break;
                    }
                }
                Err(err) => {
                    tracing::warn!(?err, "ignoring malformed client frame");
                }
            },
            Message::Close(_) => break,
            _ => {}
        }
    }

    state.manager.detach(&agent_id, client_id);
    to_client.abort();
}

/// JSON-RPC over POST (RFD): `initialize` → 200+JSON; other single messages →
/// 202 (M1 returns the response envelope synchronously; SSE delivery is M6).
/// Batches → 501 (RFD).
async fn post_acp(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    let content_type_ok = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !content_type_ok {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(json!({"error": "unsupported_media_type", "message": "Content-Type must be application/json"})),
        )
            .into_response();
    }
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid_request", "message": "body must be a single JSON-RPC object"})),
            )
                .into_response();
        }
    };
    if parsed.is_array() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "batch_unsupported", "message": "JSON-RPC batches are not supported"})),
        )
            .into_response();
    }
    let Ok(env) = serde_json::from_value::<Envelope>(parsed) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_envelope", "message": "not a valid ACP JSON-RPC envelope"})),
        )
            .into_response();
    };

    let is_initialize = env.method.as_deref() == Some(method::INITIALIZE);
    let id = env.id.clone();

    let Some(id) = id else {
        // Notification: forward, acknowledge with 202.
        let _ = state.manager.send_from_client(&agent_id, usize::MAX, env);
        return StatusCode::ACCEPTED.into_response();
    };

    match state.manager.round_trip(&agent_id, env).await {
        Ok(resp) => {
            let status = if is_initialize {
                StatusCode::OK
            } else {
                StatusCode::ACCEPTED
            };
            (status, Json(serde_json::to_value(resp).unwrap_or_default())).into_response()
        }
        Err(e) => {
            let (status, message) = match &e {
                crate::GatewayError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
                crate::GatewayError::AgentUnavailable(m) => {
                    (StatusCode::SERVICE_UNAVAILABLE, m.clone())
                }
                crate::GatewayError::Timeout(m) => (StatusCode::GATEWAY_TIMEOUT, m.clone()),
                other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
            };
            (
                status,
                Json(json!({
                    "id": id,
                    "error": {"code": -32000, "message": message}
                })),
            )
                .into_response()
        }
    }
}
