use crate::error::Result;
use crate::provider::AgentRecord;
use async_trait::async_trait;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Rootfs bootstrap engines (PLAN §5.4). `FakeBootstrap` is the minimal
/// dev/test stand-in; `SpawnBootstrap` clones a catalog base image via
/// FICLONE reflink (best-effort, silent copy fallback, never hardlink),
/// applies ACLs/setgid, refreshes identity files, and drops the ready marker
/// last.
#[async_trait]
pub trait BootstrapEngine: Send + Sync {
    /// Build `agents/<id>/root` for a fresh record. Idempotent per record.
    async fn bootstrap(&self, record: &AgentRecord) -> Result<PathBuf>;
    /// Remove the agent's rootfs tree.
    async fn remove(&self, id: &str) -> Result<()>;
}

pub const READY_MARKER: &str = ".papin-rootfs-ready";

const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\npapin:x:1000:1000:papin:/home/papin:/bin/sh\n";
const GROUP: &str = "root:x:0:\npapin:x:1000:\n";
const NSSWITCH: &str = "passwd: files\ngroup: files\nhosts: files dns\n";
const HOSTS: &str = "127.0.0.1 localhost\n::1 localhost ip6-localhost ip6-loopback\n";
const RESOLV: &str = "# refreshed from the host at bootstrap\nnameserver 1.1.1.1\n";

pub struct FakeBootstrap {
    /// `agents/` dir: `<agents_root>/<id>/root` trees + parent of registry.
    pub agents_root: PathBuf,
    /// Server-local seed template dirs (`<seeds_dir>/<seed>`).
    pub seeds_dir: PathBuf,
}

impl FakeBootstrap {
    pub fn new(agents_root: impl Into<PathBuf>, seeds_dir: impl Into<PathBuf>) -> Self {
        FakeBootstrap {
            agents_root: agents_root.into(),
            seeds_dir: seeds_dir.into(),
        }
    }

    fn root_dir(&self, id: &str) -> PathBuf {
        self.agents_root.join(id).join("root")
    }
}

#[async_trait]
impl BootstrapEngine for FakeBootstrap {
    async fn bootstrap(&self, record: &AgentRecord) -> Result<PathBuf> {
        let root = self.root_dir(&record.id);
        if root.join(READY_MARKER).exists() {
            return Ok(root); // idempotent
        }
        prepare_layout(&root)?;
        if let Some(seed) = &record.config.seed {
            copy_dir_all(&self.seeds_dir.join(seed), &root.join("workspace"))?;
        }
        write_identity_files(&root)?;
        drop_ready_marker(&root, &record.id)?;
        Ok(root)
    }

    async fn remove(&self, id: &str) -> Result<()> {
        remove_tree(&self.agents_root.join(id))
    }
}

/// Shared layout (§5.4): writable dirs, modes, ACL+setgid, marker last.
fn prepare_layout(root: &Path) -> Result<()> {
    let home = root.join("home/papin");
    let workspace = root.join("workspace");
    let tmp = root.join("tmp");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&tmp)?;
    std::fs::set_permissions(&home, PermissionsExt::from_mode(0o700))?;
    std::fs::set_permissions(&tmp, PermissionsExt::from_mode(0o1777))?;
    apply_acl_and_setgid(&home);
    apply_acl_and_setgid(&workspace);
    Ok(())
}

/// Best-effort default ACL + setgid (§5.4). ACLs need the `setfacl` tool and
/// a filesystem with xattr support; both are probed and skipped silently,
/// mirroring the reflink best-effort rule.
fn apply_acl_and_setgid(dir: &Path) {
    use std::os::unix::fs::MetadataExt;
    let setgid = std::fs::metadata(dir)
        .map(|m| PermissionsExt::from_mode(m.mode() | 0o2000))
        .and_then(|p| std::fs::set_permissions(dir, p));
    if setgid.is_err() {
        tracing::warn!(?dir, "could not set setgid bit");
    }
    let has_setfacl = std::process::Command::new("which")
        .arg("setfacl")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_setfacl {
        let _ = std::process::Command::new("setfacl")
            .args(["-d", "-m", "group:papin-gateway:rwx"])
            .arg(dir)
            .status();
    }
}

/// Identity files from templates (§5.4), refreshed from the host.
fn write_identity_files(root: &Path) -> Result<()> {
    let etc = root.join("etc");
    std::fs::create_dir_all(&etc)?;
    for (name, contents) in [
        ("passwd", PASSWD),
        ("group", GROUP),
        ("nsswitch.conf", NSSWITCH),
        ("hosts", HOSTS),
        ("resolv.conf", RESOLV),
    ] {
        std::fs::write(etc.join(name), contents)?;
    }
    Ok(())
}

/// Ready marker, dropped last via atomic rename (§5.4).
fn drop_ready_marker(root: &Path, id: &str) -> Result<()> {
    let marker_tmp = root.join(format!(".{READY_MARKER}.tmp"));
    std::fs::write(&marker_tmp, format!("{id}\n"))?;
    std::fs::rename(&marker_tmp, root.join(READY_MARKER))?;
    Ok(())
}

fn remove_tree(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Real bootstrap engine (M4): clone a catalog base image into
/// `agents/<id>/root` (reflink where the fs supports it, full copy
/// otherwise — never hardlink), then the shared layout/identity/marker.
pub struct SpawnBootstrap {
    pub agents_root: PathBuf,
    pub bases_dir: PathBuf,
    pub seeds_dir: PathBuf,
    /// Test override for the reflink probe: None = probe per clone.
    pub use_reflink: Option<bool>,
}

/// linux/fs.h FICLONE.
const FICLONE: libc::c_ulong = 0x4004_9409;

#[derive(Clone, Copy, PartialEq)]
enum CloneMode {
    Probe,
    Reflink,
    Copy,
}

impl SpawnBootstrap {
    pub fn new(
        agents_root: impl Into<PathBuf>,
        bases_dir: impl Into<PathBuf>,
        seeds_dir: impl Into<PathBuf>,
    ) -> Self {
        SpawnBootstrap {
            agents_root: agents_root.into(),
            bases_dir: bases_dir.into(),
            seeds_dir: seeds_dir.into(),
            use_reflink: None,
        }
    }

    fn root_dir(&self, id: &str) -> PathBuf {
        self.agents_root.join(id).join("root")
    }

    fn clone_dir(&self, src: &Path, dst: &Path) -> Result<()> {
        std::fs::create_dir_all(dst)?;
        let mut mode = match self.use_reflink {
            Some(true) => CloneMode::Reflink,
            Some(false) => CloneMode::Copy,
            None => CloneMode::Probe,
        };
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let from = entry.path();
            let to = dst.join(entry.file_name());
            let meta = entry.metadata()?;
            if meta.is_dir() {
                self.clone_dir(&from, &to)?;
            } else if mode == CloneMode::Probe {
                // Probe FICLONE once per clone; fall back silently.
                mode = match reflink_file(&from, &to) {
                    Ok(()) => CloneMode::Reflink,
                    Err(e) => {
                        tracing::debug!(?from, %e, "reflink unsupported; copying");
                        CloneMode::Copy
                    }
                };
                if mode == CloneMode::Copy {
                    copy_file(&from, &to)?;
                }
            } else if mode == CloneMode::Reflink {
                match reflink_file(&from, &to) {
                    Ok(()) => {}
                    // A mid-clone per-file failure (e.g. xattr-limited
                    // file) falls back for that file only.
                    Err(_) => copy_file(&from, &to)?,
                }
            } else {
                copy_file(&from, &to)?;
            }
        }
        Ok(())
    }
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to)?;
    Ok(())
}

/// FICLONE reflink of a single file (COW clone — shares extents but never
/// mutates the source on write; distinct from a hardlink).
fn reflink_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let src = std::fs::File::open(from)?;
    let mode = src.metadata()?.mode();
    let dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(to)?;
    let rc = unsafe { libc::ioctl(dst.as_raw_fd(), FICLONE, src.as_raw_fd()) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        let _ = std::fs::remove_file(to);
        return Err(err);
    }
    let _ = from;
    Ok(())
}

#[async_trait]
impl BootstrapEngine for SpawnBootstrap {
    async fn bootstrap(&self, record: &AgentRecord) -> Result<PathBuf> {
        let root = self.root_dir(&record.id);
        if root.join(READY_MARKER).exists() {
            return Ok(root); // idempotent
        }
        let base = self.bases_dir.join(&record.config.base);
        if !base.is_dir() {
            return Err(crate::error::GatewayError::NotFound(format!(
                "base image {:?} not found under {} (build one: papin-gateway mkbase)",
                record.config.base,
                self.bases_dir.display()
            )));
        }
        self.clone_dir(&base, &root)?;
        prepare_layout(&root)?;
        if let Some(seed) = &record.config.seed {
            copy_dir_all(&self.seeds_dir.join(seed), &root.join("workspace"))?;
        }
        write_identity_files(&root)?;
        drop_ready_marker(&root, &record.id)?;
        Ok(root)
    }

    async fn remove(&self, id: &str) -> Result<()> {
        remove_tree(&self.agents_root.join(id))
    }
}

pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            copy_file(&from, &to)?;
        }
    }
    Ok(())
}
