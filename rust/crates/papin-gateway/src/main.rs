//! `papin-gateway` binary: serve the gateway, or build catalog base images
//! (`mkbase`, §5.4/operations.md).

use clap::{Parser, Subcommand};
use papin_gateway::provider::{AgentProvider, FakeProvider, SpawnProvider};
use papin_gateway::{
    BootstrapEngine, Catalog, EnrollConfig, FakeBootstrap, GatewayConfig, SpawnBootstrap,
    TokenStore,
};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "papin-gateway", about = "Remote Papin agent harness gateway")]
struct Cli {
    /// Path to gateway.toml.
    #[arg(long, default_value = "/etc/papin/gateway.toml")]
    config: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Build a catalog base image: copy <source-dir> into
    /// <bases_dir>/<name> and drop the base marker.
    Mkbase {
        name: String,
        /// Source tree (minimal OS + node runtime + kimi install + CA certs).
        source_dir: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("papin-gateway: {e}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config = GatewayConfig::load(std::path::Path::new(&cli.config))?;

    if let Some(Commands::Mkbase { name, source_dir }) = cli.command {
        return mkbase(&config, &name, std::path::Path::new(&source_dir));
    }

    let catalog: Catalog = config.catalog.clone();
    let tokens = TokenStore::load(&config.tokens_file, config.idle_timeout())?;

    let (provider, bootstrap): (Arc<dyn AgentProvider>, Arc<dyn BootstrapEngine>) =
        match config.provider.as_str() {
            "fake" => (
                Arc::new(FakeProvider::new(
                    config.agents_root.clone(),
                    config.fake_script.clone(),
                    catalog.clone(),
                )),
                Arc::new(FakeBootstrap::new(
                    config.agents_root.clone(),
                    config.seeds_dir.clone(),
                )),
            ),
            "spawn" => (
                Arc::new(SpawnProvider::new(
                    config.agents_root.clone(),
                    config.socket_path.clone(),
                    catalog.clone(),
                )),
                Arc::new(SpawnBootstrap::new(
                    config.agents_root.clone(),
                    config.bases_dir.clone(),
                    config.seeds_dir.clone(),
                )),
            ),
            other => {
                return Err(
                    format!("unknown provider {other:?} (expected \"fake\" or \"spawn\")").into(),
                )
            }
        };

    let enroll = EnrollConfig {
        peers_dir: config.wg_peers_dir.clone(),
        gateway_ip: config.wg_gateway_ip.clone(),
        client_pool: config.wg_client_pool.clone(),
        helper: config.enroll_helper.clone(),
    };

    let state = papin_gateway::server::make_state_full(
        provider,
        bootstrap,
        catalog,
        config.idle_timeout(),
        Arc::new(tokens),
        enroll,
    );
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "papin_gateway=info".into()),
        )
        .init();
    println!(
        "papin-gateway listening on {} (provider = {})",
        config.listen, config.provider
    );
    papin_gateway::server::serve(state, &config.listen).await?;
    Ok(())
}

fn mkbase(
    config: &GatewayConfig,
    name: &str,
    source_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !papin_gateway::provider::validate_agent_id(name) {
        return Err(
            format!("invalid base name {name:?}: must match ^[a-z0-9][a-z0-9-]{{0,63}}$").into(),
        );
    }
    if !source_dir.is_dir() {
        return Err(format!("source dir {} does not exist", source_dir.display()).into());
    }
    let dest = config.bases_dir.join(name);
    if dest.exists() {
        return Err(format!("base image {name:?} already exists at {}", dest.display()).into());
    }
    std::fs::create_dir_all(&config.bases_dir)?;
    papin_gateway::bootstrap::copy_dir_all(source_dir, &dest)?;
    std::fs::write(
        dest.join(".papin-base"),
        format!(
            "name = {name:?}\ncreated_at = {}\n",
            papin_gateway::config::now_secs()
        ),
    )?;
    println!(
        "base image {name:?} created at {} — publish it in the catalog (gateway.toml) and build agents from it",
        dest.display()
    );
    Ok(())
}
