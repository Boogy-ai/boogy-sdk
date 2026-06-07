mod build;
mod deploy;
mod manage;
mod provision;

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "boogy", about = "Boogy service management CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Host URL
    #[arg(long, default_value = "http://localhost:3000", global = true)]
    host: String,

    /// Bearer token for authenticated commands (falls back to BOOGY_TOKEN)
    #[arg(long, env = "BOOGY_TOKEN", global = true)]
    token: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Build a service to wasm32-wasip2
    Build {
        /// Path to the service project directory
        path: String,
    },
    /// Deploy a service to the host (publish + provision sugar)
    Deploy {
        /// Path to the deployment manifest (.boogy.toml)
        manifest: String,
    },
    /// Publish a module (immutable, versioned wasm+manifest artifact)
    Publish {
        /// Path to the manifest (.boogy.toml)
        manifest: String,
        /// Also provision the publisher's own service from it
        #[arg(long)]
        provision: bool,
    },
    /// Provision a service instance from a published module
    Provision {
        /// Module ref: boogy://owner/modules/name@version
        module: String,
        /// Service id for the new instance
        service_id: String,
        /// Path to a TOML file of sparse manifest overrides
        #[arg(long)]
        overrides: Option<String>,
    },
    /// Upgrade a provisioned service to a newer module version
    Upgrade {
        /// Service id to upgrade
        service_id: String,
        /// Target module version
        #[arg(long)]
        to: String,
    },
    /// List deployed services (requires admin scope)
    List,
    /// Remove a deployed service (requires admin scope)
    Remove {
        /// Owner user ID
        owner: String,
        /// Service ID to remove
        service_id: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build { path } => build::run(&path).await?,
        Commands::Deploy { manifest } => {
            let token = resolve_token(&cli.token)?;
            deploy::run(&cli.host, &token, &manifest).await?
        }
        Commands::Publish {
            manifest,
            provision,
        } => {
            let token = resolve_token(&cli.token)?;
            provision::publish(&cli.host, &token, &manifest, provision).await?
        }
        Commands::Provision {
            module,
            service_id,
            overrides,
        } => {
            let token = resolve_token(&cli.token)?;
            provision::provision(
                &cli.host,
                &token,
                &module,
                &service_id,
                overrides.as_deref(),
            )
            .await?
        }
        Commands::Upgrade { service_id, to } => {
            let token = resolve_token(&cli.token)?;
            provision::upgrade(&cli.host, &token, &service_id, &to).await?
        }
        Commands::List => {
            let token = resolve_token(&cli.token)?;
            manage::list(&cli.host, &token).await?
        }
        Commands::Remove { owner, service_id } => {
            let token = resolve_token(&cli.token)?;
            manage::remove(&cli.host, &token, &owner, &service_id).await?
        }
    }

    Ok(())
}

/// Resolve the bearer token from `--token` / `BOOGY_TOKEN`, erroring clearly
/// if neither is set.
fn resolve_token(token: &Option<String>) -> anyhow::Result<String> {
    token
        .clone()
        .context("set --token or BOOGY_TOKEN")
}
