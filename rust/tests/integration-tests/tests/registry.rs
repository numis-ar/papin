use http::{header, Request, StatusCode};
use integration_tests::common::*;
use serde_json::{json, Value};
use tower::ServiceExt;

fn get(uri: &str) -> Request<String> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(String::new())
        .unwrap()
}

fn post(uri: &str, body: Value) -> Request<String> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(body.to_string())
        .unwrap()
}

fn delete(uri: &str) -> Request<String> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(String::new())
        .unwrap()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn catalog_lists_bases_seeds_and_env() {
    let (state, _dirs) = gateway_state(std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state);
    let resp = app.oneshot(get("/api/v1/config-catalog")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["bases"][0]["name"], "fake");
    assert_eq!(body["seeds"][0]["name"], "demo");
    assert_eq!(body["env"][0]["key"], "PAPIN_FAKE_ENV");
    assert_eq!(body["env"][0]["default"], "1");
}

#[tokio::test]
async fn create_agent_validates_against_catalog() {
    let (state, dirs) = gateway_state(std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state);

    // Unknown base → 400.
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/agents",
            json!({"id": "a1", "config": {"base": "nope"}}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Unknown seed → 400.
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/agents",
            json!({"id": "a1", "config": {"base": "fake", "seed": "nope"}}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Disallowed env key → 400.
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/agents",
            json!({"id": "a1", "config": {"base": "fake", "env": {"EVIL": "1"}}}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Invalid id → 400.
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/agents",
            json!({"id": "Bad_ID", "config": {"base": "fake"}}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Valid → 201, record returned, rootfs bootstrapped, no process started.
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/agents",
            json!({"id": "a1", "name": "Agent One", "config": {"base": "fake", "seed": "demo", "env": {"PAPIN_FAKE_ENV": "2"}}}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["id"], "a1");
    assert_eq!(body["name"], "Agent One");
    assert_eq!(body["state"], "stopped");

    // Rootfs stand-in: seed copied, marker dropped.
    let root = dirs.agents_root.join("a1/root");
    assert!(root.join(".papin-rootfs-ready").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("workspace/src/main.txt")).unwrap(),
        "hello seed\n"
    );
    assert!(root.join("etc/passwd").exists());

    // Duplicate → 409.
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/agents",
            json!({"id": "a1", "config": {"base": "fake"}}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn list_agents_reports_state_from_connection_table() {
    let (state, _dirs) = gateway_state(std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state.clone());

    app.clone()
        .oneshot(post(
            "/api/v1/agents",
            json!({"id": "idle-agent", "config": {"base": "fake"}}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(post(
            "/api/v1/agents",
            json!({"id": "busy-agent", "config": {"base": "fake"}}),
        ))
        .await
        .unwrap();

    // Attach a WS client to busy-agent → state becomes active.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(papin_gateway::server::serve_on(state.clone(), listener));
    let url = format!("ws://{addr}/acp/agents/busy-agent");
    // The WS attachment itself registers the client; keep it alive until the
    // listing below has been fetched.
    let _ws = acp_wire::ws_connect_with(&url, &[("Authorization", &format!("Bearer {TOKEN}"))])
        .await
        .expect("ws handshake");
    // Wait for the gateway-agent connection to come up.
    for _ in 0..200 {
        if state.manager.is_connected("busy-agent") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(state.manager.is_connected("busy-agent"));

    let resp = app.oneshot(get("/api/v1/agents")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let agents = body.as_array().unwrap();
    assert_eq!(agents.len(), 2);
    let by_id: std::collections::HashMap<&str, &Value> = agents
        .iter()
        .map(|a| (a["id"].as_str().unwrap(), a))
        .collect();
    assert_eq!(by_id["busy-agent"]["state"], "active");
    assert_eq!(by_id["idle-agent"]["state"], "stopped");
}

#[tokio::test]
async fn delete_refuses_while_attached_and_force_disconnects() {
    let (state, dirs) = gateway_state(std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state.clone());
    app.clone()
        .oneshot(post(
            "/api/v1/agents",
            json!({"id": "del-agent", "config": {"base": "fake"}}),
        ))
        .await
        .unwrap();

    // Attach via the manager directly (WS handshake not needed for the table).
    let att = state.manager.attach("del-agent").unwrap();
    for _ in 0..200 {
        if state.manager.is_connected("del-agent") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Attached → 409.
    let resp = app
        .clone()
        .oneshot(delete("/api/v1/agents/del-agent"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // force=true → 204, metadata + rootfs gone.
    let resp = app
        .clone()
        .oneshot(delete("/api/v1/agents/del-agent?force=true"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(!dirs.registry_dir.join("del-agent.json").exists());
    assert!(!dirs.agents_root.join("del-agent/root").exists());

    // Gone → 404 (and detached).
    let resp = app
        .clone()
        .oneshot(delete("/api/v1/agents/del-agent"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    drop(att);
}

#[tokio::test]
async fn registry_requires_auth() {
    let (state, _dirs) = gateway_state(std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/agents")
                .body(String::new())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
