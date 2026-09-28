use acp_wire::Envelope;
use papin_gateway::config::{now_secs, Catalog, CatalogEnv, CatalogItem, TokenEntry, TokenStore};
use papin_gateway::provider::{AgentProvider, AgentSpec, FakeProvider};
use papin_gateway::{AppState, ConnectionManager};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

pub const TOKEN: &str = "test-device-token";
pub const AGENT: &str = "test-agent";

/// Path to scripts/fake-papin-acp in the repo root.
pub fn fake_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("scripts/fake-papin-acp")
        .canonicalize()
        .expect("fake script exists")
}

/// Catalog fixture: one base, one seed, one env key (matches config {"base": "fake"}).
pub fn test_catalog() -> Catalog {
    Catalog {
        bases: vec![CatalogItem {
            name: "fake".into(),
            description: "fake base".into(),
        }],
        seeds: vec![CatalogItem {
            name: "demo".into(),
            description: "demo workspace".into(),
        }],
        env: vec![CatalogEnv {
            key: "PAPIN_FAKE_ENV".into(),
            default: "1".into(),
        }],
    }
}

/// State dir layout for a full gateway (registry + rootfs + seeds).
pub struct TestDirs {
    pub _tmp: TempDir,
    pub registry_dir: PathBuf,
    pub agents_root: PathBuf,
    pub seeds_dir: PathBuf,
}

pub fn test_dirs() -> TestDirs {
    let tmp = TempDir::new().expect("tempdir");
    let state = tmp.path().join("state");
    let seeds_dir = tmp.path().join("workspaces");
    std::fs::create_dir_all(&seeds_dir).unwrap();
    // demo seed content
    std::fs::write(seeds_dir.join("README.demo"), "demo seed\n").unwrap();
    std::fs::create_dir_all(seeds_dir.join("demo/src")).unwrap();
    std::fs::write(seeds_dir.join("demo/src/main.txt"), "hello seed\n").unwrap();
    TestDirs {
        _tmp: tmp,
        registry_dir: state.join("agents"),
        agents_root: state.join("agents"),
        seeds_dir,
    }
}

/// Full gateway state (registry REST + bootstrap) over a temp dir.
pub fn gateway_state(idle_timeout: Duration) -> (Arc<AppState>, TestDirs) {
    let dirs = test_dirs();
    let catalog = test_catalog();
    let provider = FakeProvider::new(dirs.registry_dir.clone(), fake_script(), catalog.clone());
    let bootstrap =
        papin_gateway::FakeBootstrap::new(dirs.agents_root.clone(), dirs.seeds_dir.clone());
    let st = papin_gateway::server::make_state(
        Arc::new(provider),
        Arc::new(bootstrap),
        catalog,
        idle_timeout,
        token_store(),
    );
    (st, dirs)
}

/// FakeProvider with a registry in a temp dir and pre-created agents.
pub async fn provider_with(ids: &[&str]) -> (FakeProvider, TempDir) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "papin_gateway=debug".into()),
        )
        .with_test_writer()
        .try_init();
    let dir = TempDir::new().expect("tempdir");
    let provider = FakeProvider::new(
        dir.path().join("state/agents"),
        fake_script(),
        test_catalog(),
    );
    for id in ids {
        provider
            .create(AgentSpec {
                id: Some(id.to_string()),
                name: None,
                config: papin_gateway::AgentConfig {
                    base: "fake".into(),
                    seed: None,
                    env: Default::default(),
                },
            })
            .await
            .expect("create agent");
    }
    (provider, dir)
}

pub fn token_store() -> Arc<TokenStore> {
    let now = now_secs();
    Arc::new(TokenStore::from_entries(
        vec![TokenEntry {
            token: TOKEN.into(),
            created_at: now,
            expires_at: Some(now + 3600),
            assigned_ip: None,
            pubkey: None,
            name: None,
        }],
        Duration::from_secs(3600),
    ))
}

/// Build an AppState over an existing provider (keep the provider's TempDir alive).
pub fn state(provider: FakeProvider, idle_timeout: Duration) -> Arc<AppState> {
    papin_gateway::server::fake_state(Arc::new(provider), idle_timeout, token_store())
}

pub fn initialize_request(id: i64) -> Envelope {
    Envelope::request(
        id,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": true, "writeTextFile": true}, "terminal": true},
            "clientInfo": {"name": "test-client", "version": "0.0.0"}
        }),
    )
}

pub fn prompt_request(id: i64, session_id: &str, text: &str) -> Envelope {
    let mut env = Envelope::request(
        id,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": text}]
        }),
    );
    env.session_id = Some(session_id.to_string());
    env
}

pub fn updates_of(env: &Envelope) -> Vec<&str> {
    if env.method.as_deref() != Some("session/update") {
        return vec![];
    }
    env.params
        .as_ref()
        .and_then(|p| p.get("update"))
        .and_then(|u| u.get("sessionUpdate"))
        .and_then(Value::as_str)
        .into_iter()
        .collect()
}

/// Receive envelopes from an mpsc attachment with a real-time timeout.
pub async fn recv_attach(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>) -> Envelope {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("receive within 10s")
        .expect("attachment stream open")
}

/// Receive the next envelope on a WebSocket with a real-time timeout.
pub async fn recv_ws(stream: &mut acp_wire::WsStream) -> Envelope {
    tokio::time::timeout(Duration::from_secs(10), acp_wire::ws_recv_envelope(stream))
        .await
        .expect("ws recv within 10s")
        .expect("ws stream open")
}

pub async fn send_ws(stream: &mut acp_wire::WsStream, env: &Envelope) {
    acp_wire::ws_send_envelope(stream, env)
        .await
        .expect("ws send");
}

pub async fn send_attach(
    manager: &ConnectionManager,
    agent: &str,
    client_id: usize,
    env: Envelope,
) {
    manager
        .send_from_client(agent, client_id, env)
        .expect("send");
}

/// Poll until the agent connection is up (drives paused tokio time forward).
pub async fn wait_connected(manager: &ConnectionManager, agent: &str) {
    for _ in 0..500 {
        if manager.is_connected(agent) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        tokio::task::yield_now().await;
    }
    panic!("agent {agent} did not connect");
}

/// Poll until the agent connection is down (drives paused tokio time forward).
pub async fn wait_disconnected(manager: &ConnectionManager, agent: &str) {
    for _ in 0..500 {
        if !manager.is_connected(agent) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        tokio::task::yield_now().await;
    }
    panic!("agent {agent} did not disconnect");
}

/// Yield-spin waits for paused-time tests: no virtual timers are created, so
/// the runtime never auto-advances the clock; it parks in epoll and real time
/// passes while the child process starts. Only safe while no virtual timer
/// (e.g. the idle deadline) is pending — use for connect-phase waits.
async fn wait_spin(cond: impl Fn() -> bool, what: &str, agent: &str) {
    for _ in 0..2_000_000 {
        if cond() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("agent {agent} {what}");
}

pub async fn wait_connected_real(manager: &ConnectionManager, agent: &str) {
    wait_spin(|| manager.is_connected(agent), "did not connect", agent).await;
}

pub async fn wait_disconnected_real(manager: &ConnectionManager, agent: &str) {
    wait_spin(|| !manager.is_connected(agent), "did not disconnect", agent).await;
}
