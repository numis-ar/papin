use crate::acp::{AcpClient, AcpEvent};
use crate::config::CliConfig;
use crate::gateway::AgentEntry;
use crate::http::Result;
use crate::model::{ChatModel, Command, Effect, InputAction, Item};
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures_util::StreamExt;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde_json::json;
use std::io;
use std::time::Duration;

/// Terminal chat (§6). The model lives in `model.rs`; this file only renders
/// it and translates keys/events into model calls and ACP I/O.
pub async fn run(config: CliConfig, agent: AgentEntry, resume: Option<String>) -> Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = chat_loop(&mut terminal, config, agent, resume).await;

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

struct Io {
    client: AcpClient,
}

async fn chat_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: CliConfig,
    agent: AgentEntry,
    resume: Option<String>,
) -> Result<()> {
    let mut model = ChatModel::new();
    let mut io = match connect(&config, &agent.id, &mut model, resume).await {
        Ok(io) => Some(io),
        Err(e) => {
            model.push_status(format!("connect failed: {e}"));
            None
        }
    };

    let mut keys = EventStream::new();
    let mut needs_redraw = true;
    loop {
        if needs_redraw {
            terminal.draw(|f| draw(f, &model))?;
            needs_redraw = false;
        }
        tokio::select! {
            Some(Ok(ev)) = keys.next() => {
                if let TermEvent::Key(key) = ev {
                    needs_redraw = true;
                    if handle_key(key, &mut model, &mut io, &config).await? {
                        return Ok(());
                    }
                }
            }
            Some(event) = async { match io.as_mut() { Some(io) => io.client.next_event().await, None => None } }, if io.is_some() => {
                needs_redraw = true;
                match event {
                    AcpEvent::Notification(env) | AcpEvent::Response(env) => {
                        let effect = model.apply(&env);
                        run_effect(effect, &mut model, &mut io).await?;
                    }
                    AcpEvent::ReverseRequest(env) => {
                        let effect = model.apply(&env);
                        run_effect(effect, &mut model, &mut io).await?;
                    }
                    AcpEvent::Disconnected => {
                        model.push_status("connection lost; reconnecting…");
                        io = match reconnect(&config, &agent.id, &mut model).await {
                            Ok(new_io) => Some(new_io),
                            Err(e) => {
                                model.push_status(format!("reconnect failed: {e}; retrying on next event"));
                                None
                            }
                        };
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

/// Initial connect + initialize (+ optional session resume).
async fn connect(
    config: &CliConfig,
    agent_id: &str,
    model: &mut ChatModel,
    resume: Option<String>,
) -> Result<Io> {
    let mut client = AcpClient::connect(&config.ws_url(agent_id), &config.token).await?;
    client.initialize().await?;
    model.status = format!("agent {agent_id} ready");
    match resume {
        Some(sid) => {
            client.session_load(&sid).await?;
            model.session_id = Some(sid.clone());
            model.push_status(format!("resumed session {sid}"));
        }
        None => {
            let sid = client.session_new().await?;
            model.session_id = Some(sid);
        }
    }
    Ok(Io { client })
}

/// Reconnect healing (§6): new WS, initialize, session/load for the current
/// session so the transcript resumes.
async fn reconnect(config: &CliConfig, agent_id: &str, model: &mut ChatModel) -> Result<Io> {
    let mut client = AcpClient::connect(&config.ws_url(agent_id), &config.token).await?;
    client.initialize().await?;
    if let Some(sid) = model.session_id.clone() {
        client.session_load(&sid).await?;
        model.push_status(format!("reconnected; resumed session {sid}"));
    }
    model.in_flight = None;
    model.status = format!("agent {agent_id} ready (reconnected)");
    Ok(Io { client })
}

/// Returns true when the app should quit.
async fn handle_key(
    key: KeyEvent,
    model: &mut ChatModel,
    io: &mut Option<Io>,
    config: &CliConfig,
) -> Result<bool> {
    match key.code {
        KeyCode::Esc => {
            let effect = model.key_esc();
            run_effect(effect, model, io).await?;
        }
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::ALT)
                || key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            model.input.push('\n');
        }
        KeyCode::Enter => {
            if model.pending_permission.is_some() {
                // Enter answers with the first option.
                let effect = model.answer_permission(0);
                run_effect(effect, model, io).await?;
                return Ok(false);
            }
            match model.submit_input() {
                InputAction::Prompt(text) => {
                    if let Some(io) = io.as_mut() {
                        if let Some(sid) = model.session_id.clone() {
                            let id = io.client.id();
                            model.in_flight = Some(id);
                            io.client.prompt(&sid, &text, id).await?;
                        }
                    }
                }
                InputAction::Command(cmd) => run_command(cmd, model, io, config).await?,
                InputAction::Effect(Effect::Quit) => return Ok(true),
                _ => {}
            }
        }
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c == 'c' => {
            return Ok(true);
        }
        KeyCode::Char(c) => {
            // Number keys answer a pending permission.
            if model.pending_permission.is_some() && c.is_ascii_digit() {
                let effect = model.answer_permission(c as usize - '1' as usize);
                run_effect(effect, model, io).await?;
                return Ok(false);
            }
            model.input.push(c);
        }
        KeyCode::Backspace => {
            model.input.pop();
        }
        KeyCode::Tab => {
            let last = model
                .items
                .iter()
                .rposition(|i| matches!(i, Item::Thinking { .. } | Item::ToolCall { .. }));
            if let Some(index) = last {
                model.toggle(index);
            }
        }
        _ => {}
    }
    Ok(false)
}

async fn run_command(
    cmd: Command,
    model: &mut ChatModel,
    io: &mut Option<Io>,
    _config: &CliConfig,
) -> Result<()> {
    match cmd {
        Command::Model(m) => {
            if let (Some(io), Some(sid)) = (io.as_mut(), model.session_id.clone()) {
                let resp = io
                    .client
                    .request("session/set_model", json!({"sessionId": sid, "modelId": m}))
                    .await?;
                if resp.error.is_none() {
                    model.model = Some(m.clone());
                    model.push_status(format!("model set to {m}"));
                }
            }
        }
        Command::Mode(m) => {
            if let (Some(io), Some(sid)) = (io.as_mut(), model.session_id.clone()) {
                let resp = io
                    .client
                    .request("session/set_mode", json!({"sessionId": sid, "modeId": m}))
                    .await?;
                if resp.error.is_none() {
                    model.mode = Some(m.clone());
                    model.push_status(format!("mode set to {m}"));
                }
            }
        }
        Command::Sessions => {
            if let Some(io) = io.as_mut() {
                match io.client.session_list().await {
                    Ok(sessions) => {
                        if sessions.is_empty() {
                            model.push_status("no sessions");
                        }
                        for (sid, title) in sessions {
                            model.items.push(Item::Status {
                                text: format!("session {sid} — {title}  (/resume {sid})"),
                            });
                        }
                    }
                    Err(e) => model.push_status(format!("session/list failed: {e}")),
                }
            }
        }
        Command::Resume(sid) => {
            if let Some(io) = io.as_mut() {
                match io.client.session_load(&sid).await {
                    Ok(()) => {
                        model.session_id = Some(sid.clone());
                        model.items.clear();
                        model.push_status(format!("resumed session {sid}"));
                    }
                    Err(e) => model.push_status(format!("resume failed: {e}")),
                }
            }
        }
        Command::NewSession => {
            if let Some(io) = io.as_mut() {
                match io.client.session_new().await {
                    Ok(sid) => {
                        model.session_id = Some(sid.clone());
                        model.items.clear();
                        model.push_status(format!("new session {sid}"));
                    }
                    Err(e) => model.push_status(format!("session/new failed: {e}")),
                }
            }
        }
        Command::CloseSession => {
            if let (Some(io), Some(sid)) = (io.as_mut(), model.session_id.clone()) {
                let _ = io
                    .client
                    .request("session/close", json!({"sessionId": sid}))
                    .await;
                model.session_id = None;
                model.push_status("session closed");
            }
        }
        Command::DeleteSession => {
            if let (Some(io), Some(sid)) = (io.as_mut(), model.session_id.clone()) {
                let _ = io
                    .client
                    .request("session/delete", json!({"sessionId": sid}))
                    .await;
                model.session_id = None;
                model.items.clear();
                model.push_status("session deleted");
            }
        }
        Command::Help => {
            model.push_status(
                "enter: send · alt/shift+enter: newline · esc: cancel turn · /model /mode /sessions /resume <id> /new /close /delete /quit",
            );
        }
        Command::Quit => {}
        Command::Unknown(u) => model.push_status(format!("unknown command {u}; try /help")),
    }
    Ok(())
}

async fn run_effect(effect: Effect, model: &mut ChatModel, io: &mut Option<Io>) -> Result<()> {
    match effect {
        Effect::CancelTurn { session_id } => {
            if let Some(io) = io.as_mut() {
                model.push_status("cancelling…");
                let _ = io.client.cancel(&session_id).await;
            }
        }
        Effect::AnswerPermission {
            request_id,
            outcome,
        } => {
            if let Some(io) = io.as_mut() {
                io.client
                    .respond(request_id, json!({"outcome": outcome}))
                    .await?;
                model.push_status(format!("answered: {outcome}"));
            }
        }
        Effect::Quit => {}
        Effect::Noop => {}
    }
    Ok(())
}

fn draw(f: &mut Frame, model: &ChatModel) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    let transcript: Vec<ListItem> = model
        .items
        .iter()
        .map(|item| ListItem::new(render_item(item)))
        .collect();
    let list = List::new(transcript)
        .block(Block::default().borders(Borders::ALL).title("papin"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    let mut state = ListState::default();
    if !model.items.is_empty() {
        state.select(Some(model.items.len() - 1));
    }
    f.render_stateful_widget(list, chunks[0], &mut state);

    let input = Paragraph::new(model.input.as_str())
        .block(Block::default().borders(Borders::ALL).title("input"))
        .wrap(Wrap { trim: false });
    f.render_widget(input, chunks[1]);
    f.set_cursor_position((chunks[1].x + model.input.len() as u16 + 1, chunks[1].y + 1));

    let status = if model.in_flight.is_some() {
        format!("{} · turn in flight (esc to cancel)", model.status)
    } else {
        model.status.clone()
    };
    f.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn render_item<'a>(item: &Item) -> Text<'a> {
    match item {
        Item::User { text } => {
            Text::styled(format!("❯ {text}"), Style::default().fg(Color::Yellow))
        }
        Item::Thinking { text, collapsed } => {
            let head = if *collapsed {
                "▼ thinking"
            } else {
                "▶ thinking"
            };
            if *collapsed {
                Text::styled(head.to_string(), Style::default().fg(Color::DarkGray))
            } else {
                Text::from(vec![
                    Line::styled(head.to_string(), Style::default().fg(Color::DarkGray)),
                    Line::from(format!("  {text}")),
                ])
            }
        }
        Item::ToolCall {
            title,
            status,
            detail,
            expanded,
            ..
        } => {
            let head = format!("{} [{}] {title}", if *expanded { "▶" } else { "▼" }, status);
            let mut lines = vec![Line::styled(
                head,
                Style::default().fg(if status == "completed" {
                    Color::Green
                } else {
                    Color::Cyan
                }),
            )];
            if *expanded {
                for l in detail.lines() {
                    lines.push(Line::from(format!("  {l}")));
                }
            }
            Text::from(lines)
        }
        Item::Plan { items } => Text::from(
            items
                .iter()
                .map(|(done, content)| {
                    Line::from(format!("  [{}] {content}", if *done { "x" } else { " " }))
                })
                .collect::<Vec<_>>(),
        ),
        Item::Agent { text } => Text::from(text.clone()),
        Item::Status { text } => {
            Text::styled(format!("— {text}"), Style::default().fg(Color::DarkGray))
        }
    }
}
