use crate::error::{GatewayError, Result};
use crate::provider::{AgentProvider, BoxRead, BoxWrite};
use crate::proxy;
use acp_wire::{method, Envelope};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::BufReader;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

const GATEWAY_INIT_ID: &str = "_gw_init";
const REVERSE_RPC_GRACE: Duration = Duration::from_secs(2);
pub const ROUND_TRIP_TIMEOUT: Duration = Duration::from_secs(30);

/// Virtual client id for POST /acp round-trips (no WS attachment).
const POST_CLIENT: usize = usize::MAX;

type ClientSink = mpsc::UnboundedSender<Envelope>;

/// Gateway-side agent state for the registry (PLAN §5.1): `active` iff a
/// client connection is attached; `error` when the last connect attempt
/// failed and nobody is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Stopped,
    Active,
    Error,
}

pub struct ConnectionManager {
    provider: Arc<dyn AgentProvider>,
    idle_timeout: Duration,
    conns: Mutex<HashMap<String, Arc<AgentConn>>>,
}

/// A client's handle to an agent: send via [`ConnectionManager::send_from_client`],
/// receive agent-destined envelopes on `rx`.
pub struct Attachment {
    pub client_id: usize,
    pub rx: mpsc::UnboundedReceiver<Envelope>,
}

impl ConnectionManager {
    pub fn new(provider: Arc<dyn AgentProvider>, idle_timeout: Duration) -> Self {
        ConnectionManager {
            provider,
            idle_timeout,
            conns: Mutex::new(HashMap::new()),
        }
    }

    fn conn(&self, agent_id: &str) -> Arc<AgentConn> {
        let mut conns = self.conns.lock().unwrap();
        if let Some(c) = conns.get(agent_id) {
            return Arc::clone(c);
        }
        let (internal_tx, internal_rx) = mpsc::unbounded_channel::<Internal>();
        let conn = Arc::new(AgentConn {
            id: agent_id.to_string(),
            provider: Arc::clone(&self.provider),
            idle_timeout: self.idle_timeout,
            internal_tx,
            clients: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            session_owner: Mutex::new(HashMap::new()),
            next_client_id: AtomicUsize::new(1),
            connected: AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(0),
            last_error: Mutex::new(None),
        });
        tokio::spawn(conn_task(Arc::clone(&conn), internal_rx));
        conns.insert(agent_id.to_string(), Arc::clone(&conn));
        conn
    }

    /// Register a new client; the returned `rx` receives envelopes from the agent.
    pub fn attach(&self, agent_id: &str) -> Result<Attachment> {
        let conn = self.conn(agent_id);
        let client_id = conn.next_client_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
        conn.clients.lock().unwrap().insert(client_id, tx);
        // First attach opens the connection (§5.3).
        let _ = conn.internal_tx.send(Internal::Attach);
        Ok(Attachment { client_id, rx })
    }

    pub fn detach(&self, agent_id: &str, client_id: usize) {
        let Some(conn) = self.conns.lock().unwrap().get(agent_id).cloned() else {
            return;
        };
        conn.clients.lock().unwrap().remove(&client_id);
        schedule_grace_cancels(&conn, client_id);
        let _ = conn.internal_tx.send(Internal::Detach(client_id));
    }

    /// Client ids currently attached to an agent (empty if never connected).
    pub fn attached_clients(&self, agent_id: &str) -> Vec<usize> {
        self.conns
            .lock()
            .unwrap()
            .get(agent_id)
            .map(|c| c.clients.lock().unwrap().keys().copied().collect())
            .unwrap_or_default()
    }

    /// Detach every client of an agent (force-delete path).
    pub fn detach_all(&self, agent_id: &str) {
        for cid in self.attached_clients(agent_id) {
            self.detach(agent_id, cid);
        }
    }

    /// Registry-facing agent state (§5.1).
    pub fn state(&self, agent_id: &str) -> AgentState {
        let conn = self.conns.lock().unwrap().get(agent_id).cloned();
        match conn {
            None => AgentState::Stopped,
            Some(c) if !c.clients.lock().unwrap().is_empty() => AgentState::Active,
            Some(c) if c.last_error.lock().unwrap().is_some() => AgentState::Error,
            Some(_) => AgentState::Stopped,
        }
    }

    /// Forward one client-originated envelope to the agent.
    pub fn send_from_client(&self, agent_id: &str, client_id: usize, env: Envelope) -> Result<()> {
        let conn = self.conn(agent_id);
        conn.internal_tx
            .send(Internal::FromClient(client_id, env))
            .map_err(|_| GatewayError::Closed(format!("agent {} task gone", conn.id)))
    }

    /// POST /acp M1 path: forward and await the JSON-RPC response synchronously.
    pub async fn round_trip(&self, agent_id: &str, env: Envelope) -> Result<Envelope> {
        let conn = self.conn(agent_id);
        let id = env
            .id
            .clone()
            .ok_or_else(|| GatewayError::InvalidInput("POST message has no id".into()))?;
        let (tx, rx) = oneshot::channel();
        conn.pending
            .lock()
            .unwrap()
            .entry(proxy::id_key(&id))
            .or_insert(PendingEntry {
                target: Target::Gateway(tx),
                method: env.method.clone(),
                cid: None,
            });
        self.send_from_client(agent_id, POST_CLIENT, env)?;
        tokio::time::timeout(ROUND_TRIP_TIMEOUT, rx)
            .await
            .map_err(|_| {
                conn.pending.lock().unwrap().remove(&proxy::id_key(&id));
                GatewayError::Timeout(format!("agent {} did not respond", conn.id))
            })?
            .map_err(|_| GatewayError::Closed("gateway response channel closed".into()))
            .and_then(|resp| {
                // Connect-failure errors become typed gateway errors so routes
                // can map them (e.g. unknown_agent → 404).
                if let Some(err) = &resp.error {
                    let msg = err.message.clone();
                    if err.code == proxy::ERR_AGENT_LOST && msg.starts_with("not found: ") {
                        return Err(GatewayError::NotFound(msg));
                    }
                    if err.code == proxy::ERR_AGENT_LOST {
                        return Err(GatewayError::AgentUnavailable(msg));
                    }
                }
                Ok(resp)
            })
    }

    /// Connection generation: increments on every successful (re)connect.
    pub fn generation(&self, agent_id: &str) -> u64 {
        self.conns
            .lock()
            .unwrap()
            .get(agent_id)
            .map(|c| c.generation.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    pub fn is_connected(&self, agent_id: &str) -> bool {
        self.conns
            .lock()
            .unwrap()
            .get(agent_id)
            .is_some_and(|c| c.connected.load(Ordering::SeqCst))
    }
}

enum Internal {
    Attach,
    Detach(usize),
    FromClient(usize, Envelope),
    ToAgent(Envelope),
}

enum Target {
    Client(usize),
    /// Client response must be routed back to the agent (reverse-RPC).
    Agent,
    Gateway(oneshot::Sender<Envelope>),
}

struct PendingEntry {
    target: Target,
    method: Option<String>,
    cid: Option<usize>,
}

struct AgentConn {
    id: String,
    provider: Arc<dyn AgentProvider>,
    idle_timeout: Duration,
    internal_tx: mpsc::UnboundedSender<Internal>,
    clients: Mutex<HashMap<usize, ClientSink>>,
    pending: Mutex<HashMap<String, PendingEntry>>,
    session_owner: Mutex<HashMap<String, usize>>,
    next_client_id: AtomicUsize,
    connected: AtomicBool,
    generation: std::sync::atomic::AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl AgentConn {
    fn no_clients(&self) -> bool {
        self.clients.lock().unwrap().is_empty()
    }

    fn has_gateway_pending(&self) -> bool {
        self.pending
            .lock()
            .unwrap()
            .values()
            .any(|e| matches!(e.target, Target::Gateway(_)))
    }

    fn client_sink(&self, client_id: usize) -> Option<ClientSink> {
        self.clients.lock().unwrap().get(&client_id).cloned()
    }

    fn any_client(&self) -> Option<(usize, ClientSink)> {
        self.clients
            .lock()
            .unwrap()
            .iter()
            .next()
            .map(|(id, s)| (*id, s.clone()))
    }

    /// Bookkeeping after a client envelope was successfully written to the agent.
    fn on_client_written(&self, cid: usize, env: &Envelope) {
        if env.is_response() {
            let key = proxy::id_key(env.id.as_ref().unwrap());
            match self.pending.lock().unwrap().remove(&key).map(|e| e.target) {
                Some(Target::Agent) => {}
                Some(_) => {
                    debug!(agent = %self.id, %key, "dropping duplicate/late client response")
                }
                None => warn!(agent = %self.id, %key, "client response with unknown id"),
            }
            return;
        }
        if let Some(id) = env.id.clone() {
            self.pending
                .lock()
                .unwrap()
                .entry(proxy::id_key(&id))
                .or_insert(PendingEntry {
                    target: Target::Client(cid),
                    method: env.method.clone(),
                    cid: Some(cid),
                });
        }
    }

    async fn on_agent_msg(&self, mut env: Envelope, wtr: &mut BoxWrite) {
        if env.is_response() {
            let key = proxy::id_key(env.id.as_ref().unwrap());
            let Some(entry) = self.pending.lock().unwrap().remove(&key) else {
                warn!(agent = %self.id, %key, "agent response with unknown id");
                return;
            };
            proxy::translate_error(&mut env);
            if entry.method.as_deref() == Some(method::INITIALIZE) {
                proxy::apply_capability_policy(&mut env);
            }
            if let Some(sid) = proxy::transfers_ownership(entry.method.as_deref(), &env) {
                if let Some(cid) = entry.cid {
                    self.session_owner.lock().unwrap().insert(sid, cid);
                }
            }
            match entry.target {
                Target::Client(cid) => {
                    if let Some(sink) = self.client_sink(cid) {
                        let _ = sink.send(env);
                    }
                }
                Target::Gateway(tx) => {
                    let _ = tx.send(env);
                }
                Target::Agent => {
                    warn!(agent = %self.id, %key, "agent answered its own reverse-RPC")
                }
            }
            return;
        }
        if env.is_notification() {
            let owner = proxy::owner_for(
                &self.session_owner.lock().unwrap(),
                env.session_id.as_deref(),
            );
            match owner.and_then(|cid| self.client_sink(cid)) {
                Some(sink) => {
                    let _ = sink.send(env);
                }
                None => {
                    // Unknown owner (e.g. updates after reconnect): broadcast.
                    let clients = self.clients.lock().unwrap().clone();
                    for (_, sink) in clients {
                        let _ = sink.send(env.clone());
                    }
                }
            }
            return;
        }
        // Reverse-RPC from the agent (permission / elicitation / …).
        let owner = proxy::owner_for(
            &self.session_owner.lock().unwrap(),
            env.session_id.as_deref(),
        )
        .or_else(|| self.any_client().map(|(cid, _)| cid));
        let Some(cid) = owner else {
            let id = env.id.clone().unwrap_or(Value::Null);
            let err =
                Envelope::error_response(id, proxy::ERR_REQUEST_CANCELLED, "no client attached");
            if acp_wire::write_frame(wtr, &err).await.is_err() {
                warn!(agent = %self.id, "failed to answer orphaned reverse-RPC");
            }
            return;
        };
        let Some(sink) = self.client_sink(cid) else {
            let id = env.id.clone().unwrap_or(Value::Null);
            let err =
                Envelope::error_response(id, proxy::ERR_REQUEST_CANCELLED, "owning client gone");
            if acp_wire::write_frame(wtr, &err).await.is_err() {
                warn!(agent = %self.id, "failed to answer orphaned reverse-RPC");
            }
            return;
        };
        if let Some(id) = env.id.clone() {
            self.pending.lock().unwrap().insert(
                proxy::id_key(&id),
                PendingEntry {
                    target: Target::Agent,
                    method: env.method.clone(),
                    cid: None,
                },
            );
        }
        let _ = sink.send(env);
    }

    /// Connect failure / unhealable EOF: answer everything in flight with a
    /// structured error so no client or POST caller hangs.
    fn fail_all_pending(&self, message: &str) {
        let drained: Vec<(String, PendingEntry)> = self.pending.lock().unwrap().drain().collect();
        for (key, entry) in drained {
            let Ok(id) = serde_json::from_str::<Value>(&key) else {
                continue;
            };
            let err = Envelope {
                id: Some(id),
                session_id: None,
                method: None,
                params: None,
                result: None,
                error: Some(acp_wire::RpcError {
                    code: proxy::ERR_AGENT_LOST,
                    message: format!("{message}: agent connection lost"),
                    data: None,
                }),
            };
            match entry.target {
                Target::Client(cid) => {
                    if let Some(sink) = self.client_sink(cid) {
                        let _ = sink.send(err);
                    }
                }
                Target::Gateway(tx) => {
                    let _ = tx.send(err);
                }
                Target::Agent => {}
            }
        }
    }
}

/// After a client detaches, give in-flight requests a grace period, then answer
/// the agent with `request_cancelled` if the owner never came back.
fn schedule_grace_cancels(conn: &Arc<AgentConn>, client_id: usize) {
    let stale: Vec<String> = conn
        .pending
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, e)| match &e.target {
            Target::Client(cid) => *cid == client_id,
            Target::Agent => {
                e.cid.is_none()
                    && conn
                        .session_owner
                        .lock()
                        .unwrap()
                        .values()
                        .all(|owner| *owner != client_id)
            }
            _ => false,
        })
        .map(|(k, _)| k.clone())
        .collect();
    if stale.is_empty() {
        return;
    }
    let conn = Arc::clone(conn);
    tokio::spawn(async move {
        tokio::time::sleep(REVERSE_RPC_GRACE).await;
        for key in stale {
            let removed = {
                let mut pending = conn.pending.lock().unwrap();
                match pending.get(&key) {
                    Some(PendingEntry {
                        target: Target::Client(cid),
                        ..
                    }) if *cid == client_id && conn.client_sink(*cid).is_none() => {
                        pending.remove(&key)
                    }
                    Some(PendingEntry {
                        target: Target::Agent,
                        ..
                    }) if conn.clients.lock().unwrap().is_empty() => pending.remove(&key),
                    _ => None,
                }
            };
            if removed.is_some() {
                if let Ok(id) = serde_json::from_str::<Value>(&key) {
                    let _ = conn
                        .internal_tx
                        .send(Internal::ToAgent(Envelope::error_response(
                            id,
                            proxy::ERR_REQUEST_CANCELLED,
                            "owning client disconnected",
                        )));
                }
            }
        }
    });
}

/// Connect + gateway-side `initialize`/`initialized` handshake (§5.3 healing).
async fn connect_agent(conn: &AgentConn) -> Result<(BufReader<BoxRead>, BoxWrite)> {
    let io = conn.provider.connect(&conn.id).await?;
    let (rdr, mut wtr) = io.split_io();
    let mut buf = BufReader::new(rdr);
    let init = Envelope::request(
        GATEWAY_INIT_ID,
        method::INITIALIZE,
        serde_json::json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name": "papin-gateway", "version": env!("CARGO_PKG_VERSION")}
        }),
    );
    acp_wire::write_frame(&mut wtr, &init).await?;
    loop {
        let Some(line) = acp_wire::read_frame(&mut buf).await? else {
            return Err(GatewayError::AgentUnavailable(
                "agent EOF during initialize".into(),
            ));
        };
        let Ok(env) = serde_json::from_value::<Envelope>(line) else {
            continue;
        };
        if env
            .id
            .as_ref()
            .is_some_and(|id| id.as_str() == Some(GATEWAY_INIT_ID))
        {
            if env.error.is_some() {
                return Err(GatewayError::AgentUnavailable(
                    "agent rejected gateway initialize".into(),
                ));
            }
            break;
        }
    }
    acp_wire::write_frame(
        &mut wtr,
        &Envelope::notification(method::INITIALIZED, serde_json::json!({})),
    )
    .await?;
    Ok((buf, wtr))
}

enum PumpEnd {
    Eof,
    Idle,
}

async fn conn_task(conn: Arc<AgentConn>, mut internal_rx: mpsc::UnboundedReceiver<Internal>) {
    // Start in the waiting phase: connect on first attach / first client message.
    let mut need_trigger = true;
    loop {
        if need_trigger {
            loop {
                match internal_rx.recv().await {
                    // Re-queue client messages: the pump delivers them once connected.
                    Some(Internal::FromClient(cid, env)) => {
                        let _ = conn.internal_tx.send(Internal::FromClient(cid, env));
                        break;
                    }
                    Some(Internal::Attach) => break,
                    Some(Internal::Detach(_) | Internal::ToAgent(_)) => {}
                    None => return,
                }
            }
        }
        let Some((rdr, wtr)) = connect_loop(&conn, &mut internal_rx).await else {
            // Nobody left to serve: wait for a trigger again.
            need_trigger = true;
            continue;
        };
        match pump(&conn, &mut internal_rx, rdr, wtr).await {
            PumpEnd::Eof => need_trigger = false, // clients attached: heal now
            PumpEnd::Idle => need_trigger = true, // quiescent: reconnect on next activity
        }
    }
}

/// Connect with backoff. Returns `None` when there is nobody to serve anymore.
/// Fails all pending round-trips on each failed attempt so callers never hang.
async fn connect_loop(
    conn: &Arc<AgentConn>,
    internal_rx: &mut mpsc::UnboundedReceiver<Internal>,
) -> Option<(BufReader<BoxRead>, BoxWrite)> {
    let mut attempt = 0u32;
    loop {
        match connect_agent(conn).await {
            Ok(io) => {
                conn.connected.store(true, Ordering::SeqCst);
                conn.generation.fetch_add(1, Ordering::SeqCst);
                *conn.last_error.lock().unwrap() = None;
                return Some(io);
            }
            Err(e) => {
                warn!(agent = %conn.id, error = %e, attempt, "agent connect failed");
                *conn.last_error.lock().unwrap() = Some(e.to_string());
                conn.fail_all_pending(&e.to_string());
                if conn.no_clients() && !conn.has_gateway_pending() {
                    return None;
                }
                // While backing off, service control messages; fail client
                // messages immediately (agent is down).
                tokio::select! {
                    _ = tokio::time::sleep(acp_wire::backoff(attempt)) => {}
                    msg = internal_rx.recv() => match msg {
                        Some(Internal::FromClient(cid, env)) => {
                            let id = env.id.clone().unwrap_or(Value::Null);
                            if let Some(sink) = conn.client_sink(cid) {
                                let _ = sink.send(Envelope::error_response(
                                    id,
                                    proxy::ERR_AGENT_LOST,
                                    format!("agent unavailable: {e}"),
                                ));
                            }
                        }
                        Some(_) => {}
                        None => return None,
                    }
                }
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

async fn pump(
    conn: &Arc<AgentConn>,
    internal_rx: &mut mpsc::UnboundedReceiver<Internal>,
    rdr: BufReader<BoxRead>,
    wtr: BoxWrite,
) -> PumpEnd {
    let mut rdr = rdr;
    let mut wtr = wtr;
    // Message that failed to write (agent died mid-send): re-queued on exit.
    let mut requeue: Option<(usize, Envelope)> = None;
    let end = loop {
        // The idle arm is re-created every iteration: any line (agent or
        // client) resets the idle deadline (§5.3).
        let idle_enabled = !conn.idle_timeout.is_zero();
        let deadline = tokio::time::Instant::now() + conn.idle_timeout;
        tokio::select! {
            msg = internal_rx.recv() => match msg {
                None => break PumpEnd::Eof,
                Some(Internal::Attach) => {}
                Some(Internal::Detach(cid)) => schedule_grace_cancels(conn, cid),
                Some(Internal::FromClient(cid, env)) => {
                    match acp_wire::write_frame(&mut wtr, &env).await {
                        Ok(()) => conn.on_client_written(cid, &env),
                        Err(err) => {
                            debug!(agent = %conn.id, %err, "agent write failed; healing");
                            requeue = Some((cid, env));
                            break PumpEnd::Eof;
                        }
                    }
                }
                Some(Internal::ToAgent(env)) => {
                    let _ = acp_wire::write_frame(&mut wtr, &env).await;
                }
            },
            line = acp_wire::read_frame(&mut rdr) => match line {
                Ok(Some(value)) => match serde_json::from_value::<Envelope>(value) {
                    Ok(env) => conn.on_agent_msg(env, &mut wtr).await,
                    Err(err) => warn!(agent = %conn.id, ?err, "malformed agent frame"),
                },
                Ok(None) => break PumpEnd::Eof,
                Err(err) => {
                    debug!(agent = %conn.id, %err, "agent stream ended");
                    break PumpEnd::Eof;
                }
            },
            _ = tokio::time::sleep_until(deadline), if idle_enabled => {
                debug!(agent = %conn.id, "idle timeout; dropping agent connection");
                break PumpEnd::Idle;
            }
        }
    };
    drop(wtr);
    if let Some((cid, env)) = requeue {
        let _ = conn.internal_tx.send(Internal::FromClient(cid, env));
    }
    conn.connected.store(false, Ordering::SeqCst);
    end
}
