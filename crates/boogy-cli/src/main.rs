mod build;
mod deploy;
mod manage;
mod provision;
mod skills;

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "boogy", about = "Boogy service management CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Host URL (falls back to BOOGY_HOST_URL)
    #[arg(long, env = "BOOGY_HOST_URL", default_value = "http://localhost:3000", global = true)]
    host: String,

    /// Bearer token for authenticated commands (falls back to BOOGY_TOKEN)
    #[arg(long, env = "BOOGY_TOKEN", global = true)]
    token: Option<String>,
}

#[derive(Subcommand, Debug)]
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
    /// Vendor the Boogy skills into this project (.claude/skills/boogy)
    /// so coding agents pick them up automatically
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
}

#[derive(Subcommand, Debug)]
enum SkillsAction {
    /// Install the skills into the current project
    Install {
        /// Destination directory (default: .claude/skills/boogy)
        #[arg(long)]
        dest: Option<String>,
    },
    /// Refresh a previously installed copy
    Update {
        /// Destination directory (default: .claude/skills/boogy)
        #[arg(long)]
        dest: Option<String>,
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
        Commands::Skills { action } => match action {
            SkillsAction::Install { dest } => skills::run(dest.as_deref(), "installed")?,
            SkillsAction::Update { dest } => skills::run(dest.as_deref(), "updated")?,
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: BOOGY_HOST_URL env var must be picked up by the --host arg,
    /// and an explicit --host flag must win over the env var.
    ///
    /// Both assertions are in ONE test to avoid a parallel-test race on the
    /// shared env var.
    #[test]
    fn host_arg_reads_boogy_host_url_env_and_flag_wins() {
        // --- env var is picked up ---
        // Safety: single-threaded test binary (no other test touches BOOGY_HOST_URL).
        unsafe {
            std::env::set_var("BOOGY_HOST_URL", "https://env.example.com");
        }
        let cli = Cli::try_parse_from(["boogy", "list"]).expect("parse");
        assert_eq!(
            cli.host, "https://env.example.com",
            "BOOGY_HOST_URL env var should set the host"
        );

        // --- explicit --host flag overrides the env var ---
        let cli = Cli::try_parse_from(["boogy", "--host", "https://flag.example.com", "list"])
            .expect("parse");
        assert_eq!(
            cli.host, "https://flag.example.com",
            "--host flag should win over BOOGY_HOST_URL env var"
        );

        unsafe {
            std::env::remove_var("BOOGY_HOST_URL");
        }
    }
}
