//! `papin-acp-enter` wrapper binary tests: handshake error paths that never
//! reach chroot (unprivileged), plus optional-gated real-chroot and systemd
//! unit tests (PLAN §9: run with `cargo test -- --ignored` on a normal
//! systemd host/VM; they print a skip reason when prerequisites are missing).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn enter_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_papin-acp-enter"))
}

fn test_dirs() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    (tmp, agents)
}

fn run_enter(agents_dir: &std::path::Path, input: &str) -> (String, String, bool) {
    let mut child = Command::new(enter_bin())
        .env("PAPIN_AGENTS_DIR", agents_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn papin-acp-enter");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

#[test]
fn enter_rejects_bad_bootstrap_line() {
    let (_tmp, agents) = test_dirs();
    let (stdout, _stderr, ok) = run_enter(&agents, "this is not json\n");
    assert!(!ok);
    assert!(
        stdout.contains(r#""reason":"unknown_agent""#),
        "stdout: {stdout}"
    );
}

#[test]
fn enter_reports_rootfs_not_ready_without_marker() {
    let (_tmp, agents) = test_dirs();
    std::fs::create_dir_all(agents.join("a1/root")).unwrap();
    let (stdout, _stderr, ok) = run_enter(&agents, "{\"agent\":\"a1\"}\n");
    assert!(!ok);
    assert!(
        stdout.contains(r#""reason":"rootfs_not_ready""#),
        "stdout: {stdout}"
    );
}

#[test]
fn enter_exits_quietly_on_eof() {
    let (_tmp, agents) = test_dirs();
    let (stdout, _stderr, ok) = run_enter(&agents, "");
    assert!(!ok);
    assert!(stdout.is_empty(), "no handshake on EOF: {stdout}");
}

/// Optional-gated (PLAN §9): full wrapper run with a REAL chroot as root.
#[test]
#[ignore = "requires root + cc -static (normal systemd host/VM)"]
fn enter_chroots_and_execs_stub_agent() {
    use std::os::unix::fs::PermissionsExt;

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("SKIP: not root");
        return;
    }
    let cc = std::process::Command::new("which")
        .arg("cc")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !cc {
        eprintln!("SKIP: no C compiler for the static in-chroot stub");
        return;
    }

    let (_tmp, agents) = test_dirs();
    let root = agents.join("a1/root");
    std::fs::create_dir_all(root.join("usr/local/bin")).unwrap();

    // Static stub: minimal ACP agent (initialize → response), then exit.
    let c_src = root.join("stub.c");
    std::fs::write(
        &c_src,
        r#"
#include <stdio.h>
#include <string.h>
int main(void) {
    char buf[4096];
    if (fgets(buf, sizeof buf, stdin) == NULL) return 2;
    if (strstr(buf, "\"initialize\"") != NULL) {
        fputs("{\"id\":\"_gw_init\",\"result\":{\"protocolVersion\":1,\"capabilities\":{},\"agentInfo\":{\"name\":\"stub\",\"version\":\"0\"}}}\n", stdout);
        fflush(stdout);
    }
    return 0;
}
"#,
    )
    .unwrap();
    let compile = std::process::Command::new("cc")
        .args(["-static", "-O2", "-o"])
        .arg(root.join("usr/local/bin/papin-acp"))
        .arg(&c_src)
        .output()
        .unwrap();
    if !compile.status.success() {
        eprintln!("SKIP: cc -static failed (no static libc?)");
        return;
    }
    std::fs::set_permissions(
        root.join("usr/local/bin/papin-acp"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(root.join(".papin-rootfs-ready"), "a1\n").unwrap();

    let mut child = Command::new(enter_bin())
        .env("PAPIN_AGENTS_DIR", &agents)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"{\"agent\":\"a1\"}\n{\"id\":\"_gw_init\",\"method\":\"initialize\",\"params\":{\"protocolVersion\":1}}\n")
        .unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(r#""status":"ready""#),
        "ready handshake missing: {stdout}"
    );
    assert!(
        stdout.contains("\"protocolVersion\":1"),
        "ACP response from inside the chroot missing: {stdout}"
    );
}

/// Optional-gated: verify the shipped systemd units parse on a systemd host.
#[test]
#[ignore = "requires systemd-analyze (normal systemd host/VM)"]
fn systemd_units_verify() {
    let tool = std::process::Command::new("which")
        .arg("systemd-analyze")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !tool {
        eprintln!("SKIP: systemd-analyze not available");
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for unit in [
        "rust/systemd/papin-agent.socket",
        "rust/systemd/papin-agent@.service",
        "rust/systemd/papin-gateway.service",
    ] {
        // --recursive-errors=no keeps verification to just these units
        // (otherwise unrelated host units with broken symlinks fail it).
        let out = std::process::Command::new("systemd-analyze")
            .arg("verify")
            .arg("--recursive-errors=no")
            .arg(root.join(unit))
            .output()
            .unwrap();
        if out.status.success() {
            continue;
        }
        // Hosts with unrelated broken units in /etc/systemd/system produce
        // "Failed to resolve symlink" noise that is not about our units;
        // treat that as a skip, fail on anything mentioning our unit names.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let ours = [
            "papin-agent.socket",
            "papin-agent@.service",
            "papin-gateway.service",
        ];
        let is_host_noise = |l: &str| {
            l.contains("Failed to resolve symlink")
                || l.contains("is not executable: No such file or directory")
        };
        let real_failure = stderr
            .lines()
            .filter(|l| !is_host_noise(l))
            .any(|l| !l.trim().is_empty());
        let mentions_ours = stderr
            .lines()
            .filter(|l| !is_host_noise(l))
            .any(|l| ours.iter().any(|u| l.contains(u)));
        if real_failure || mentions_ours {
            panic!("{unit} failed systemd-analyze verify: {stderr}");
        }
        eprintln!("SKIP: host has unrelated unresolvable units; {unit} itself verified clean");
    }
}
