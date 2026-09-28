use acp_wire::Envelope;
use serde_json::Value;

/// Stable map key for a JSON-RPC id.
pub fn id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_default()
}

/// Capability policy (PLAN §5.3): strip `fs`/`terminal` from the advertised
/// agent capabilities in an `initialize` response; pass `elicitation` through.
pub fn apply_capability_policy(env: &mut Envelope) {
    let Some(result) = env.result.as_mut() else {
        return;
    };
    let Some(caps) = result
        .get_mut("capabilities")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    caps.remove("fs");
    caps.remove("terminal");
}

/// Error translation (PLAN §5.3): `auth_required` errors become actionable;
/// `turn.agent_busy` and everything else pass through unchanged.
pub fn translate_error(env: &mut Envelope) {
    let Some(err) = env.error.as_mut() else {
        return;
    };
    let auth_required = err.message.contains("auth_required")
        || err
            .data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str)
            .is_some_and(|r| r == "auth_required");
    if auth_required {
        err.message = "agent authentication required: re-authenticate the shared kimi login on the server (see operations.md)".into();
    }
}

/// Should this agent response transfer session ownership to the requesting
/// client? Yes for `session/new` and `session/load` responses that carry a
/// `sessionId` in the result.
pub fn transfers_ownership(method: Option<&str>, env: &Envelope) -> Option<String> {
    match method {
        Some("session/new") | Some("session/load") => env
            .result
            .as_ref()
            .and_then(|r| r.get("sessionId"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// Pick the owning client for an agent-originated message by `sessionId`.
pub fn owner_for(
    session_owner: &std::collections::HashMap<String, usize>,
    session_id: Option<&str>,
) -> Option<usize> {
    session_id.and_then(|s| session_owner.get(s).copied())
}

pub const ERR_AGENT_LOST: i64 = -32000;
pub const ERR_REQUEST_CANCELLED: i64 = -32800;

pub fn agent_lost_error() -> acp_wire::RpcError {
    acp_wire::RpcError {
        code: ERR_AGENT_LOST,
        message: "agent connection lost and could not be re-established".into(),
        data: None,
    }
}

pub fn request_cancelled_error() -> acp_wire::RpcError {
    acp_wire::RpcError {
        code: ERR_REQUEST_CANCELLED,
        message: "request cancelled: owning client disconnected".into(),
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn initialize_response() -> Envelope {
        let mut env = Envelope::response(
            json!(1),
            json!({
                "protocolVersion": 1,
                "capabilities": {
                    "fs": {"readTextFile": true, "writeTextFile": true},
                    "terminal": true,
                    "mcp": {},
                    "elicitation": true
                },
                "agentInfo": {"name": "fake", "version": "0.1"}
            }),
        );
        env.session_id = Some("s1".into());
        env
    }

    #[test]
    fn capability_policy_strips_fs_and_terminal() {
        let mut env = initialize_response();
        apply_capability_policy(&mut env);
        let caps = env.result.as_ref().unwrap()["capabilities"]
            .as_object()
            .unwrap();
        assert!(!caps.contains_key("fs"));
        assert!(!caps.contains_key("terminal"));
        assert!(caps.contains_key("elicitation"));
        assert!(caps.contains_key("mcp"));
    }

    #[test]
    fn capability_policy_ignores_non_initialize() {
        let mut env = Envelope::response(json!(2), json!({"sessionId": "s1"}));
        apply_capability_policy(&mut env);
        assert_eq!(env.result.unwrap(), json!({"sessionId": "s1"}));
    }

    #[test]
    fn auth_required_error_is_actionable() {
        let mut env = Envelope {
            id: Some(json!(3)),
            session_id: None,
            method: None,
            params: None,
            result: None,
            error: Some(acp_wire::RpcError {
                code: -32603,
                message: "auth_required".into(),
                data: Some(json!({"reason": "auth_required"})),
            }),
        };
        translate_error(&mut env);
        assert!(env
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("re-authenticate"));
        // pass-through
        let mut busy = Envelope::error_response(json!(4), -32000, "turn.agent_busy");
        translate_error(&mut busy);
        assert_eq!(busy.error.as_ref().unwrap().message, "turn.agent_busy");
    }

    #[test]
    fn ownership_transfer_only_for_new_and_load() {
        let mut env = Envelope::response(json!(5), json!({"sessionId": "s9"}));
        assert_eq!(
            transfers_ownership(Some("session/new"), &env),
            Some("s9".into())
        );
        assert_eq!(
            transfers_ownership(Some("session/load"), &env),
            Some("s9".into())
        );
        assert_eq!(transfers_ownership(Some("session/prompt"), &env), None);
        env.result = None;
        assert_eq!(transfers_ownership(Some("session/new"), &env), None);
    }

    #[test]
    fn owner_lookup_by_session() {
        let mut map = std::collections::HashMap::new();
        map.insert("s1".to_string(), 7usize);
        assert_eq!(owner_for(&map, Some("s1")), Some(7));
        assert_eq!(owner_for(&map, Some("nope")), None);
        assert_eq!(owner_for(&map, None), None);
    }

    #[test]
    fn id_key_distinct_and_stable() {
        assert_eq!(id_key(&json!(1)), "1");
        assert_ne!(id_key(&json!(1)), id_key(&json!("1")));
    }
}
