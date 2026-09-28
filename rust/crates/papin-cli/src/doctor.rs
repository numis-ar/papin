use crate::config::CliConfig;
use crate::gateway::Gateway;
use crate::http::{HttpClient, Result};

pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub hint: Option<String>,
}

impl Check {
    fn ok(name: &str, detail: String) -> Check {
        Check {
            name: name.into(),
            ok: true,
            detail,
            hint: None,
        }
    }

    fn fail(name: &str, detail: String, hint: &str) -> Check {
        Check {
            name: name.into(),
            ok: false,
            detail,
            hint: Some(hint.into()),
        }
    }
}

/// `papin doctor` (§6): WG heuristic → TCP → WS+initialize → registry.
pub async fn run(config: &CliConfig) -> Result<i32> {
    let mut checks = Vec::new();

    // 1. WireGuard interface heuristic: is the `wg` tool present and does an
    // interface exist? (Best-effort; unprivileged `wg` may not see anything.)
    match std::process::Command::new("wg").arg("show").output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.trim().is_empty() {
                checks.push(Check::fail(
                    "wireguard",
                    "no WireGuard interfaces configured".into(),
                    "run `wg-quick up wg0` (see operations.md)",
                ));
            } else {
                checks.push(Check::ok("wireguard", "interfaces present".into()));
            }
        }
        _ => checks.push(Check::ok(
            "wireguard",
            "`wg` unavailable; assuming userspace tunnel or direct network".into(),
        )),
    }

    // 2. TCP reachability of the gateway HTTP endpoint.
    // Cheap probe target: the healthz endpoint is unauthenticated.
    let tcp_ok =
        HttpClient::new(&config.url, &config.token).map(|c| (c.host().to_string(), c.port()));
    let tcp = match tcp_ok {
        Ok((host, port)) => {
            match tokio::time::timeout(
                std::time::Duration::from_secs(3),
                tokio::net::TcpStream::connect((host.as_str(), port)),
            )
            .await
            {
                Ok(Ok(_)) => Check::ok("gateway tcp", format!("{host}:{port} reachable")),
                Ok(Err(e)) => Check::fail(
                    "gateway tcp",
                    format!("{host}:{port}: {e}"),
                    "is the WireGuard tunnel up? try `wg-quick up wg0`; check the gateway address in ~/.config/papin-cli/config.toml",
                ),
                Err(_) => Check::fail(
                    "gateway tcp",
                    format!("{host}:{port} timed out"),
                    "check the tunnel and the gateway address in ~/.config/papin-cli/config.toml",
                ),
            }
        }
        Err(e) => Check::fail("gateway tcp", e.to_string(), "fix the `url` in config.toml"),
    };
    let tcp_ok = tcp.ok;
    checks.push(tcp);
    if !tcp_ok {
        print_checks(&checks);
        return Ok(1);
    }

    // 3. Registry endpoint (auth + provider).
    let gateway = Gateway::new(config)?;
    match gateway.list_agents().await {
        Ok(agents) => checks.push(Check::ok(
            "registry",
            format!("{} agent(s) visible", agents.len()),
        )),
        Err(e) => checks.push(Check::fail(
            "registry",
            e.to_string(),
            "check the device token in ~/.config/papin-cli/config.toml (re-run add-peer to reissue)",
        )),
    }

    // 4. WS handshake + initialize against the default/first agent.
    let agent = match &config.default_agent {
        Some(a) => Some(a.clone()),
        None => gateway
            .list_agents()
            .await
            .ok()
            .and_then(|mut agents| agents.pop().map(|a| a.id)),
    };
    match agent {
        None => checks.push(Check::ok(
            "acp initialize",
            "no agents yet; create one with `papin agent new <name>`".into(),
        )),
        Some(agent_id) => {
            let mut client = match crate::acp::AcpClient::connect(
                &config.ws_url(&agent_id),
                &config.token,
            )
            .await
            {
                Ok(c) => c,
                Err(e) => {
                    checks.push(Check::fail(
                        "acp initialize",
                        format!("ws connect: {e}"),
                        "check that the gateway serves /acp/agents/{id} and the token is valid",
                    ));
                    print_checks(&checks);
                    return Ok(1);
                }
            };
            match client.initialize().await {
                Ok(resp) if resp.error.is_none() => {
                    checks.push(Check::ok("acp initialize", format!("agent {agent_id} ok")))
                }
                Ok(resp) => checks.push(Check::fail(
                    "acp initialize",
                    resp.error.map(|e| e.message).unwrap_or_default(),
                    "the agent may be down; the gateway respawns on next connect",
                )),
                Err(e) => checks.push(Check::fail(
                    "acp initialize",
                    e.to_string(),
                    "check gateway logs",
                )),
            }
        }
    }

    print_checks(&checks);
    Ok(if checks.iter().all(|c| c.ok) { 0 } else { 1 })
}

fn print_checks(checks: &[Check]) {
    for c in checks {
        let mark = if c.ok { "✓" } else { "✗" };
        println!("{mark} {}: {}", c.name, c.detail);
        if let Some(hint) = &c.hint {
            println!("  hint: {hint}");
        }
    }
}
