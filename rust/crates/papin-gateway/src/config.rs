use crate::error::{GatewayError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GatewayConfig {
    /// "fake" (dev/tests) or "spawn" (socket activation; M4 units).
    pub provider: String,
    /// Registry/metadata dir (`agents/<id>.json` records).
    pub state_dir: PathBuf,
    /// Agent rootfs trees live under `state_dir/agents/<id>/root`.
    pub agents_root: PathBuf,
    /// Server-local workspace seed template dirs (catalog).
    pub seeds_dir: PathBuf,
    /// Path to the fake agent script spawned by FakeProvider::connect().
    pub fake_script: PathBuf,
    /// systemd socket-activation socket for SpawnProvider::connect().
    pub socket_path: PathBuf,
    /// Seconds of agent-stream quiescence before disconnect. 0 = never.
    pub idle_timeout_secs: u64,
    /// Default device-token lifetime, applied to tokens without expires_at.
    pub token_ttl_secs: u64,
    pub tokens_file: PathBuf,
    /// HTTP listen address.
    pub listen: String,
    /// Catalog base images (`rootfs/base/<name>`) cloned at bootstrap.
    pub bases_dir: PathBuf,
    /// What clients may select: the security boundary on agent creation.
    pub catalog: Catalog,
    /// WireGuard enrollment (§7.1).
    pub wg_peers_dir: PathBuf,
    pub wg_gateway_ip: String,
    pub wg_client_pool: String,
    pub wg_endpoint: String,
    /// setuid-root enrollment helper binary.
    pub enroll_helper: PathBuf,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            provider: "fake".into(),
            state_dir: "/var/lib/papin-gateway".into(),
            agents_root: "/var/lib/papin/agents".into(),
            seeds_dir: "/var/lib/papin/workspaces".into(),
            fake_script: "scripts/fake-papin-acp".into(),
            socket_path: "/run/papin/agent.sock".into(),
            idle_timeout_secs: 600,
            token_ttl_secs: 30 * 24 * 3600,
            tokens_file: "/var/lib/papin/tokens.toml".into(),
            listen: "127.0.0.1:8080".into(),
            bases_dir: "/var/lib/papin/rootfs/base".into(),
            catalog: Catalog::default(),
            wg_peers_dir: "/run/papin/wg-peers".into(),
            wg_gateway_ip: "10.77.0.1".into(),
            wg_client_pool: "10.77.0.0/24".into(),
            wg_endpoint: "vpn.example.com:51820".into(),
            enroll_helper: "/usr/local/lib/papin/papin-enroll-helper".into(),
        }
    }
}

impl GatewayConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_secs)
    }
}

/// Client-selectable creation options (PLAN §5.1). Clients choose from the
/// catalog only — the catalog is the security boundary on agent creation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    /// Base images: `rootfs/base/<name>` on the real deployment.
    pub bases: Vec<CatalogItem>,
    /// Workspace seed templates: server-local dirs under `seeds_dir`.
    pub seeds: Vec<CatalogItem>,
    /// Allowed env keys with defaults (v1: from catalog, not from clients).
    pub env: Vec<CatalogEnv>,
}

impl Catalog {
    pub fn has_base(&self, name: &str) -> bool {
        self.bases.iter().any(|b| b.name == name)
    }

    pub fn has_seed(&self, name: &str) -> bool {
        self.seeds.iter().any(|s| s.name == name)
    }

    pub fn env_default(&self, key: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.default.as_str())
    }

    pub fn allows_env_key(&self, key: &str) -> bool {
        self.env.iter().any(|e| e.key == key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogItem {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEnv {
    pub key: String,
    #[serde(default)]
    pub default: String,
}

/// Agent creation spec, validated strictly against the catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub base: String,
    #[serde(default)]
    pub seed: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenEntry {
    pub token: String,
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// WireGuard enrollment binding (§7.1); re-keying overwrites the pubkey.
    #[serde(default)]
    pub assigned_ip: Option<String>,
    #[serde(default)]
    pub pubkey: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TokensFile {
    #[serde(default)]
    tokens: Vec<TokenEntry>,
}

/// Device-token store backing the bearer auth middleware and enrollment.
pub struct TokenStore {
    inner: RwLock<HashMap<String, TokenEntry>>, // expires_at resolved
    path: Option<PathBuf>,
}

impl TokenStore {
    pub fn load(path: &Path, ttl: Duration) -> Result<Self> {
        let file: TokensFile = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TokensFile::default(),
            Err(e) => return Err(GatewayError::Io(e)),
        };
        Ok(Self {
            inner: RwLock::new(Self::resolve(file.tokens, ttl)),
            path: Some(path.to_path_buf()),
        })
    }

    /// Empty in-memory store (tests).
    pub fn default_entries() -> Self {
        Self::from_entries(Vec::new(), Duration::ZERO)
    }

    pub fn from_entries(entries: Vec<TokenEntry>, ttl: Duration) -> Self {
        TokenStore {
            inner: RwLock::new(Self::resolve(entries, ttl)),
            path: None,
        }
    }

    fn resolve(entries: Vec<TokenEntry>, ttl: Duration) -> HashMap<String, TokenEntry> {
        entries
            .into_iter()
            .map(|mut e| {
                e.expires_at = Some(
                    e.expires_at
                        .unwrap_or(e.created_at.saturating_add(ttl.as_secs())),
                );
                (e.token.clone(), e)
            })
            .collect()
    }

    pub fn validate(&self, token: &str) -> bool {
        let inner = self.inner.read().unwrap();
        match inner.get(token) {
            Some(e) => e.expires_at.is_some_and(|exp| exp > now_secs()),
            None => false,
        }
    }

    pub fn entry(&self, token: &str) -> Option<TokenEntry> {
        self.inner.read().unwrap().get(token).cloned()
    }

    /// Bind a WireGuard pubkey to the token (overwrite = re-keying, §7.1).
    pub fn bind(&self, token: &str, pubkey: &str, assigned_ip: &str) -> Result<()> {
        {
            let mut inner = self.inner.write().unwrap();
            let Some(entry) = inner.get_mut(token) else {
                return Err(GatewayError::NotFound(format!("token {token}")));
            };
            entry.pubkey = Some(pubkey.to_string());
            entry.assigned_ip = Some(assigned_ip.to_string());
        }
        self.save()
    }

    /// Lowest free host address in the pool (excludes network, gateway, used).
    pub fn allocate_ip(&self, pool: &str, gateway_ip: &str) -> Option<std::net::IpAddr> {
        let pool = ipnet_fallback(pool)?;
        let gateway: std::net::IpAddr = gateway_ip.parse().ok()?;
        let used: Vec<std::net::IpAddr> = self
            .inner
            .read()
            .unwrap()
            .values()
            .filter_map(|e| e.assigned_ip.as_deref()?.parse().ok())
            .collect();
        pool.hosts().find(|ip| *ip != gateway && !used.contains(ip))
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(()); // in-memory store (tests)
        };
        let file = TokensFile {
            tokens: self.inner.read().unwrap().values().cloned().collect(),
        };
        let text = toml::to_string_pretty(&file)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Minimal CIDR host iterator (no extra deps): `addr/prefix` over IPv4/IPv6.
fn ipnet_fallback(cidr: &str) -> Option<Pool> {
    let (addr, prefix) = cidr.split_once('/')?;
    let addr: std::net::IpAddr = addr.parse().ok()?;
    let prefix: u8 = prefix.parse().ok()?;
    match (addr, prefix) {
        (std::net::IpAddr::V4(a), p) if p <= 32 => Some(Pool::V4(u32::from(a), p)),
        (std::net::IpAddr::V6(a), p) if p <= 128 => Some(Pool::V6(u128::from(a), p)),
        _ => None,
    }
}

enum Pool {
    V4(u32, u8),
    V6(u128, u8),
}

impl Pool {
    fn hosts(&self) -> PoolHosts {
        match *self {
            Pool::V4(base, p) => {
                let mask = if p == 0 { 0 } else { u32::MAX << (32 - p) };
                let first = (base & mask).saturating_add(1);
                let last = (base | !mask).saturating_sub(1);
                PoolHosts::V4(first, last)
            }
            Pool::V6(base, p) => {
                let mask = if p == 0 { 0 } else { u128::MAX << (128 - p) };
                let first = (base & mask).saturating_add(1);
                let last = (base | !mask).saturating_sub(1);
                PoolHosts::V6(first, last)
            }
        }
    }
}

enum PoolHosts {
    V4(u32, u32),
    V6(u128, u128),
}

impl Iterator for PoolHosts {
    type Item = std::net::IpAddr;
    fn next(&mut self) -> Option<Self::Item> {
        match *self {
            PoolHosts::V4(ref mut cur, last) if *cur <= last => {
                let ip = std::net::Ipv4Addr::from(*cur);
                *cur += 1;
                Some(ip.into())
            }
            PoolHosts::V6(ref mut cur, last) if *cur <= last => {
                let ip = std::net::Ipv6Addr::from(*cur);
                *cur += 1;
                Some(ip.into())
            }
            _ => None,
        }
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub type SharedTokenStore = Arc<TokenStore>;
