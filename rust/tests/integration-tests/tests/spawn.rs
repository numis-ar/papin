use integration_tests::common::*;
use papin_gateway::provider::{AgentProvider, BootstrapStatus, SpawnProvider};
use std::sync::Arc;

#[test]
fn bootstrap_status_line_parsing() {
    assert_eq!(
        BootstrapStatus::from_line("{\"status\":\"ready\"}"),
        BootstrapStatus::Ready
    );
    assert_eq!(
        BootstrapStatus::from_line("{\"status\":\"error\",\"reason\":\"unknown_agent\"}"),
        BootstrapStatus::UnknownAgent
    );
    assert_eq!(
        BootstrapStatus::from_line("{\"status\":\"error\",\"reason\":\"rootfs_not_ready\"}"),
        BootstrapStatus::RootfsNotReady
    );
    assert_eq!(
        BootstrapStatus::from_line("{\"status\":\"error\",\"reason\":\"exec_failed\"}"),
        BootstrapStatus::ExecFailed
    );
    assert_eq!(
        BootstrapStatus::from_line("{\"status\":\"error\",\"reason\":\"weird\"}"),
        BootstrapStatus::Malformed
    );
    assert_eq!(
        BootstrapStatus::from_line("garbage"),
        BootstrapStatus::Malformed
    );
}

/// Tiny unix-socket stub standing in for systemd's socket activation: reads
/// the one-line bootstrap, replies with a canned status line, then lingers.
async fn socket_stub(path: std::path::PathBuf, reply: &'static str) {
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Read the bootstrap line.
        let mut buf = vec![0u8; 128];
        let n = stream.read(&mut buf).await.unwrap();
        let line = String::from_utf8_lossy(&buf[..n]).trim().to_string();
        assert!(line.contains("\"agent\":\"a1\""), "bootstrap line: {line}");
        stream.write_all(reply.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        // Keep the socket open briefly so the gateway can proceed.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    });
}

async fn spawn_provider_with_stub(
    reply: &'static str,
) -> (SpawnProvider, tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    let socket = dir.path().join("agent.sock");
    let provider = SpawnProvider::new(dir.path().join("agents"), socket.clone(), test_catalog());
    // Registry record for a1 (connect requires it).
    use papin_gateway::provider::AgentSpec;
    provider
        .create(AgentSpec {
            id: Some("a1".into()),
            name: None,
            config: papin_gateway::AgentConfig {
                base: "fake".into(),
                seed: None,
                env: Default::default(),
            },
        })
        .await
        .unwrap();
    socket_stub(socket.clone(), reply).await;
    (provider, dir, socket)
}

#[tokio::test]
async fn spawn_provider_handshake_ready() {
    let (provider, _dir, _socket) = spawn_provider_with_stub("{\"status\":\"ready\"}\n").await;
    let io = provider.connect("a1").await.expect("ready handshake");
    let (_r, _w) = io.split_io();
}

#[tokio::test]
async fn spawn_provider_handshake_error_reasons() {
    // unknown_agent → NotFound (404-class).
    let (provider, _dir, _s) =
        spawn_provider_with_stub("{\"status\":\"error\",\"reason\":\"unknown_agent\"}\n").await;
    let err = match provider.connect("a1").await {
        Err(e) => e,
        Ok(_) => panic!("connect must fail"),
    };
    assert!(
        matches!(err, papin_gateway::GatewayError::NotFound(_)),
        "{err}"
    );

    // rootfs_not_ready → AgentUnavailable (503-class).
    let (provider, _dir, _s) =
        spawn_provider_with_stub("{\"status\":\"error\",\"reason\":\"rootfs_not_ready\"}\n").await;
    let err = match provider.connect("a1").await {
        Err(e) => e,
        Ok(_) => panic!("connect must fail"),
    };
    assert!(
        matches!(err, papin_gateway::GatewayError::AgentUnavailable(_)),
        "{err}"
    );

    // exec_failed → AgentUnavailable (503-class).
    let (provider, _dir, _s) =
        spawn_provider_with_stub("{\"status\":\"error\",\"reason\":\"exec_failed\"}\n").await;
    let err = match provider.connect("a1").await {
        Err(e) => e,
        Ok(_) => panic!("connect must fail"),
    };
    assert!(
        matches!(err, papin_gateway::GatewayError::AgentUnavailable(_)),
        "{err}"
    );
}

#[tokio::test]
async fn spawn_provider_unknown_registry_record() {
    let dir = tempfile::TempDir::new().unwrap();
    let provider = SpawnProvider::new(
        dir.path().join("agents"),
        dir.path().join("agent.sock"),
        test_catalog(),
    );
    let err = match provider.connect("ghost").await {
        Err(e) => e,
        Ok(_) => panic!("connect must fail"),
    };
    assert!(matches!(err, papin_gateway::GatewayError::NotFound(_)));
    let _ = Arc::new(provider); // keep type import used
}
