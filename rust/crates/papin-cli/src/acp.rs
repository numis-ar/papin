use crate::http::Result;
use acp_wire::{method, Envelope};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::protocol::Message;

/// One event delivered to the UI/headless loop.
#[derive(Debug, Clone)]
pub enum AcpEvent {
    /// Agent-originated notification (session/update, ...).
    Notification(Envelope),
    /// A response whose request the client does not track (e.g. an in-flight
    /// prompt handled by the app loop).
    Response(Envelope),
    /// Agent→client reverse-RPC (permission / elicitation).
    ReverseRequest(Envelope),
    /// The WebSocket dropped; the caller should reconnect + resume.
    Disconnected,
}

type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<Envelope>>>>;

/// ACP client over the gateway WebSocket. Responses to our requests are
/// demuxed by JSON-RPC id; everything else becomes an [`AcpEvent`].
pub struct AcpClient {
    out: mpsc::UnboundedSender<Envelope>,
    events: mpsc::UnboundedReceiver<AcpEvent>,
    pending: PendingMap,
    next_id: Arc<AtomicI64>,
}

impl AcpClient {
    pub async fn connect(ws_url: &str, token: &str) -> Result<AcpClient> {
        let mut stream =
            acp_wire::ws_connect_with(ws_url, &[("Authorization", &format!("Bearer {token}"))])
                .await
                .map_err(|e| format!("ws connect {ws_url}: {e}"))?;
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Envelope>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<AcpEvent>();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let pending_w = Arc::clone(&pending);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Client → agent
                    Some(env) = out_rx.recv() => {
                        let text = serde_json::to_string(&env).unwrap_or_default();
                        if stream.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                        // A response to an agent reverse-RPC needs no reply-tracking here.
                    }
                    // Agent → client
                    msg = stream.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(env) = serde_json::from_str::<Envelope>(&text) {
                                    if env.is_response() {
                                        let key = env.id.as_ref()
                                            .map(|id| serde_json::to_string(id).unwrap_or_default())
                                            .unwrap_or_default();
                                        let tx = pending_w.lock().unwrap().remove(&key);
                                        match tx {
                                            Some(tx) => { let _ = tx.send(env); }
                                            None => { let _ = ev_tx.send(AcpEvent::Response(env)); }
                                        }
                                    } else if env.is_request() {
                                        let _ = ev_tx.send(AcpEvent::ReverseRequest(env));
                                    } else {
                                        let _ = ev_tx.send(AcpEvent::Notification(env));
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Err(_)) => break,
                            _ => {}
                        }
                    }
                }
            }
            let _ = ev_tx.send(AcpEvent::Disconnected);
        });

        Ok(AcpClient {
            out: out_tx,
            events: ev_rx,
            pending,
            next_id: Arc::new(AtomicI64::new(1)),
        })
    }

    pub fn id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a request and await its response (30 s timeout).
    pub async fn request(&mut self, method_name: &str, params: Value) -> Result<Envelope> {
        let id = self.id();
        self.request_with_id(id, method_name, params).await
    }

    pub async fn request_with_id(
        &mut self,
        id: i64,
        method_name: &str,
        params: Value,
    ) -> Result<Envelope> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(serde_json::to_string(&json!(id))?, tx);
        let env = Envelope::request(id, method_name, params);
        self.out
            .send(env)
            .map_err(|_| "agent connection closed".to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| format!("timeout waiting for {method_name} response"))?
            .map_err(|_| "agent connection closed".into())
    }

    pub fn notify(&self, method_name: &str, params: Value) -> Result<()> {
        self.out
            .send(Envelope::notification(method_name, params))
            .map_err(|_| "agent connection closed".to_string().into())
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<()> {
        self.out
            .send(Envelope::response(id, result))
            .map_err(|_| "agent connection closed".to_string().into())
    }

    pub async fn next_event(&mut self) -> Option<AcpEvent> {
        self.events.recv().await
    }

    /// `initialize` → `initialized` handshake.
    pub async fn initialize(&mut self) -> Result<Envelope> {
        let resp = self
            .request(
                method::INITIALIZE,
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "papin-cli", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        self.notify(method::INITIALIZED, json!({}))?;
        Ok(resp)
    }

    pub async fn session_new(&mut self) -> Result<String> {
        let resp = self
            .request(method::SESSION_NEW, json!({"cwd": "/workspace"}))
            .await?;
        if let Some(err) = resp.error {
            return Err(err.message.into());
        }
        Ok(resp
            .result
            .as_ref()
            .and_then(|r| r["sessionId"].as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn session_load(&mut self, session_id: &str) -> Result<()> {
        let resp = self
            .request(method::SESSION_LOAD, json!({"sessionId": session_id}))
            .await?;
        if let Some(err) = resp.error {
            return Err(err.message.into());
        }
        Ok(())
    }

    pub async fn session_list(&mut self) -> Result<Vec<(String, String)>> {
        let resp = self.request("session/list", json!({})).await?;
        if let Some(err) = resp.error {
            return Err(err.message.into());
        }
        Ok(resp
            .result
            .as_ref()
            .and_then(|r| r["sessions"].as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| {
                        Some((
                            s["sessionId"].as_str()?.to_string(),
                            s["title"].as_str().unwrap_or("").to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn prompt(&mut self, session_id: &str, text: &str, id: i64) -> Result<()> {
        let mut env = Envelope::request(
            id,
            method::SESSION_PROMPT,
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": text}]
            }),
        );
        env.session_id = Some(session_id.to_string());
        self.out
            .send(env)
            .map_err(|_| "agent connection closed".to_string())?;
        Ok(())
    }

    pub async fn cancel(&mut self, session_id: &str) -> Result<Envelope> {
        self.request(method::SESSION_CANCEL, json!({"sessionId": session_id}))
            .await
    }
}
