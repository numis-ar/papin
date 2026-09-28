mod ndjson;
mod ws;

pub use ndjson::{read_frame, reader, write_frame};
pub use ws::{ws_connect, ws_connect_with, ws_recv_envelope, ws_send_envelope, WsStream};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC error object (ACP v1 `JSONRPCError`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// ACP v1 JSON-RPC envelope. One type covers requests, responses, and
/// notifications; field names match the vendored schema 1.23.0 exactly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Envelope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Envelope {
    pub fn request(id: impl Into<Value>, method: &str, params: Value) -> Self {
        Envelope {
            id: Some(id.into()),
            session_id: None,
            method: Some(method.to_string()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    pub fn notification(method: &str, params: Value) -> Self {
        Envelope {
            id: None,
            session_id: None,
            method: Some(method.to_string()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    pub fn response(id: Value, result: Value) -> Self {
        Envelope {
            id: Some(id),
            session_id: None,
            method: None,
            params: None,
            result: Some(result),
            error: None,
        }
    }

    pub fn error_response(id: Value, code: i64, message: impl Into<String>) -> Self {
        Envelope {
            id: Some(id),
            session_id: None,
            method: None,
            params: None,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    pub fn is_request(&self) -> bool {
        self.method.is_some() && self.id.is_some()
    }

    pub fn is_notification(&self) -> bool {
        self.method.is_some() && self.id.is_none()
    }

    pub fn is_response(&self) -> bool {
        self.method.is_none() && self.id.is_some()
    }
}

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "initialized";
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_LOAD: &str = "session/load";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_CANCEL: &str = "session/cancel";
    pub const SESSION_UPDATE: &str = "session/update";
    pub const SESSION_REQUEST_PERMISSION: &str = "session/request_permission";
    pub const SESSION_ELICITATION: &str = "session/elicitation";
    pub const CANCEL_REQUEST: &str = "$/cancel_request";
}

/// Exponential backoff with cap, for reconnect loops.
pub fn backoff(attempt: u32) -> std::time::Duration {
    let base = std::time::Duration::from_millis(50);
    let shift = attempt.min(7);
    base.saturating_mul(1 << shift)
        .min(std::time::Duration::from_secs(5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn envelope_roundtrip() {
        let e = Envelope::request(1, "initialize", json!({"protocolVersion": 1}));
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"method\":\"initialize\""));
        assert!(s.contains("\"id\":1"));
        assert!(!s.contains("sessionId"));
        let back: Envelope = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn notification_has_no_id() {
        let e = Envelope::notification("initialized", json!({}));
        assert!(e.is_notification());
        let s = serde_json::to_string(&e).unwrap();
        assert!(!s.contains("\"id\""));
    }

    #[test]
    fn response_kinds() {
        assert!(Envelope::response(json!(1), json!({})).is_response());
        assert!(Envelope::error_response(json!(1), -1, "x").is_response());
        assert!(Envelope::request(1, "m", json!({})).is_request());
    }
}
