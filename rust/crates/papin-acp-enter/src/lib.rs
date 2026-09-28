//! `papin-acp-enter` — the per-connection agent wrapper (PLAN §5.4).
//!
//! Runs as `DynamicUser` with ambient `CAP_SYS_CHROOT` + `CAP_DAC_OVERRIDE`,
//! inside the hardened `papin-agent@.service`. Steps:
//!   1. read one bootstrap line `{"agent":"<id>"}` from fd 0
//!   2. validate the id (`^[a-z0-9][a-z0-9-]{0,63}$`)
//!   3. stat `/var/lib/papin/agents/<id>/root/.papin-rootfs-ready`
//!   4. read `<id>.env` metadata (pre-chroot)
//!   5. chroot(root) + chdir("/")
//!   6. write `{"status":"ready"}`, then execve(`/usr/local/bin/papin-acp`)
//!
//! All side effects sit behind [`System`] so the whole decision sequence is
//! unit-testable without root. On `NoNewPrivileges=yes` (set by the unit)
//! execve clears the ambient capabilities — post-exec the agent holds only
//! the two-cap bounding set, inside the chroot, as an unprivileged uid.

use std::fmt;
use std::path::{Path, PathBuf};

/// Agent metadata/rootfs root. Overridable for tests/dev.
pub fn agents_dir() -> PathBuf {
    std::env::var("PAPIN_AGENTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/papin/agents"))
}

/// Rootfs-internal shim that execs `kimi acp` (§5.4 step 6, §10 EOF shim).
pub const INNER_EXEC: &str = "/usr/local/bin/papin-acp";
pub const READY_MARKER: &str = ".papin-rootfs-ready";

pub fn root_dir(agents_dir: &Path, id: &str) -> PathBuf {
    agents_dir.join(id).join("root")
}

/// Error handshake reasons (§5.2): mapped by the gateway to 404/503.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    UnknownAgent,
    RootfsNotReady,
    ExecFailed,
}

impl Reason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Reason::UnknownAgent => "unknown_agent",
            Reason::RootfsNotReady => "rootfs_not_ready",
            Reason::ExecFailed => "exec_failed",
        }
    }

    pub fn error_line(&self) -> String {
        format!("{{\"status\":\"error\",\"reason\":\"{}\"}}", self.as_str())
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parse + validate the one-line bootstrap handshake (§5.2 step 1).
pub fn parse_bootstrap_line(line: &str) -> Result<String, Reason> {
    let Some(Some(id)) = serde_json_minimal(line) else {
        return Err(Reason::UnknownAgent);
    };
    if !valid_id(&id) {
        return Err(Reason::UnknownAgent);
    }
    Ok(id.to_string())
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`
pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    id.len() <= 64 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Minimal `{"agent": "..."}` extractor: no serde dependency in the wrapper.
fn serde_json_minimal(line: &str) -> Option<Option<String>> {
    let line = line.trim();
    let inner = line.strip_prefix('{')?.strip_suffix('}')?.trim();
    let (key, value) = inner.split_once(':')?;
    if key.trim() != "\"agent\"" {
        return Some(None);
    }
    let value = value.trim();
    let quoted = value.strip_prefix('"')?.strip_suffix('"')?;
    Some(Some(quoted.to_string()))
}

/// Parse an agent `.env` metadata file: `KEY=VALUE` lines, `#` comments.
pub fn parse_env(contents: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if valid_env_key(key) {
                out.push((key.to_string(), value.trim().to_string()));
            }
        }
    }
    out
}

fn valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Build the minimal post-chroot environ (§5.4 step 4).
pub fn build_environ(env: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out = vec![
        ("HOME".into(), "/home/papin".into()),
        ("KIMI_CODE_HOME".into(), "/home/papin/.kimi-code".into()),
        ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
        ("USER".into(), "papin".into()),
        ("LOGNAME".into(), "papin".into()),
    ];
    out.extend(env);
    out
}

/// Injectable side effects for [`run`].
pub trait System {
    /// Read one line from fd 0 (the bootstrap handshake input).
    fn read_bootstrap_line(&mut self) -> Option<String>;
    /// Write one handshake line to fd 1.
    fn write_line(&mut self, line: &str);
    fn marker_exists(&self, root: &Path) -> bool;
    /// Read the agent's `.env` metadata; a missing file means "no extra env".
    fn read_env_file(&self, agents_dir: &Path, id: &str) -> Vec<(String, String)>;
    fn chroot(&self, root: &Path) -> Result<(), Reason>;
    /// `Ok` is only reachable in tests (a real execve never returns).
    fn exec(&self, path: &Path, env: &[(String, String)]) -> Result<(), Reason>;
}

/// Execute the wrapper sequence. On failure the error handshake line has
/// already been written. `Ok(())` means exec succeeded (test doubles only).
pub fn run<S: System>(sys: &mut S) -> Result<(), Reason> {
    let Some(line) = sys.read_bootstrap_line() else {
        // EOF before any bootstrap: exit silently (dying instance).
        return Err(Reason::UnknownAgent);
    };
    let id = match parse_bootstrap_line(&line) {
        Ok(id) => id,
        Err(reason) => {
            sys.write_line(&reason.error_line());
            return Err(reason);
        }
    };
    let agents = agents_dir();
    let root = root_dir(&agents, &id);

    if !sys.marker_exists(&root) {
        sys.write_line(&Reason::RootfsNotReady.error_line());
        return Err(Reason::RootfsNotReady);
    }

    let extra_env = sys.read_env_file(&agents, &id);
    let env = build_environ(extra_env);

    if sys.chroot(&root).is_err() {
        sys.write_line(&Reason::RootfsNotReady.error_line());
        return Err(Reason::RootfsNotReady);
    }
    std::env::set_current_dir("/").ok();

    sys.write_line("{\"status\":\"ready\"}");
    match sys.exec(Path::new(INNER_EXEC), &env) {
        Ok(()) => Ok(()),
        Err(reason) => {
            sys.write_line(&reason.error_line());
            Err(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Mock {
        line: Option<String>,
        written: Vec<String>,
        has_marker: bool,
        env: Vec<(String, String)>,
        chroot_fail: bool,
        exec_fail: bool,
    }

    impl System for Mock {
        fn read_bootstrap_line(&mut self) -> Option<String> {
            self.line.take()
        }
        fn write_line(&mut self, line: &str) {
            self.written.push(line.to_string());
        }
        fn marker_exists(&self, _root: &Path) -> bool {
            self.has_marker
        }
        fn read_env_file(&self, _agents: &Path, _id: &str) -> Vec<(String, String)> {
            self.env.clone()
        }
        fn chroot(&self, _root: &Path) -> Result<(), Reason> {
            if self.chroot_fail {
                Err(Reason::RootfsNotReady)
            } else {
                Ok(())
            }
        }
        fn exec(&self, _path: &Path, _env: &[(String, String)]) -> Result<(), Reason> {
            if self.exec_fail {
                Err(Reason::ExecFailed)
            } else {
                Ok(())
            }
        }
    }

    fn mock_with(line: &str) -> Mock {
        Mock {
            line: Some(line.into()),
            has_marker: true,
            ..Default::default()
        }
    }

    #[test]
    fn happy_path_ready_then_exec() {
        let mut m = mock_with("{\"agent\":\"my-agent\"}");
        m.env = vec![("PAPIN_EXTRA".into(), "1".into())];
        assert!(run(&mut m).is_ok());
        assert_eq!(m.written, vec!["{\"status\":\"ready\"}"]);
    }

    #[test]
    fn bad_bootstrap_line_is_unknown_agent() {
        for bad in [
            "",
            "garbage",
            "{\"agent\":123}",
            "{\"agent\":\"Bad_ID\"}",
            "{}",
            "{\"other\":\"x\"}",
        ] {
            let mut m = mock_with(bad);
            assert_eq!(run(&mut m), Err(Reason::UnknownAgent), "input {bad:?}");
            assert_eq!(m.written, vec![Reason::UnknownAgent.error_line()]);
        }
    }

    #[test]
    fn missing_marker_is_rootfs_not_ready() {
        let mut m = mock_with("{\"agent\":\"my-agent\"}");
        m.has_marker = false;
        assert_eq!(run(&mut m), Err(Reason::RootfsNotReady));
        assert_eq!(m.written, vec![Reason::RootfsNotReady.error_line()]);
    }

    #[test]
    fn chroot_failure_is_rootfs_not_ready() {
        let mut m = mock_with("{\"agent\":\"my-agent\"}");
        m.chroot_fail = true;
        assert_eq!(run(&mut m), Err(Reason::RootfsNotReady));
        // No ready line: the handshake failed before ACP may flow.
        assert_eq!(m.written, vec![Reason::RootfsNotReady.error_line()]);
    }

    #[test]
    fn exec_failure_is_exec_failed() {
        let mut m = mock_with("{\"agent\":\"my-agent\"}");
        m.exec_fail = true;
        assert_eq!(run(&mut m), Err(Reason::ExecFailed));
        assert_eq!(
            m.written,
            vec![
                "{\"status\":\"ready\"}".to_string(),
                Reason::ExecFailed.error_line(),
            ]
        );
    }

    #[test]
    fn bootstrap_line_edge_cases() {
        assert_eq!(
            parse_bootstrap_line("{\"agent\":\"a\"}"),
            Ok("a".to_string())
        );
        assert_eq!(
            parse_bootstrap_line(" { \"agent\" : \"abc-9\" } "),
            Ok("abc-9".to_string())
        );
        assert!(parse_bootstrap_line("{\"agent\":\"- leading-dash\"}").is_err());
        assert!(parse_bootstrap_line(&format!("{{\"agent\":\"{}\"}}", "a".repeat(65))).is_err());
    }

    #[test]
    fn env_parsing_and_environ() {
        let env = parse_env("# comment\nFOO=bar\nBAD-KEY=no\nEMPTY=\n SPACED = trimmed \n");
        assert_eq!(
            env,
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("EMPTY".to_string(), String::new()),
                ("SPACED".to_string(), "trimmed".to_string()),
            ]
        );
        let full = build_environ(env);
        assert!(full.contains(&("HOME".to_string(), "/home/papin".to_string())));
        assert!(full.contains(&(
            "KIMI_CODE_HOME".to_string(),
            "/home/papin/.kimi-code".to_string()
        )));
        assert!(full.contains(&("FOO".to_string(), "bar".to_string())));
    }
}
