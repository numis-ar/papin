use acp_wire::method;
use integration_tests::common::*;
use serde_json::json;

async fn ws_url(state: std::sync::Arc<papin_gateway::AppState>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(papin_gateway::server::serve_on(state, listener));
    format!("ws://{addr}/acp/agents/{AGENT}")
}

#[tokio::test]
async fn ws_full_turn_with_fake_provider() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let url = ws_url(state).await;
    let mut ws = acp_wire::ws_connect_with(&url, &[("Authorization", &format!("Bearer {TOKEN}"))])
        .await
        .expect("ws handshake");

    // initialize: capabilities must be policy-filtered (no fs/terminal, elicitation kept).
    send_ws(&mut ws, &initialize_request(1)).await;
    let resp = recv_ws(&mut ws).await;
    assert_eq!(resp.id, Some(json!(1)));
    let caps = resp.result.as_ref().unwrap()["capabilities"]
        .as_object()
        .unwrap();
    assert!(!caps.contains_key("fs"));
    assert!(!caps.contains_key("terminal"));
    assert!(caps.contains_key("elicitation"));
    assert!(caps.contains_key("mcp"));

    send_ws(
        &mut ws,
        &acp_wire::Envelope::notification(method::INITIALIZED, json!({})),
    )
    .await;

    // session/new
    let mut req = acp_wire::Envelope::request(2, method::SESSION_NEW, json!({"cwd": "/workspace"}));
    req.session_id = None;
    send_ws(&mut ws, &req).await;
    let resp = recv_ws(&mut ws).await;
    assert_eq!(resp.id, Some(json!(2)));
    let session_id = resp.result.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(resp.session_id.as_deref(), Some(session_id.as_str()));

    // session/prompt: expect a stream of session/update then a stop response.
    send_ws(&mut ws, &prompt_request(3, &session_id, "hello world")).await;
    let mut saw_message = false;
    let mut saw_tool_call = false;
    let mut stop = None;
    for _ in 0..32 {
        let env = recv_ws(&mut ws).await;
        for u in updates_of(&env) {
            match u {
                "agent_message_chunk" => saw_message = true,
                "tool_call" => saw_tool_call = true,
                _ => {}
            }
        }
        if env.id == Some(json!(3)) {
            stop = Some(env);
            break;
        }
    }
    let stop = stop.expect("prompt response");
    assert_eq!(stop.result.as_ref().unwrap()["stopReason"], "end_turn");
    assert!(saw_message, "agent_message_chunk streamed");
    assert!(saw_tool_call, "tool_call streamed");

    // fs/read_text_file passes through to the fake (agent-side capability).
    let mut req =
        acp_wire::Envelope::request(4, "fs/read_text_file", json!({"path": "/workspace/x.txt"}));
    req.session_id = Some(session_id.clone());
    send_ws(&mut ws, &req).await;
    let resp = recv_ws(&mut ws).await;
    assert_eq!(resp.id, Some(json!(4)));
    assert_eq!(
        resp.result.as_ref().unwrap()["content"],
        "fake file contents\n"
    );
}

#[tokio::test]
async fn ws_reverse_rpc_permission_forwarded_to_owning_client() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let url = ws_url(state).await;
    let mut ws = acp_wire::ws_connect_with(&url, &[("Authorization", &format!("Bearer {TOKEN}"))])
        .await
        .expect("ws handshake");

    send_ws(&mut ws, &initialize_request(1)).await;
    let _ = recv_ws(&mut ws).await; // initialize response
    send_ws(
        &mut ws,
        &acp_wire::Envelope::notification(method::INITIALIZED, json!({})),
    )
    .await;
    send_ws(
        &mut ws,
        &acp_wire::Envelope::request(2, method::SESSION_NEW, json!({"cwd": "/"})),
    )
    .await;
    let resp = recv_ws(&mut ws).await;
    let session_id = resp.result.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    send_ws(&mut ws, &prompt_request(3, &session_id, "permission")).await;
    // Agent asks the client for permission (reverse-RPC arrives on our WS).
    let mut perm = None;
    for _ in 0..8 {
        let env = recv_ws(&mut ws).await;
        if env.method.as_deref() == Some(method::SESSION_REQUEST_PERMISSION) {
            perm = Some(env);
            break;
        }
    }
    let perm = perm.expect("request_permission forwarded to owning client");
    assert_eq!(perm.session_id.as_deref(), Some(session_id.as_str()));
    let perm_id = perm.id.clone().unwrap();

    // Answer: allow_once. The fake then completes the turn.
    send_ws(
        &mut ws,
        &acp_wire::Envelope::response(perm_id, json!({"outcome": "allow_once"})),
    )
    .await;
    let mut stop = None;
    for _ in 0..16 {
        let env = recv_ws(&mut ws).await;
        if env.id == Some(json!(3)) {
            stop = Some(env);
            break;
        }
    }
    let stop = stop.expect("prompt response after permission");
    assert_eq!(stop.result.as_ref().unwrap()["stopReason"], "end_turn");
}

#[tokio::test]
async fn ws_cancel_both_directions() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let url = ws_url(state).await;
    let mut ws = acp_wire::ws_connect_with(&url, &[("Authorization", &format!("Bearer {TOKEN}"))])
        .await
        .expect("ws handshake");

    send_ws(&mut ws, &initialize_request(1)).await;
    let _ = recv_ws(&mut ws).await;
    send_ws(
        &mut ws,
        &acp_wire::Envelope::notification(method::INITIALIZED, json!({})),
    )
    .await;
    send_ws(
        &mut ws,
        &acp_wire::Envelope::request(2, method::SESSION_NEW, json!({"cwd": "/"})),
    )
    .await;
    let resp = recv_ws(&mut ws).await;
    let session_id = resp.result.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    // session/cancel: prompt "loop" streams until cancelled.
    send_ws(&mut ws, &prompt_request(10, &session_id, "loop")).await;
    let mut got_tick = false;
    for _ in 0..16 {
        let env = recv_ws(&mut ws).await;
        if !updates_of(&env).is_empty() {
            got_tick = true;
            break;
        }
    }
    assert!(got_tick, "streaming updates before cancel");

    let mut cancel =
        acp_wire::Envelope::request(11, method::SESSION_CANCEL, json!({"sessionId": session_id}));
    cancel.session_id = Some(session_id.clone());
    send_ws(&mut ws, &cancel).await;

    let mut prompt_err = false;
    let mut cancel_ack = false;
    for _ in 0..8 {
        let env = recv_ws(&mut ws).await;
        if env.id == Some(json!(10)) {
            assert!(env.error.is_some(), "cancelled prompt returns an error");
            prompt_err = true;
        }
        if env.id == Some(json!(11)) {
            assert!(env.error.is_none(), "session/cancel acked");
            cancel_ack = true;
        }
        if prompt_err && cancel_ack {
            break;
        }
    }
    assert!(prompt_err && cancel_ack);
}

#[tokio::test]
async fn ws_cancel_request_notification() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let url = ws_url(state).await;
    let mut ws = acp_wire::ws_connect_with(&url, &[("Authorization", &format!("Bearer {TOKEN}"))])
        .await
        .expect("ws handshake");

    send_ws(&mut ws, &initialize_request(1)).await;
    let _ = recv_ws(&mut ws).await;
    send_ws(
        &mut ws,
        &acp_wire::Envelope::notification(method::INITIALIZED, json!({})),
    )
    .await;
    send_ws(
        &mut ws,
        &acp_wire::Envelope::request(2, method::SESSION_NEW, json!({"cwd": "/"})),
    )
    .await;
    let resp = recv_ws(&mut ws).await;
    let session_id = resp.result.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    send_ws(&mut ws, &prompt_request(20, &session_id, "loop")).await;
    send_ws(
        &mut ws,
        &acp_wire::Envelope::notification(
            method::CANCEL_REQUEST,
            json!({"id": 20, "sessionId": session_id}),
        ),
    )
    .await;
    let mut prompt_err = false;
    for _ in 0..8 {
        let env = recv_ws(&mut ws).await;
        if env.id == Some(json!(20)) {
            assert!(env.error.is_some());
            prompt_err = true;
            break;
        }
    }
    assert!(prompt_err, "prompt cancelled via $/cancel_request");
}

#[tokio::test]
async fn ws_requires_bearer_token() {
    let (provider, _dir) = provider_with(&[AGENT]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let url = ws_url(state).await;
    let result = acp_wire::ws_connect_with(&url, &[]).await;
    assert!(result.is_err(), "ws upgrade without token rejected");
}
