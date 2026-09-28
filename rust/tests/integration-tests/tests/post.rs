use http::{header, Request, StatusCode};
use integration_tests::common::*;
use serde_json::{json, Value};
use tower::ServiceExt;

fn post(uri: &str, body: Value, bearer: Option<&str>) -> Request<String> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(body.to_string()).unwrap()
}

#[tokio::test]
async fn post_initialize_returns_200_with_filtered_capabilities() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state);

    let resp = app
        .clone()
        .oneshot(post(
            &format!("/acp/agents/{AGENT}"),
            serde_json::to_value(integration_tests::common::initialize_request(1)).unwrap(),
            Some(TOKEN),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&resp_to_bytes(resp).await).unwrap();
    assert_eq!(body["id"], 1);
    let caps = body["result"]["capabilities"].as_object().unwrap();
    assert!(!caps.contains_key("fs"));
    assert!(!caps.contains_key("terminal"));
    assert!(caps.contains_key("elicitation"));
}

#[tokio::test]
async fn post_non_initialize_returns_202_with_envelope() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state);

    let resp = app
        .clone()
        .oneshot(post(
            &format!("/acp/agents/{AGENT}"),
            json!({"id": 2, "method": "session/new", "params": {"cwd": "/workspace"}}),
            Some(TOKEN),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: Value = serde_json::from_slice(&resp_to_bytes(resp).await).unwrap();
    assert_eq!(body["id"], 2);
    let session_id = body["result"]["sessionId"].as_str().unwrap().to_string();

    // Full synchronous turn over POST (M1 simplification; SSE delivery is M6).
    let prompt = prompt_request(3, &session_id, "hello over post");
    let resp = app
        .oneshot(post(
            &format!("/acp/agents/{AGENT}"),
            serde_json::to_value(prompt).unwrap(),
            Some(TOKEN),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: Value = serde_json::from_slice(&resp_to_bytes(resp).await).unwrap();
    assert_eq!(body["result"]["stopReason"], "end_turn");
}

#[tokio::test]
async fn post_validation_errors() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state);
    let uri = format!("/acp/agents/{AGENT}");

    // Batch → 501 (RFD).
    let resp = app
        .clone()
        .oneshot(post(&uri, json!([{"id": 1}]), Some(TOKEN)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

    // Wrong content type → 415.
    let req = Request::builder()
        .method("POST")
        .uri(&uri)
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body("not json".to_string())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // Invalid JSON → 400.
    let req = Request::builder()
        .method("POST")
        .uri(&uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body("{oops".to_string())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn auth_rejects_missing_unknown_and_expired_tokens() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state);
    let uri = format!("/acp/agents/{AGENT}");
    let init = serde_json::to_value(initialize_request(1)).unwrap();

    // Missing token → 401 with actionable message.
    let resp = app
        .clone()
        .oneshot(post(&uri, init.clone(), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body: Value = serde_json::from_slice(&resp_to_bytes(resp).await).unwrap();
    assert!(body["message"].as_str().unwrap().contains("add-peer"));

    // Unknown token → 401.
    let resp = app
        .clone()
        .oneshot(post(&uri, init.clone(), Some("bogus")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Expired token → 401.
    let (provider2, _dir2) = provider_with(&["expired-agent"]).await;
    let now = papin_gateway::config::now_secs();
    let expired = std::sync::Arc::new(papin_gateway::config::TokenStore::from_entries(
        vec![papin_gateway::config::TokenEntry {
            token: TOKEN.into(),
            created_at: now - 7200,
            expires_at: Some(now - 3600),
            assigned_ip: None,
            pubkey: None,
            name: None,
        }],
        std::time::Duration::from_secs(3600),
    ));
    let state2 = papin_gateway::server::make_state(
        std::sync::Arc::new(provider2),
        std::sync::Arc::new(papin_gateway::FakeBootstrap::new(
            "/nonexistent",
            "/nonexistent",
        )),
        Default::default(),
        std::time::Duration::from_secs(600),
        expired,
    );
    let app2 = papin_gateway::server::router(state2);
    let resp = app2
        .oneshot(post("/acp/agents/expired-agent", init, Some(TOKEN)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn healthz_is_open_and_acp_is_not() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let app = papin_gateway::server::router(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(String::new())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(post(
            &format!("/acp/agents/{AGENT}"),
            json!({"id": 1}),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

async fn resp_to_bytes(resp: axum::response::Response) -> bytes::Bytes {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
}
