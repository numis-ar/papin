use acp_wire::method;
use integration_tests::common::*;
use serde_json::json;

async fn ws_url(state: std::sync::Arc<papin_gateway::AppState>, agent: &str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(papin_gateway::server::serve_on(state, listener));
    format!("ws://{addr}/acp/agents/{agent}")
}

async fn connect_initialized(agent: &str) -> (String, acp_wire::WsStream, String) {
    let (provider, _dir) = provider_with(&[agent]).await;
    let state = state(provider, std::time::Duration::from_secs(600));
    let url = ws_url(state, agent).await;
    let mut ws = acp_wire::ws_connect_with(&url, &[("Authorization", &format!("Bearer {TOKEN}"))])
        .await
        .expect("ws handshake");
    send_ws(&mut ws, &initialize_request(1)).await;
    let resp = recv_ws(&mut ws).await;
    assert_eq!(resp.id, Some(json!(1)));
    send_ws(
        &mut ws,
        &acp_wire::Envelope::notification(method::INITIALIZED, json!({})),
    )
    .await;
    (agent.to_string(), ws, url)
}

async fn new_session(ws: &mut acp_wire::WsStream, id: i64) -> String {
    send_ws(
        ws,
        &acp_wire::Envelope::request(id, method::SESSION_NEW, json!({"cwd": "/workspace"})),
    )
    .await;
    let resp = recv_ws(ws).await;
    assert_eq!(resp.id, Some(json!(id)));
    resp.result.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string()
}

fn session_request(
    id: i64,
    sid: &str,
    method: &str,
    params: serde_json::Value,
) -> acp_wire::Envelope {
    let mut env = acp_wire::Envelope::request(id, method, {
        let mut p = params;
        p["sessionId"] = json!(sid);
        p
    });
    env.session_id = Some(sid.to_string());
    env
}

#[tokio::test]
async fn session_surface_methods_pass_through() {
    let (_agent, mut ws, _url) = connect_initialized(AGENT).await;
    let sid = new_session(&mut ws, 2).await;

    // set_mode / set_config_option / set_model — each acked.
    for (id, m, params) in [
        (4, "session/set_mode", json!({"modeId": "vscode"})),
        (
            5,
            "session/set_config_option",
            json!({"configId": "temperature", "value": 0.2}),
        ),
        (6, "session/set_model", json!({"modelId": "kimi-k2"})),
    ] {
        send_ws(&mut ws, &session_request(id, &sid, m, params)).await;
        let resp = recv_ws(&mut ws).await;
        assert_eq!(resp.id, Some(json!(id)), "{m} response");
        assert!(resp.error.is_none(), "{m} error-free");
    }

    // session/list reflects the session.
    send_ws(
        &mut ws,
        &session_request(7, &sid, "session/list", json!({})),
    )
    .await;
    let resp = recv_ws(&mut ws).await;
    assert_eq!(resp.id, Some(json!(7)));
    let sessions = resp.result.as_ref().unwrap()["sessions"]
        .as_array()
        .unwrap();
    assert!(sessions.iter().any(|s| s["sessionId"] == sid));

    // session/close, then session/delete.
    send_ws(
        &mut ws,
        &session_request(8, &sid, "session/close", json!({})),
    )
    .await;
    let resp = recv_ws(&mut ws).await;
    assert_eq!(resp.id, Some(json!(8)));
    send_ws(
        &mut ws,
        &session_request(9, &sid, "session/delete", json!({})),
    )
    .await;
    let resp = recv_ws(&mut ws).await;
    assert_eq!(resp.id, Some(json!(9)));

    // Deleted session is gone from session/list.
    send_ws(
        &mut ws,
        &session_request(10, &sid, "session/list", json!({})),
    )
    .await;
    let resp = recv_ws(&mut ws).await;
    let sessions = resp.result.as_ref().unwrap()["sessions"]
        .as_array()
        .unwrap();
    assert!(!sessions.iter().any(|s| s["sessionId"] == sid));
}

#[tokio::test]
async fn session_load_replays_update_burst() {
    let (_agent, mut ws, _url) = connect_initialized(AGENT).await;
    let sid = new_session(&mut ws, 2).await;

    // Two turns of history.
    send_ws(&mut ws, &prompt_request(3, &sid, "first question")).await;
    wait_for_id(&mut ws, 3).await;
    send_ws(&mut ws, &prompt_request(4, &sid, "second question")).await;
    wait_for_id(&mut ws, 4).await;

    // session/load → replay burst of agent_message_chunk updates, then the response.
    send_ws(
        &mut ws,
        &session_request(5, &sid, method::SESSION_LOAD, json!({})),
    )
    .await;
    let mut replays = Vec::new();
    let mut response = None;
    for _ in 0..32 {
        let env = recv_ws(&mut ws).await;
        if env.id == Some(json!(5)) {
            response = Some(env);
            break;
        }
        if env.method.as_deref() == Some(method::SESSION_UPDATE) {
            if let Some(u) = env
                .params
                .as_ref()
                .and_then(|p| p.get("update"))
                .and_then(|u| u.get("sessionUpdate"))
                .and_then(|s| s.as_str())
            {
                replays.push(u.to_string());
            }
        }
    }
    assert!(response.is_some(), "session/load response");
    assert!(
        replays
            .iter()
            .filter(|u| *u == "agent_message_chunk")
            .count()
            >= 2,
        "replay burst contains the two turns, got {replays:?}"
    );
    // Replay updates are routed to the owning client even though the load
    // response has not arrived yet (ownership transfers on response — the
    // burst arrives before it in this fake; the client is already the owner
    // from session/new).
}

#[tokio::test]
async fn cancel_race_with_outstanding_permission_rpc() {
    let (_agent, mut ws, _url) = connect_initialized(AGENT).await;
    let sid = new_session(&mut ws, 2).await;

    // Prompt that triggers a reverse-RPC permission request.
    send_ws(&mut ws, &prompt_request(3, &sid, "permission")).await;
    let mut perm_id = None;
    for _ in 0..8 {
        let env = recv_ws(&mut ws).await;
        if env.method.as_deref() == Some(method::SESSION_REQUEST_PERMISSION) {
            perm_id = Some(env.id.clone().unwrap());
            break;
        }
    }
    let perm_id = perm_id.expect("permission request in flight");

    // session/cancel arrives while the reverse-RPC is outstanding.
    send_ws(
        &mut ws,
        &session_request(4, &sid, method::SESSION_CANCEL, json!({})),
    )
    .await;

    // The turn ends (prompt error) and the cancel is acked; the outstanding
    // permission request is then answered by the client (agent may or may not
    // consume it — the gateway must route the response by id either way).
    send_ws(
        &mut ws,
        &acp_wire::Envelope::response(perm_id, json!({"outcome": "reject_once"})),
    )
    .await;

    let mut saw_prompt_end = false;
    let mut saw_cancel_ack = false;
    for _ in 0..16 {
        let env = recv_ws(&mut ws).await;
        if env.id == Some(json!(3)) {
            saw_prompt_end = true;
        }
        if env.id == Some(json!(4)) {
            assert!(env.error.is_none(), "session/cancel acked");
            saw_cancel_ack = true;
        }
        if saw_prompt_end && saw_cancel_ack {
            break;
        }
    }
    assert!(saw_cancel_ack, "cancel acked");
}

#[tokio::test]
async fn immediate_cancel_after_prompt() {
    let (_agent, mut ws, _url) = connect_initialized(AGENT).await;
    let sid = new_session(&mut ws, 2).await;

    // Fire prompt + cancel back to back: whichever the agent processes first,
    // the client must get a definite outcome (cancel ack always; prompt either
    // runs to completion or returns a cancellation error).
    send_ws(&mut ws, &prompt_request(3, &sid, "loop")).await;
    send_ws(
        &mut ws,
        &session_request(4, &sid, method::SESSION_CANCEL, json!({})),
    )
    .await;
    let mut prompt_done = false;
    let mut cancel_done = false;
    for _ in 0..32 {
        let env = recv_ws(&mut ws).await;
        match env.id {
            Some(id) if id == json!(3) => prompt_done = true,
            Some(id) if id == json!(4) => {
                assert!(env.error.is_none());
                cancel_done = true;
            }
            _ => {}
        }
        if prompt_done && cancel_done {
            break;
        }
    }
    assert!(cancel_done, "cancel acked under race");
}

async fn wait_for_id(ws: &mut acp_wire::WsStream, id: i64) -> acp_wire::Envelope {
    for _ in 0..32 {
        let env = recv_ws(ws).await;
        if env.id == Some(json!(id)) {
            return env;
        }
    }
    panic!("no response for id {id}");
}
