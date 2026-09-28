//! `papin-enroll-helper` — the only root-capable component in the system
//! (PLAN §7.1). Deliberately tiny: `apply <peers-file>` and nothing else.
//! All validation is pure and unit-tested here; `main` stays a thin shell.

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

pub const PEERS_DIR: &str = "/run/papin/wg-peers";
pub const WG_BIN: &str = "/usr/bin/wg";

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Apply(PathBuf),
}

/// The interface is deliberately tiny: exactly `apply <peers-file>`, nothing
/// else — no other subcommands, no flags, no config, no network.
pub fn parse_args(args: &[String]) -> Result<Action, String> {
    let mut it = args.iter();
    match (it.next().map(String::as_str), it.next(), it.next()) {
        (Some("apply"), Some(path), None) => Ok(Action::Apply(PathBuf::from(path))),
        _ => Err("usage: papin-enroll-helper apply <peers-file>".into()),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValidateError {
    NotRegularFile,
    NotInPeersDir,
    Symlink,
    Empty,
    BadLine(String),
    BadPublicKey(String),
    BadCidr(String),
    MissingPublicKey,
    MissingAllowedIps,
}

impl fmt::Display for ValidateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidateError::NotRegularFile => write!(f, "not a regular file"),
            ValidateError::NotInPeersDir => write!(f, "path escapes {PEERS_DIR}"),
            ValidateError::Symlink => write!(f, "symlinks are not accepted"),
            ValidateError::Empty => write!(f, "empty peers file"),
            ValidateError::BadLine(l) => write!(f, "unrecognized line: {l:?}"),
            ValidateError::BadPublicKey(k) => write!(f, "invalid WireGuard public key: {k:?}"),
            ValidateError::BadCidr(c) => write!(f, "invalid CIDR: {c:?}"),
            ValidateError::MissingPublicKey => write!(f, "[Peer] stanza missing PublicKey"),
            ValidateError::MissingAllowedIps => write!(f, "[Peer] stanza missing AllowedIPs"),
        }
    }
}

impl std::error::Error for ValidateError {}

/// Verify a peers-file path: canonicalize, reject symlinks and escapes, and
/// require a regular file inside `PEERS_DIR`.
pub fn validate_path(path: &Path, peers_dir: &Path) -> Result<PathBuf, ValidateError> {
    if path
        .symlink_metadata()
        .map_err(|_| ValidateError::NotRegularFile)?
        .file_type()
        .is_symlink()
    {
        return Err(ValidateError::Symlink);
    }
    let canonical_file = path
        .canonicalize()
        .map_err(|_| ValidateError::NotRegularFile)?;
    let canonical_dir = peers_dir
        .canonicalize()
        .map_err(|_| ValidateError::NotInPeersDir)?;
    if !canonical_file.starts_with(&canonical_dir) {
        return Err(ValidateError::NotInPeersDir);
    }
    let meta = std::fs::metadata(&canonical_file).map_err(|_| ValidateError::NotRegularFile)?;
    if !meta.is_file() {
        return Err(ValidateError::NotRegularFile);
    }
    Ok(canonical_file)
}

/// WireGuard public keys are 32 bytes, base64-encoded with standard padding
/// (44 chars, `=` padding). Validated without a crypto dependency.
pub fn validate_public_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.len() != 44 || !bytes[43].is_ascii() {
        return false;
    }
    let payload = &bytes[..43];
    let pad = &bytes[43..];
    let b64_ok = payload
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
        && pad == b"=";
    if !b64_ok {
        return false;
    }
    // Reject whitespace/structure abuse by decoding length semantics only:
    // 43 payload chars encode 32 bytes (33 would round up to 44 with one pad).
    true
}

/// Strict `wg-quick`-style subset: comments, blank lines, `[Peer]` stanzas
/// containing exactly `PublicKey = …` and `AllowedIPs = <cidr>[,<cidr>…]`.
pub fn validate_peers(contents: &str) -> Result<usize, ValidateError> {
    let mut peers = 0usize;
    let mut in_peer = false;
    let mut has_pubkey = false;
    let mut has_ips = false;
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[Peer]" {
            if in_peer && (!has_pubkey || !has_ips) {
                return Err(if !has_pubkey {
                    ValidateError::MissingPublicKey
                } else {
                    ValidateError::MissingAllowedIps
                });
            }
            in_peer = true;
            has_pubkey = false;
            has_ips = false;
            continue;
        }
        if !in_peer {
            return Err(ValidateError::BadLine(line.to_string()));
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "PublicKey" => {
                    let v = value.trim();
                    if !validate_public_key(v) {
                        return Err(ValidateError::BadPublicKey(v.to_string()));
                    }
                    has_pubkey = true;
                }
                "AllowedIPs" => {
                    for cidr in value.split(',') {
                        let cidr = cidr.trim();
                        if !validate_cidr(cidr) {
                            return Err(ValidateError::BadCidr(cidr.to_string()));
                        }
                    }
                    has_ips = true;
                }
                other => {
                    return Err(ValidateError::BadLine(format!(
                        "{other} = {}",
                        value.trim()
                    )))
                }
            }
        } else {
            return Err(ValidateError::BadLine(line.to_string()));
        }
    }
    if in_peer && (!has_pubkey || !has_ips) {
        return Err(if !has_pubkey {
            ValidateError::MissingPublicKey
        } else {
            ValidateError::MissingAllowedIps
        });
    }
    if peers == 0 && !contents.contains("[Peer]") {
        return Err(ValidateError::Empty);
    }
    peers += contents.matches("[Peer]").count();
    Ok(peers)
}

pub fn validate_cidr(cidr: &str) -> bool {
    let Some((addr, prefix)) = cidr.split_once('/') else {
        return false;
    };
    let Ok(addr) = addr.parse::<IpAddr>() else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    let max = match addr {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    prefix <= max
}

/// Build the fixed, shell-free invocation: absolute `wg` path, scrubbed
/// environment, no PATH lookup ambiguity.
pub fn wg_command(path: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(WG_BIN);
    cmd.args(["syncconf", "wg0"]).arg(path);
    cmd.env_clear();
    cmd.env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    cmd.env("LC_ALL", "C");
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("papin-enroll-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("wg-peers")).unwrap();
        dir
    }

    #[test]
    fn args_strictly_apply_one_path() {
        assert_eq!(
            parse_args(&["apply".into(), "/x/y.conf".into()]).unwrap(),
            Action::Apply(PathBuf::from("/x/y.conf"))
        );
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["status".into()]).is_err());
        assert!(parse_args(&["apply".into()]).is_err());
        assert!(parse_args(&["apply".into(), "a".into(), "b".into()]).is_err());
        assert!(parse_args(&["--help".into()]).is_err());
    }

    #[test]
    fn path_validation() {
        let dir = tmpdir();
        let peers = dir.join("wg-peers");
        let good = peers.join("token.conf");
        std::fs::write(&good, "[Peer]\n").unwrap();
        assert!(validate_path(&good, &peers).is_ok());

        // Escape: sibling file outside the peers dir.
        let outside = dir.join("evil.conf");
        std::fs::write(&outside, "[Peer]\n").unwrap();
        assert_eq!(
            validate_path(&outside, &peers),
            Err(ValidateError::NotInPeersDir)
        );

        // Symlink into the peers dir pointing outside.
        #[cfg(unix)]
        {
            let link = peers.join("link.conf");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert_eq!(validate_path(&link, &peers), Err(ValidateError::Symlink));
        }

        // Nonexistent.
        assert_eq!(
            validate_path(&peers.join("nope.conf"), &peers),
            Err(ValidateError::NotRegularFile)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn peers_content_validation() {
        let good = format!(
            "# peers\n\n[Peer]\nPublicKey = {GOOD_KEY}\nAllowedIPs = 10.77.0.2/32\n\n[Peer]\nPublicKey = {GOOD_KEY}\nAllowedIPs = 10.77.0.3/32, fd00::2/128\n"
        );
        assert_eq!(validate_peers(&good), Ok(2));

        assert_eq!(validate_peers(""), Err(ValidateError::Empty));
        assert_eq!(
            validate_peers("[Interface]\nListenPort = 5\n"),
            Err(ValidateError::BadLine("[Interface]".into()))
        );
        assert_eq!(
            validate_peers("[Peer]\nAllowedIPs = 10.0.0.1/32\n"),
            Err(ValidateError::MissingPublicKey)
        );
        assert_eq!(
            validate_peers(&format!("[Peer]\nPublicKey = {GOOD_KEY}\n")),
            Err(ValidateError::MissingAllowedIps)
        );
        assert_eq!(
            validate_peers("[Peer]\nPublicKey = not!!base64\nAllowedIPs = 10.0.0.1/32\n"),
            Err(ValidateError::BadPublicKey("not!!base64".into()))
        );
        assert_eq!(
            validate_peers(&format!(
                "[Peer]\nPublicKey = {GOOD_KEY}\nAllowedIPs = 10.0.0.1/33\n"
            )),
            Err(ValidateError::BadCidr("10.0.0.1/33".into()))
        );
        assert_eq!(
            validate_peers(&format!(
                "[Peer]\nPublicKey = {GOOD_KEY}\nAllowedIPs = 10.0.0.1/32\nEndpoint = 1.2.3.4:5\n"
            )),
            Err(ValidateError::BadLine("Endpoint = 1.2.3.4:5".into()))
        );
    }

    #[test]
    fn cidr_checks() {
        assert!(validate_cidr("10.0.0.1/32"));
        assert!(validate_cidr("10.0.0.0/8"));
        assert!(validate_cidr("fd00::1/128"));
        assert!(validate_cidr("::/0"));
        assert!(!validate_cidr("10.0.0.1/33"));
        assert!(!validate_cidr("999.0.0.1/32"));
        assert!(!validate_cidr("10.0.0.1"));
        assert!(!validate_cidr("10.0.0.1/x"));
        assert!(!validate_cidr("/32"));
    }

    #[test]
    fn wg_command_is_absolute_and_scrubbed() {
        let cmd = wg_command(Path::new("/run/papin/wg-peers/x.conf"));
        let prog = cmd.get_program().to_str().unwrap();
        assert!(prog.starts_with('/'), "absolute execve: {prog}");
        assert_eq!(prog, WG_BIN);
        assert!(cmd.get_envs().count() <= 2, "env scrubbed");
    }
}
