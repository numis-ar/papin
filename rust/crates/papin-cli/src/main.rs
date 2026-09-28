mod acp;
mod config;
mod doctor;
mod gateway;
mod headless;
mod http;
mod model;
mod tui;

use clap::{Parser, Subcommand};
use config::CliConfig;
use gateway::{AgentEntry, Gateway};
use http::Result;

#[derive(Parser)]
#[command(
    name = "papin",
    about = "Terminal client for the remote Papin agent harness"
)]
struct Cli {
    /// Skip the agent picker and connect to this agent.
    #[arg(long)]
    agent: Option<String>,

    /// Resume this session (session/load replay) instead of creating a new one.
    #[arg(long)]
    resume: Option<String>,

    /// Headless mode: send one prompt, print the final response, exit.
    #[arg(short = 'p')]
    prompt: Option<String>,

    /// Path to a config file (default ~/.config/papin-cli/config.toml).
    #[arg(long)]
    config: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Manage agents.
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Diagnose gateway connectivity.
    Doctor,
}

#[derive(Subcommand, Debug)]
enum AgentAction {
    /// Create an agent and connect to it.
    New {
        name: String,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        seed: Option<String>,
    },
    /// Delete an agent (refuses while attached unless --force).
    Rm {
        id: String,
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Some(path) = &cli.config {
        std::env::set_var("PAPIN_CLI_CONFIG", path);
    }
    let config = CliConfig::load();
    let code = match run(cli, config).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("papin: {e}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli, config: CliConfig) -> Result<i32> {
    if let Some(Commands::Doctor) = cli.command {
        return doctor::run(&config).await;
    }

    let gateway = Gateway::new(&config)?;

    if let Some(Commands::Agent { action }) = cli.command {
        return match action {
            AgentAction::New { name, base, seed } => {
                agent_new(&gateway, &config, &name, base, seed).await
            }
            AgentAction::Rm { id, force } => agent_rm(&gateway, &id, force).await,
        };
    }

    let agent = match cli.agent.or(config.default_agent.clone()) {
        Some(id) => AgentEntry {
            id,
            name: String::new(),
            state: String::new(),
        },
        None => pick_agent(&gateway).await?,
    };

    if let Some(prompt) = cli.prompt {
        return headless::run(&config, &agent.id, &prompt, cli.resume.as_deref()).await;
    }

    tui::run(config, agent, cli.resume).await?;
    Ok(0)
}

/// `papin agent new`: choose base/seed from the catalog when not given.
async fn agent_new(
    gateway: &Gateway,
    config: &CliConfig,
    name: &str,
    base: Option<String>,
    seed: Option<String>,
) -> Result<i32> {
    let catalog = gateway.config_catalog().await?;
    let base = match base {
        Some(b) => b,
        None => match catalog.bases.as_slice() {
            [only] => only.clone(),
            many => {
                eprintln!("available base images:");
                for b in many {
                    eprintln!("  {b}");
                }
                return Err("choose one with --base <name>".to_string().into());
            }
        },
    };
    if !catalog.bases.contains(&base) {
        return Err(
            format!("unknown base {base:?}; run `papin agent new` without --base to list").into(),
        );
    }
    if let Some(s) = &seed {
        if !catalog.seeds.iter().any(|x| x == s) {
            return Err(format!(
                "unknown seed {s:?}; available: {}",
                catalog.seeds.join(", ")
            )
            .into());
        }
    }
    let agent = gateway.create_agent(name, &base, seed.as_deref()).await?;
    println!("created agent {} ({})", agent.id, agent.name);
    println!("connecting… (Ctrl-C to cancel)");
    tui::run(config.clone(), agent, None).await?;
    Ok(0)
}

async fn agent_rm(gateway: &Gateway, id: &str, force: bool) -> Result<i32> {
    gateway.delete_agent(id, force).await?;
    println!("deleted agent {id}");
    Ok(0)
}

/// Agent picker (§6): list agents with state badges; simple numbered choice.
/// Falls back to creating an agent when the registry is empty.
async fn pick_agent(gateway: &Gateway) -> Result<AgentEntry> {
    let agents = gateway.list_agents().await?;
    if agents.is_empty() {
        eprintln!("no agents yet; creating one…");
        return agent_create_interactive(gateway).await;
    }
    eprintln!("agents:");
    for (i, a) in agents.iter().enumerate() {
        eprintln!("  {}. {} [{}] — {}", i + 1, a.name, a.state, a.id);
    }
    eprint!("choose 1..{} (or `n` to create): ", agents.len());
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let choice = line.trim();
    if choice.eq_ignore_ascii_case("n") {
        return agent_create_interactive(gateway).await;
    }
    let idx: usize = choice.parse().map_err(|_| "invalid choice")?;
    agents
        .get(idx.saturating_sub(1))
        .cloned()
        .ok_or_else(|| "invalid choice".to_string().into())
}

async fn agent_create_interactive(gateway: &Gateway) -> Result<AgentEntry> {
    eprint!("agent name: ");
    let mut name = String::new();
    std::io::stdin().read_line(&mut name)?;
    let name = name.trim();
    if name.is_empty() {
        return Err("agent name required".into());
    }
    let catalog = gateway.config_catalog().await?;
    eprintln!("base images: {}", catalog.bases.join(", "));
    eprint!("base: ");
    let mut base = String::new();
    std::io::stdin().read_line(&mut base)?;
    let base = base.trim().to_string();
    agent_create(gateway, name, &base, None).await
}

async fn agent_create(
    gateway: &Gateway,
    name: &str,
    base: &str,
    seed: Option<&str>,
) -> Result<AgentEntry> {
    gateway.create_agent(name, base, seed).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parsing() {
        let cli = Cli::try_parse_from(["papin"]).unwrap();
        assert!(cli.agent.is_none() && cli.prompt.is_none());

        let cli = Cli::try_parse_from(["papin", "--agent", "a1", "-p", "hello"]).unwrap();
        assert_eq!(cli.agent.as_deref(), Some("a1"));
        assert_eq!(cli.prompt.as_deref(), Some("hello"));

        let cli = Cli::try_parse_from(["papin", "--agent", "a1", "--resume", "s9"]).unwrap();
        assert_eq!(cli.resume.as_deref(), Some("s9"));

        let cli = Cli::try_parse_from(["papin", "agent", "new", "mine", "--base", "fake"]).unwrap();
        match cli.command {
            Some(Commands::Agent {
                action: AgentAction::New { name, base, seed },
            }) => {
                assert_eq!(name, "mine");
                assert_eq!(base.as_deref(), Some("fake"));
                assert!(seed.is_none());
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        let cli = Cli::try_parse_from(["papin", "agent", "rm", "a1", "--force"]).unwrap();
        match cli.command {
            Some(Commands::Agent {
                action: AgentAction::Rm { id, force },
            }) => {
                assert_eq!(id, "a1");
                assert!(force);
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        assert!(Cli::try_parse_from(["papin", "agent", "bogus"]).is_err());
    }
}
