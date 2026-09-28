use acp_wire::{method, Envelope};
use serde_json::{json, Value};

/// One transcript entry. Rendering lives in `tui.rs`; this model is terminal-
/// independent so tests can drive it with canned envelopes.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    User {
        text: String,
    },
    Thinking {
        text: String,
        collapsed: bool,
    },
    ToolCall {
        id: String,
        title: String,
        status: String,
        detail: String,
        expanded: bool,
    },
    Plan {
        items: Vec<(bool, String)>,
    },
    Agent {
        text: String,
    },
    Status {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Send session/cancel for this session.
    CancelTurn {
        session_id: String,
    },
    /// Answer an agent reverse-RPC with the chosen option index.
    AnswerPermission {
        request_id: Value,
        outcome: &'static str,
    },
    Quit,
    Noop,
}

/// Slash-command parse result.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Model(String),
    Mode(String),
    Sessions,
    Resume(String),
    NewSession,
    CloseSession,
    DeleteSession,
    Help,
    Quit,
    Unknown(String),
}

pub fn parse_command(input: &str) -> Option<Command> {
    let rest = input.strip_prefix('/')?;
    let (cmd, arg) = match rest.split_once(' ') {
        Some((c, a)) => (c, a.trim()),
        None => (rest, ""),
    };
    Some(match cmd {
        "model" if !arg.is_empty() => Command::Model(arg.to_string()),
        "mode" if !arg.is_empty() => Command::Mode(arg.to_string()),
        "sessions" => Command::Sessions,
        "resume" if !arg.is_empty() => Command::Resume(arg.to_string()),
        "new" => Command::NewSession,
        "close" => Command::CloseSession,
        "delete" => Command::DeleteSession,
        "help" => Command::Help,
        "quit" | "exit" => Command::Quit,
        other => Command::Unknown(format!("/{other}")),
    })
}

/// Terminal-independent chat model (§6): streaming transcript, turn state,
/// slash commands. The TUI feeds envelopes in and renders `items`; effects
/// are actions the caller must perform (I/O).
#[derive(Debug, Default)]
pub struct ChatModel {
    pub items: Vec<Item>,
    pub input: String,
    pub session_id: Option<String>,
    /// id of the in-flight prompt request; send is disabled while set.
    pub in_flight: Option<i64>,
    pub status: String,
    /// Reverse-RPC awaiting an answer (permission / elicitation).
    pub pending_permission: Option<(Value, Vec<String>)>,
    pub model: Option<String>,
    pub mode: Option<String>,
}

impl ChatModel {
    pub fn new() -> ChatModel {
        ChatModel {
            status: "connecting…".into(),
            ..Default::default()
        }
    }

    pub fn can_send(&self) -> bool {
        self.in_flight.is_none() && self.session_id.is_some()
    }

    pub fn push_status(&mut self, text: impl Into<String>) {
        let text = text.into();
        if let Some(Item::Status { text: last }) = self.items.last_mut() {
            *last = text;
        } else {
            self.items.push(Item::Status { text });
        }
    }

    /// Submit the input box: returns the prompt text, or a command effect.
    pub fn submit_input(&mut self) -> InputAction {
        let text = std::mem::take(&mut self.input);
        if text.starts_with('/') {
            if self.in_flight.is_some() && parse_command(&text).is_some() {
                // Commands are allowed mid-turn except where noted.
            }
            return match parse_command(&text) {
                Some(Command::Quit) => InputAction::Effect(Effect::Quit),
                Some(cmd) => InputAction::Command(cmd),
                None => InputAction::None,
            };
        }
        if text.trim().is_empty() {
            return InputAction::None;
        }
        if !self.can_send() {
            self.push_status("a turn is already in flight (Esc to cancel)");
            return InputAction::None;
        }
        self.items.push(Item::User { text: text.clone() });
        InputAction::Prompt(text)
    }

    pub fn key_esc(&mut self) -> Effect {
        match (&self.in_flight, &self.session_id) {
            (Some(_), Some(sid)) => Effect::CancelTurn {
                session_id: sid.clone(),
            },
            _ => Effect::Noop,
        }
    }

    /// Answer the pending permission prompt; `None` if none outstanding.
    pub fn answer_permission(&mut self, index: usize) -> Effect {
        let Some((request_id, options)) = self.pending_permission.take() else {
            return Effect::Noop;
        };
        let outcome = options
            .get(index)
            .cloned()
            .unwrap_or_else(|| "reject_once".to_string());
        let outcome: &'static str = match outcome.as_str() {
            "allow_once" => "allow_once",
            "allow_always" => "allow_always",
            "reject_once" => "reject_once",
            "reject_always" => "reject_always",
            _ => "reject_once",
        };
        Effect::AnswerPermission {
            request_id,
            outcome,
        }
    }

    pub fn apply(&mut self, env: &Envelope) -> Effect {
        if env.is_response() {
            return self.apply_response(env);
        }
        if env.method.as_deref() == Some(method::SESSION_UPDATE) {
            if let Some(update) = env.params.as_ref().and_then(|p| p.get("update")).cloned() {
                self.apply_update(&update);
            }
            return Effect::Noop;
        }
        if env.method.as_deref() == Some(method::SESSION_REQUEST_PERMISSION) {
            let options = env
                .params
                .as_ref()
                .and_then(|p| p.get("options"))
                .and_then(|o| o.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|o| o["kind"].as_str().map(str::to_string))
                        .collect::<Vec<String>>()
                })
                .unwrap_or_else(|| vec!["allow_once".into(), "reject_once".into()]);
            self.pending_permission = Some((env.id.clone().unwrap_or(Value::Null), options));
            let title = env
                .params
                .as_ref()
                .and_then(|p| p.get("toolCall"))
                .and_then(|t| t["title"].as_str())
                .unwrap_or("permission requested")
                .to_string();
            self.items.push(Item::Status {
                text: format!(
                    "permission: {title} — press 1..{} to answer",
                    self.pending_permission
                        .as_ref()
                        .map(|(_, o)| o.len())
                        .unwrap_or(0)
                ),
            });
            return Effect::Noop;
        }
        if env.method.as_deref() == Some("session/elicitation") {
            self.items.push(Item::Status {
                text: "elicitation requested (not supported by this client yet)".into(),
            });
            return Effect::Noop;
        }
        Effect::Noop
    }

    fn apply_response(&mut self, env: &Envelope) -> Effect {
        if self.in_flight.is_some()
            && env.id.as_ref() == self.in_flight.map(|id| json!(id)).as_ref()
        {
            self.in_flight = None;
            if let Some(err) = &env.error {
                if err.message.contains("cancelled") {
                    self.push_status("turn cancelled");
                } else if err.message.contains("agent_busy") {
                    self.push_status("agent is busy; wait for the current turn");
                    // Allow retrying: no in-flight anymore.
                } else {
                    self.items.push(Item::Status {
                        text: format!("error: {}", err.message),
                    });
                }
                return Effect::Noop;
            }
            let stop = env
                .result
                .as_ref()
                .and_then(|r| r["stopReason"].as_str())
                .unwrap_or("end_turn")
                .to_string();
            if stop != "end_turn" {
                self.push_status(format!("turn ended: {stop}"));
            }
            return Effect::Noop;
        }
        // session/cancel ack etc.: swallow silently.
        Effect::Noop
    }

    fn apply_update(&mut self, update: &Value) {
        let Some(kind) = update.get("sessionUpdate").and_then(|k| k.as_str()) else {
            return;
        };
        match kind {
            "agent_thought_chunk" => {
                let text = text_content(update.get("content"));
                match self.items.last_mut() {
                    Some(Item::Thinking { text: t, .. }) => t.push_str(&text),
                    _ => self.items.push(Item::Thinking {
                        text,
                        collapsed: false,
                    }),
                }
            }
            "agent_message_chunk" => {
                let text = text_content(update.get("content"));
                match self.items.last_mut() {
                    Some(Item::Agent { text: t }) => t.push_str(&text),
                    _ => self.items.push(Item::Agent { text }),
                }
            }
            "tool_call" => {
                let id = update["toolCallId"].as_str().unwrap_or("").to_string();
                let title = update["title"].as_str().unwrap_or("tool call").to_string();
                let raw = update.get("rawInput").cloned().unwrap_or(Value::Null);
                self.items.push(Item::ToolCall {
                    id,
                    title,
                    status: "running".into(),
                    detail: serde_json::to_string_pretty(&raw).unwrap_or_default(),
                    expanded: false,
                });
            }
            "tool_call_update" => {
                let id = update["toolCallId"].as_str().unwrap_or("").to_string();
                let status = update["status"].as_str().unwrap_or("completed").to_string();
                let content = update
                    .get("content")
                    .cloned()
                    .map(|c| serde_json::to_string_pretty(&c).unwrap_or_default())
                    .unwrap_or_default();
                for item in &mut self.items {
                    if let Item::ToolCall {
                        id: tid,
                        status: s,
                        detail,
                        ..
                    } = item
                    {
                        if *tid == id {
                            *s = status.clone();
                            if !content.is_empty() {
                                *detail = content.clone();
                            }
                            break;
                        }
                    }
                }
            }
            "plan" => {
                let items = update["plan"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|p| {
                                Some((
                                    p["status"].as_str() == Some("completed"),
                                    p["content"].as_str()?.to_string(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.items.push(Item::Plan { items });
            }
            _ => {}
        }
    }

    /// Toggle collapse/expand of the item under `index` ( thinking / tool call).
    pub fn toggle(&mut self, index: usize) {
        match self.items.get_mut(index) {
            Some(Item::Thinking { collapsed, .. }) => *collapsed = !*collapsed,
            Some(Item::ToolCall { expanded, .. }) => *expanded = !*expanded,
            _ => {}
        }
    }
}

fn text_content(v: Option<&Value>) -> String {
    v.and_then(|c| c["text"].as_str())
        .map(str::to_string)
        .unwrap_or_default()
}

pub enum InputAction {
    Prompt(String),
    Command(Command),
    Effect(Effect),
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(_env_type: &str, extra: Value) -> Envelope {
        Envelope::notification(
            method::SESSION_UPDATE,
            json!({"sessionId": "s1", "update": extra}),
        )
    }

    fn model_with_session() -> ChatModel {
        let mut m = ChatModel::new();
        m.session_id = Some("s1".into());
        m
    }

    #[test]
    fn submit_requires_session_and_no_inflight() {
        let mut m = ChatModel::new();
        m.session_id = None;
        assert!(matches!(m.submit_input(), InputAction::None)); // not sent
        let mut m = model_with_session();
        m.in_flight = Some(7);
        assert!(!m.can_send());
    }

    #[test]
    fn prompt_lifecycle_streams_and_stops() {
        let mut m = model_with_session();
        let action = m.submit_input_with("hello");
        assert!(matches!(action, InputAction::Prompt(_)));
        m.in_flight = Some(9);

        m.apply(&update(
            "thought",
            json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}}),
        ));
        m.apply(&update(
            "msg",
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "partial "}}),
        ));
        m.apply(&update(
            "msg2",
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "answer"}}),
        ));
        // Turn response frees the input.
        let resp = Envelope {
            id: Some(json!(9)),
            session_id: Some("s1".into()),
            method: None,
            params: None,
            result: Some(json!({"stopReason": "end_turn"})),
            error: None,
        };
        m.apply(&resp);
        assert!(m.can_send());
        assert_eq!(
            m.items,
            vec![
                Item::User {
                    text: "hello".into()
                },
                Item::Thinking {
                    text: "hmm".into(),
                    collapsed: false
                },
                Item::Agent {
                    text: "partial answer".into()
                },
            ]
        );
    }

    #[test]
    fn tool_call_card_collapses_and_updates() {
        let mut m = model_with_session();
        m.apply(&update(
            "tc",
            json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "cargo test", "rawInput": {"cmd": "cargo test"}}),
        ));
        let resp = Envelope {
            id: Some(json!(5)),
            session_id: None,
            method: None,
            params: None,
            result: None,
            error: Some(acp_wire::RpcError {
                code: -32800,
                message: "x".into(),
                data: None,
            }),
        };
        let _ = resp;
        m.apply(&update(
            "tcu",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed", "content": [{"type": "text", "text": "ok"}]}),
        ));
        assert_eq!(
            m.items[0],
            Item::ToolCall {
                id: "c1".into(),
                title: "cargo test".into(),
                status: "completed".into(),
                detail: "[\n  {\n    \"text\": \"ok\",\n    \"type\": \"text\"\n  }\n]".into(),
                expanded: false,
            }
        );
        m.toggle(0);
        assert_eq!(
            m.items[0],
            Item::ToolCall {
                id: "c1".into(),
                title: "cargo test".into(),
                status: "completed".into(),
                detail: "[\n  {\n    \"text\": \"ok\",\n    \"type\": \"text\"\n  }\n]".into(),
                expanded: true,
            }
        );
    }

    #[test]
    fn plan_renders_checklist() {
        let mut m = model_with_session();
        m.apply(&update(
            "plan",
            json!({"sessionUpdate": "plan", "plan": [
                {"content": "step one", "status": "completed"},
                {"content": "step two", "status": "in_progress"},
            ]}),
        ));
        assert_eq!(
            m.items[0],
            Item::Plan {
                items: vec![(true, "step one".into()), (false, "step two".into())]
            }
        );
    }

    #[test]
    fn esc_cancels_inflight_turn() {
        let mut m = model_with_session();
        assert_eq!(m.key_esc(), Effect::Noop);
        m.in_flight = Some(3);
        assert_eq!(
            m.key_esc(),
            Effect::CancelTurn {
                session_id: "s1".into()
            }
        );
    }

    #[test]
    fn permission_reverse_rpc_and_answer() {
        let mut m = model_with_session();
        let req = Envelope {
            id: Some(json!("perm-1")),
            session_id: Some("s1".into()),
            method: Some(method::SESSION_REQUEST_PERMISSION.into()),
            params: Some(json!({
                "sessionId": "s1",
                "toolCall": {"toolCallId": "c9", "title": "rm -rf x", "kind": "execute", "status": "pending"},
                "options": [
                    {"optionId": "a", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "b", "name": "Reject", "kind": "reject_once"},
                ],
            })),
            result: None,
            error: None,
        };
        m.apply(&req);
        assert!(m.pending_permission.is_some());
        assert_eq!(
            m.answer_permission(1),
            Effect::AnswerPermission {
                request_id: json!("perm-1"),
                outcome: "reject_once"
            }
        );
        assert!(m.pending_permission.is_none());
    }

    #[test]
    fn agent_busy_error_frees_input_with_notice() {
        let mut m = model_with_session();
        m.in_flight = Some(4);
        let resp = Envelope {
            id: Some(json!(4)),
            session_id: Some("s1".into()),
            method: None,
            params: None,
            result: None,
            error: Some(acp_wire::RpcError {
                code: -32000,
                message: "turn.agent_busy".into(),
                data: None,
            }),
        };
        m.apply(&resp);
        assert!(m.can_send());
        assert!(matches!(m.items.last(), Some(Item::Status { .. })));
    }

    #[test]
    fn command_parsing() {
        assert_eq!(
            parse_command("/model k2"),
            Some(Command::Model("k2".into()))
        );
        assert_eq!(
            parse_command("/mode vscode"),
            Some(Command::Mode("vscode".into()))
        );
        assert_eq!(parse_command("/sessions"), Some(Command::Sessions));
        assert_eq!(
            parse_command("/resume s9"),
            Some(Command::Resume("s9".into()))
        );
        assert_eq!(parse_command("/quit"), Some(Command::Quit));
        assert_eq!(
            parse_command("/bogus"),
            Some(Command::Unknown("/bogus".into()))
        );
        assert_eq!(parse_command("hello"), None);
    }

    impl ChatModel {
        fn submit_input_with(&mut self, text: &str) -> InputAction {
            self.input = text.to_string();
            self.submit_input()
        }
    }
}
