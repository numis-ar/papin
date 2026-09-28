use crate::config::{AgentConfig, Catalog};
use crate::error::{GatewayError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::Command;

/// Byte stream to an ACP agent process (stdio NDJSON).
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Send + Unpin {
    /// Split into owned read/write halves (type-erased).
    fn split_io(self: Box<Self>) -> (BoxRead, BoxWrite);
}

pub type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
pub type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub name: String,
    pub config: AgentConfig,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentSpec {
    pub id: Option<String>,
    pub name: Option<String>,
    pub config: AgentConfig,
}

#[async_trait]
pub trait AgentProvider: Send + Sync {
    /// Metadata dir listing.
    async fn list(&self) -> Result<Vec<AgentRecord>>;
    /// Validate vs catalog; write metadata. No process starts.
    async fn create(&self, spec: AgentSpec) -> Result<AgentRecord>;
    /// Metadata + rootfs removal (pure filesystem).
    async fn remove(&self, id: &str, purge: bool) -> Result<()>;
    /// ACP byte stream to the agent (spawning it if needed).
    async fn connect(&self, id: &str) -> Result<Box<dyn AsyncReadWrite>>;
}

pub fn validate_agent_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    id.len() <= 64 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Metadata-dir registry shared by all providers: `<dir>/<id>.json`.
/// The registry is the only database (PLAN §3).
pub struct RegistryDir {
    pub dir: PathBuf,
}

impl RegistryDir {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        RegistryDir { dir: dir.into() }
    }

    pub fn record_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn read(&self, id: &str) -> Result<Option<AgentRecord>> {
        match std::fs::read_to_string(self.record_path(id)) {
            Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list(&self) -> Result<Vec<AgentRecord>> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            if entry.path().extension().is_some_and(|e| e == "json") {
                let text = std::fs::read_to_string(entry.path())?;
                out.push(serde_json::from_str(&text)?);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Validate strictly against the catalog and write the metadata record.
    pub fn create(&self, spec: AgentSpec, catalog: &Catalog) -> Result<AgentRecord> {
        let id = spec.id.unwrap_or_else(|| format!("agent-{}", uuidish()));
        if !validate_agent_id(&id) {
            return Err(GatewayError::InvalidInput(format!(
                "invalid agent id {id:?}: must match ^[a-z0-9][a-z0-9-]{{0,63}}$"
            )));
        }
        let config = spec.config;
        if !catalog.has_base(&config.base) {
            return Err(GatewayError::InvalidInput(format!(
                "unknown base image {:?}: choose from GET /api/v1/config-catalog",
                config.base
            )));
        }
        if let Some(seed) = &config.seed {
            if !catalog.has_seed(seed) {
                return Err(GatewayError::InvalidInput(format!(
                    "unknown workspace seed {seed:?}: choose from GET /api/v1/config-catalog"
                )));
            }
        }
        for key in config.env.keys() {
            if !catalog.allows_env_key(key) {
                return Err(GatewayError::InvalidInput(format!(
                    "env key {key:?} is not allowed: choose from GET /api/v1/config-catalog"
                )));
            }
        }
        if self.read(&id)?.is_some() {
            return Err(GatewayError::AlreadyExists(format!("agent {id}")));
        }
        std::fs::create_dir_all(&self.dir)?;
        let record = AgentRecord {
            id: id.clone(),
            name: spec.name.unwrap_or_else(|| id.clone()),
            config,
            created_at: crate::config::now_secs(),
        };
        // Write via temp file + rename so readers never see a partial record.
        let tmp = self.dir.join(format!(".{id}.json.tmp"));
        std::fs::write(&tmp, serde_json::to_string_pretty(&record)?)?;
        std::fs::rename(&tmp, self.record_path(&id))?;
        Ok(record)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        match std::fs::remove_file(self.record_path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(GatewayError::NotFound(format!("agent {id}")))
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Dev/test provider: registry in a plain directory, `connect()` spawns
/// `scripts/fake-papin-acp` on a tokio duplex pair. No bootstrap handshake —
/// instantly ready. No systemd, no root.
pub struct FakeProvider {
    pub registry: RegistryDir,
    pub script: PathBuf,
    pub catalog: Catalog,
}

impl FakeProvider {
    pub fn new(
        registry_dir: impl Into<PathBuf>,
        script: impl Into<PathBuf>,
        catalog: Catalog,
    ) -> Self {
        FakeProvider {
            registry: RegistryDir::new(registry_dir),
            script: script.into(),
            catalog,
        }
    }

    /// Compatibility accessor: registry metadata dir.
    pub fn state_dir(&self) -> &Path {
        self.registry
            .dir
            .parent()
            .unwrap_or(self.registry.dir.as_path())
    }
}

#[async_trait]
impl AgentProvider for FakeProvider {
    async fn list(&self) -> Result<Vec<AgentRecord>> {
        self.registry.list()
    }

    async fn create(&self, spec: AgentSpec) -> Result<AgentRecord> {
        self.registry.create(spec, &self.catalog)
    }

    async fn remove(&self, id: &str, purge: bool) -> Result<()> {
        let _ = purge;
        self.registry.remove(id)
    }

    async fn connect(&self, id: &str) -> Result<Box<dyn AsyncReadWrite>> {
        if self.registry.read(id)?.is_none() {
            return Err(GatewayError::NotFound(format!(
                "unknown_agent: {id} (no registry record)"
            )));
        }
        let (client, server) = tokio::io::duplex(64 * 1024);
        let mut child = {
            let mut cmd = Command::new(&self.script);
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            cmd.spawn()?
        };
        let mut child_in = child.stdin.take().unwrap();
        let mut child_out = child.stdout.take().unwrap();
        let child = Arc::new(child);
        {
            let child = Arc::clone(&child);
            tokio::spawn(async move {
                let _guard = child; // keep alive (kill_on_drop) until a direction ends
                let mut server = server;
                let mut to_child = vec![0u8; 8192];
                let mut from_child = vec![0u8; 8192];
                loop {
                    tokio::select! {
                        n = server.read(&mut to_child) => {
                            let n = match n { Ok(0) | Err(_) => break, Ok(n) => n };
                            if child_in.write_all(&to_child[..n]).await.is_err() {
                                break;
                            }
                        }
                        n = child_out.read(&mut from_child) => {
                            let n = match n { Ok(0) | Err(_) => break, Ok(n) => n };
                            if server.write_all(&from_child[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                // Dropping `server` propagates EOF/close to the gateway side.
            });
        }
        Ok(Box::new(DuplexChild { stream: client }))
    }
}

/// Bootstrap handshake error reasons (§5.2) mapped to client-facing errors.
#[derive(Debug, Clone, PartialEq)]
pub enum BootstrapStatus {
    Ready,
    UnknownAgent,
    RootfsNotReady,
    ExecFailed,
    Malformed,
}

impl BootstrapStatus {
    pub fn from_line(line: &str) -> BootstrapStatus {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return BootstrapStatus::Malformed;
        };
        match v.get("status").and_then(|s| s.as_str()) {
            Some("ready") => BootstrapStatus::Ready,
            Some("error") => match v.get("reason").and_then(|r| r.as_str()) {
                Some("unknown_agent") => BootstrapStatus::UnknownAgent,
                Some("rootfs_not_ready") => BootstrapStatus::RootfsNotReady,
                Some("exec_failed") => BootstrapStatus::ExecFailed,
                _ => BootstrapStatus::Malformed,
            },
            _ => BootstrapStatus::Malformed,
        }
    }
}

/// Production provider (PLAN §5.2): `connect()` = UnixStream to the
/// socket-activation socket + one-line bootstrap handshake, then raw NDJSON
/// ACP. The chroot wrapper itself is M4; the handshake is testable now.
pub struct SpawnProvider {
    pub registry: RegistryDir,
    pub socket_path: PathBuf,
    pub catalog: Catalog,
}

impl SpawnProvider {
    pub fn new(
        registry_dir: impl Into<PathBuf>,
        socket_path: impl Into<PathBuf>,
        catalog: Catalog,
    ) -> Self {
        SpawnProvider {
            registry: RegistryDir::new(registry_dir),
            socket_path: socket_path.into(),
            catalog,
        }
    }
}

#[async_trait]
impl AgentProvider for SpawnProvider {
    async fn list(&self) -> Result<Vec<AgentRecord>> {
        self.registry.list()
    }

    async fn create(&self, spec: AgentSpec) -> Result<AgentRecord> {
        self.registry.create(spec, &self.catalog)
    }

    async fn remove(&self, id: &str, purge: bool) -> Result<()> {
        let _ = purge;
        self.registry.remove(id)
    }

    async fn connect(&self, id: &str) -> Result<Box<dyn AsyncReadWrite>> {
        if self.registry.read(id)?.is_none() {
            return Err(GatewayError::NotFound(format!(
                "unknown_agent: {id} (no registry record)"
            )));
        }
        let mut stream = tokio::net::UnixStream::connect(&self.socket_path).await?;
        let hello = format!("{{\"agent\":\"{id}\"}}\n");
        stream.write_all(hello.as_bytes()).await?;
        let mut line = String::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await?;
            if n == 0 {
                return Err(GatewayError::AgentUnavailable(
                    "wrapper closed socket during bootstrap".into(),
                ));
            }
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0] as char);
        }
        match BootstrapStatus::from_line(&line) {
            BootstrapStatus::Ready => Ok(Box::new(stream)),
            BootstrapStatus::UnknownAgent => Err(GatewayError::NotFound(format!(
                "unknown_agent: {id} (wrapper rejected bootstrap)"
            ))),
            BootstrapStatus::RootfsNotReady => Err(GatewayError::AgentUnavailable(
                "rootfs_not_ready: agent filesystem is not built yet".into(),
            )),
            BootstrapStatus::ExecFailed => Err(GatewayError::AgentUnavailable(
                "exec_failed: wrapper could not exec kimi acp".into(),
            )),
            BootstrapStatus::Malformed => Err(GatewayError::AgentUnavailable(format!(
                "malformed bootstrap response: {line:?}"
            ))),
        }
    }
}

impl AsyncReadWrite for tokio::net::UnixStream {
    fn split_io(self: Box<Self>) -> (BoxRead, BoxWrite) {
        let (r, w) = tokio::io::split(*self);
        (Box::new(r), Box::new(w))
    }
}

/// Duplex stream to the child. The child process is kept alive by the stdio
/// bridge tasks (each holds an `Arc<Child>` with `kill_on_drop` set); when the
/// gateway side drops, the bridges' copies fail and the last `Arc` drop kills it.
struct DuplexChild {
    stream: tokio::io::DuplexStream,
}

impl AsyncRead for DuplexChild {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for DuplexChild {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

impl AsyncReadWrite for DuplexChild {
    fn split_io(self: Box<Self>) -> (BoxRead, BoxWrite) {
        let this = *self;
        let (r, w) = tokio::io::split(this.stream);
        (Box::new(r), Box::new(w))
    }
}

fn uuidish() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos % 0xffff_ffff)
}
