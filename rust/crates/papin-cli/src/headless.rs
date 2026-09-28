use crate::acp::{AcpClient, AcpEvent};
use crate::config::CliConfig;
use crate::http::Result;
use acp_wire::Envelope;
use serde_json::json;

/// `papin -p "prompt"`: one full turn, final response on stdout, progress as
/// JSONL on stderr, exit code reflects errors (§6).
pub async fn run(
    config: &CliConfig,
    agent_id: &str,
    prompt: &str,
    resume: Option<&str>,
) -> Result<i32> {
    let mut client = AcpClient::connect(&config.ws_url(agent_id), &config.token).await?;
    let init = client.initialize().await?;
    if let Some(err) = init.error {
        eprintln!("initialize failed: {}", err.message);
        return Ok(2);
    }
    eprintln!("[papin] initialized against agent {agent_id}");

    let session_id = match resume {
        Some(sid) => {
            client.session_load(sid).await?;
            eprintln!("[papin] resumed session {sid}");
            sid.to_string()
        }
        None => {
            let sid = client.session_new().await?;
            eprintln!("[papin] session {sid}");
            sid
        }
    };

    let prompt_id = client.id();
    let mut pending = std::collections::HashMap::new();
    pending.insert(serde_json::to_string(&json!(prompt_id))?, prompt_id);
    client.prompt(&session_id, prompt, prompt_id).await?;
    eprintln!("[papin] prompt sent; streaming…");

    let mut final_text = String::new();
    let mut buffer = String::new(); // current agent message chunk accumulator
    loop {
        match client.next_event().await {
            Some(AcpEvent::Notification(env)) => {
                handle_notification(&env, &mut buffer, &mut final_text, true);
            }
            Some(AcpEvent::Response(env)) => {
                if env.id == Some(json!(prompt_id)) {
                    if let Some(err) = env.error {
                        eprintln!("[papin] turn error: {}", err.message);
                        println!("{}", final_text.trim_end());
                        return Ok(1);
                    }
                    break;
                }
            }
            Some(AcpEvent::ReverseRequest(env)) => {
                // Headless: auto-answer permission requests (allow_once) so a
                // turn never stalls; the request is logged on stderr.
                if env.method.as_deref() == Some(acp_wire::method::SESSION_REQUEST_PERMISSION) {
                    eprintln!(
                        "[papin] permission request {:?} auto-answered: allow_once",
                        env.params
                            .as_ref()
                            .and_then(|p| p["toolCall"]["title"].as_str())
                    );
                    client
                        .respond(
                            env.id.clone().unwrap_or(json!(null)),
                            json!({"outcome": "allow_once"}),
                        )
                        .await?;
                }
            }
            Some(AcpEvent::Disconnected) => {
                eprintln!("[papin] connection lost");
                return Ok(2);
            }
            None => return Ok(2),
        }
    }
    let out = if final_text.is_empty() {
        buffer
    } else {
        final_text
    };
    println!("{}", out.trim_end());
    Ok(0)
}

/// Shared session/update handling; `log` enables JSONL progress on stderr.
pub fn handle_notification(
    env: &Envelope,
    buffer: &mut String,
    final_text: &mut String,
    log: bool,
) {
    if env.method.as_deref() != Some(acp_wire::method::SESSION_UPDATE) {
        return;
    }
    let Some(update) = env.params.as_ref().and_then(|p| p.get("update")) else {
        return;
    };
    let kind = update["sessionUpdate"].as_str().unwrap_or("");
    let text = update["content"]["text"].as_str().unwrap_or("");
    match kind {
        "agent_message_chunk" => {
            buffer.push_str(text);
            if buffer.ends_with('\n') {
                final_text.push_str(buffer);
                buffer.clear();
            }
        }
        "agent_thought_chunk" | "tool_call" | "tool_call_update" | "plan" => {
            if log {
                eprintln!("{}", serde_json::to_string(update).unwrap_or_default());
            }
        }
        _ => {}
    }
}
