use integration_tests::common::*;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn idle_timeout_drops_quiescent_connection_and_activity_reconnects() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let manager = Arc::new(papin_gateway::ConnectionManager::new(
        Arc::new(provider),
        Duration::from_secs(60),
    ));
    let att = manager.attach(AGENT).expect("attach");
    wait_connected_real(&manager, AGENT).await;

    // No traffic: jump virtual time past the idle deadline; the pump's idle
    // timer fires even though no real time passed.
    tokio::time::advance(Duration::from_secs(61)).await;
    wait_disconnected_real(&manager, AGENT).await;

    // Next client message reconnects transparently. Assert on the generation
    // counter rather than an agent round-trip: after reconnect the pump holds
    // a fresh idle timer, and any further idle period auto-advances virtual
    // time past it (a paused-clock artifact, not a gateway bug).
    send_attach(&manager, AGENT, att.client_id, initialize_request(1)).await;
    let g1 = manager.generation(AGENT);
    wait_connected_real(&manager, AGENT).await;
    assert!(manager.generation(AGENT) > g1, "reconnected on activity");
}

#[tokio::test]
async fn idle_timeout_disabled_keeps_connection() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let manager = Arc::new(papin_gateway::ConnectionManager::new(
        Arc::new(provider),
        Duration::from_secs(0),
    ));
    let _att = manager.attach(AGENT).expect("attach");
    // Real time: poll briefly for connect.
    for _ in 0..200 {
        if manager.is_connected(AGENT) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(manager.is_connected(AGENT));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        manager.is_connected(AGENT),
        "idle_timeout=0 keeps the connection alive"
    );
}

#[tokio::test]
async fn crash_mid_turn_heals_transparently() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let manager = Arc::new(papin_gateway::ConnectionManager::new(
        Arc::new(provider),
        Duration::from_secs(600),
    ));
    let mut att = manager.attach(AGENT).expect("attach");

    send_attach(&manager, AGENT, att.client_id, initialize_request(1)).await;
    let resp = att.rx.recv().await.expect("init response");
    assert_eq!(resp.id, Some(json!(1)));

    // Prompt "crash": the fake exits mid-turn without responding.
    send_attach(
        &manager,
        AGENT,
        att.client_id,
        prompt_request(9, "unused", "crash"),
    )
    .await;

    // Gateway reconnects (spawns a new fake), re-initializes, session/loads.
    let g0 = manager.generation(AGENT);
    for _ in 0..300 {
        if manager.generation(AGENT) > g0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(manager.is_connected(AGENT), "agent reconnected after crash");

    // The healed connection serves a full turn again.
    send_attach(
        &manager,
        AGENT,
        att.client_id,
        acp_wire::Envelope::request(10, acp_wire::method::SESSION_NEW, json!({"cwd": "/"})),
    )
    .await;
    let resp = att.rx.recv().await.expect("session/new response");
    assert_eq!(resp.id, Some(json!(10)));
    let session_id = resp.result.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    send_attach(
        &manager,
        AGENT,
        att.client_id,
        prompt_request(11, &session_id, "still alive"),
    )
    .await;
    let mut stop = None;
    for _ in 0..16 {
        let env = att.rx.recv().await.expect("stream");
        if env.id == Some(json!(11)) {
            stop = Some(env);
            break;
        }
    }
    assert_eq!(
        stop.expect("stop after healing").result.as_ref().unwrap()["stopReason"],
        "end_turn"
    );
}

#[tokio::test]
async fn unknown_agent_fails_round_trip_with_404_class_error() {
    let (provider, _dir) = provider_with(&["known-agent"]).await;
    let manager = Arc::new(papin_gateway::ConnectionManager::new(
        Arc::new(provider),
        Duration::from_secs(600),
    ));
    let err = manager
        .round_trip("ghost-agent", initialize_request(1))
        .await
        .expect_err("unknown agent must not hang");
    match err {
        papin_gateway::GatewayError::NotFound(m) => assert!(m.contains("unknown_agent")),
        other => panic!("unexpected error: {other}"),
    }
}

#[tokio::test]
async fn fake_provider_registry_lifecycle() {
    use papin_gateway::provider::{AgentProvider, AgentSpec};
    let (provider, _dir) = provider_with(&[]).await;
    let record = provider
        .create(AgentSpec {
            id: Some("my-agent".into()),
            name: Some("Mine".into()),
            config: papin_gateway::AgentConfig {
                base: "fake".into(),
                seed: None,
                env: Default::default(),
            },
        })
        .await
        .unwrap();
    assert_eq!(record.id, "my-agent");

    // Duplicate id rejected.
    let dup = provider
        .create(AgentSpec {
            id: Some("my-agent".into()),
            name: None,
            config: papin_gateway::AgentConfig {
                base: "fake".into(),
                seed: None,
                env: Default::default(),
            },
        })
        .await;
    assert!(matches!(
        dup.unwrap_err(),
        papin_gateway::GatewayError::AlreadyExists(_)
    ));

    // Invalid id rejected.
    assert!(provider
        .create(AgentSpec {
            id: Some("Bad_ID!".into()),
            name: None,
            config: papin_gateway::AgentConfig {
                base: "fake".into(),
                seed: None,
                env: Default::default(),
            },
        })
        .await
        .is_err());

    // Unknown base rejected by the catalog.
    assert!(provider
        .create(AgentSpec {
            id: Some("other-agent".into()),
            name: None,
            config: papin_gateway::AgentConfig {
                base: "does-not-exist".into(),
                seed: None,
                env: Default::default(),
            },
        })
        .await
        .is_err());

    let list = provider.list().await.unwrap();
    assert_eq!(list.len(), 1);

    provider.remove("my-agent", true).await.unwrap();
    assert!(provider.list().await.unwrap().is_empty());
    assert!(provider.remove("my-agent", true).await.is_err());
}
