//! Headless end-to-end: the real `papin-cli -p` binary against a gateway test
//! server running the FakeProvider.
use integration_tests::common::*;
use std::io::Write;
use std::process::Command;

async fn spawn_gateway() -> (
    std::sync::Arc<papin_gateway::AppState>,
    String,
    tempfile::TempDir,
) {
    let (provider, dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(papin_gateway::server::serve_on(state.clone(), listener));
    (state, addr.to_string(), dir)
}

fn write_config(addr: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    write!(f, "url = \"http://{addr}\"\ntoken = \"{TOKEN}\"\n").unwrap();
    f
}

fn run_cli(config: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_papin"))
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .expect("run papin-cli")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_prompt_prints_final_response() {
    let (_state, addr, _dir) = spawn_gateway().await;
    let config = write_config(&addr);
    let out = run_cli(config.path(), &["--agent", AGENT, "-p", "hello headless"]);
    assert!(out.status.success(), "exit ok: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Fake response to: hello headless"),
        "stdout: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("[papin] initialized"), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_unknown_agent_fails_with_nonzero_exit() {
    let (_state, addr, _dir) = spawn_gateway().await;
    let config = write_config(&addr);
    let out = run_cli(config.path(), &["--agent", "ghost", "-p", "hi"]);
    assert!(!out.status.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_resume_replays_history() {
    let (_state, addr, _dir) = spawn_gateway().await;
    let config = write_config(&addr);
    // First run creates a session; grab its id from stderr.
    let out = run_cli(config.path(), &["--agent", AGENT, "-p", "first run"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let sid = stderr
        .split("session ")
        .nth(1)
        .and_then(|s| s.split('\n').next())
        .expect("session id in stderr")
        .trim()
        .to_string();

    // Resume: the fake replays prior turns; final response still printed.
    let out = run_cli(
        config.path(),
        &["--agent", AGENT, "--resume", &sid, "-p", "second run"],
    );
    assert!(out.status.success(), "resume ok: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Fake response to: second run"),
        "stdout: {stdout}"
    );
}
