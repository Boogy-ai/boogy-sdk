mod build;
mod check;
mod config;
mod deploy;
mod domain;
mod frontend;
mod login;
mod manage;
mod provision;
mod secret;
mod skills;
mod smoke;

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "boogy", about = "Boogy service management CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Host URL (falls back to BOOGY_HOST_URL, then the hosted platform).
    /// For local development against your own host, set BOOGY_HOST_URL or pass
    /// --host http://localhost:3000.
    #[arg(long, env = "BOOGY_HOST_URL", default_value = "https://boogy.ai", global = true)]
    host: String,

    /// Bearer token for authenticated commands (falls back to BOOGY_TOKEN)
    #[arg(long, env = "BOOGY_TOKEN", global = true)]
    token: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Sign in via your browser (OAuth device flow)
    Login,
    /// Build a service to wasm32-wasip2
    Build {
        /// Path to the service project directory
        path: String,
    },
    /// Deploy a service to the host (publish + provision sugar)
    Deploy {
        /// Path to the deployment manifest (.boogy.toml)
        manifest: String,
        /// Dev loop: first delete this module version if it already exists and
        /// is unreferenced, then publish — so you can re-publish the same
        /// version without a bump. Fails if a live service still references it.
        #[arg(long)]
        replace: bool,
        /// After deploy, load the live deployed URL in a headless browser and
        /// assert it renders with a clean console (best-effort: no browser ⇒
        /// a warning, never a failure). Frontend deployments only.
        #[arg(long)]
        smoke: bool,
        /// CSS selector expected to render non-empty content (smoke).
        #[arg(long, default_value = "#app")]
        smoke_selector: String,
        /// Render-wait budget in milliseconds (smoke).
        #[arg(long, default_value_t = 10_000)]
        smoke_timeout: u64,
    },
    /// Publish a module (immutable, versioned wasm+manifest artifact)
    Publish {
        /// Path to the manifest (.boogy.toml)
        manifest: String,
        /// Also provision the publisher's own service from it
        #[arg(long)]
        provision: bool,
        /// Dev loop: delete this unreferenced module version first, then publish
        /// (re-publish the same version without a bump). See `deploy --replace`.
        #[arg(long)]
        replace: bool,
        /// Post-deploy headless-browser smoke (requires `--provision`). See
        /// `deploy --smoke`.
        #[arg(long)]
        smoke: bool,
        /// CSS selector expected to render non-empty content (smoke).
        #[arg(long, default_value = "#app")]
        smoke_selector: String,
        /// Render-wait budget in milliseconds (smoke).
        #[arg(long, default_value_t = 10_000)]
        smoke_timeout: u64,
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
    /// Remove a deployed service you own (no admin scope required)
    Remove {
        /// Service ID to remove
        service_id: String,
        /// Admin only: remove another owner's service (requires admin scope).
        /// Omit to remove your own service via the owner-scoped endpoint.
        #[arg(long)]
        owner: Option<String>,
    },
    /// Manage per-service secrets (sealed client-side before binding)
    Secret {
        #[command(subcommand)]
        action: SecretAction,
    },
    /// Vendor the Boogy skills into this project (flat into .claude/skills/)
    /// so coding agents pick them up automatically
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Lint a service crate for Boogy conventions (transactions, typed DTOs,
    /// Model schemas, annotated routes) before you deploy
    Check {
        /// Path to scan (default: current directory)
        path: Option<String>,
    },
    /// Manage custom domains for your services
    Domain(domain::DomainArgs),
}

#[derive(Subcommand, Debug)]
enum SecretAction {
    /// Bind a secret value to a service (sealed before it leaves this machine)
    Set {
        /// Service id to bind the secret to
        service: String,
        /// Secret name
        name: String,
        /// Secret value (omit and pass --value-stdin to read it from stdin)
        value: Option<String>,
        /// Read the value from stdin instead of the positional arg
        /// (keeps secrets out of shell history)
        #[arg(long)]
        value_stdin: bool,
    },
    /// Remove a secret binding from a service
    Rm {
        /// Service id the secret is bound to
        service: String,
        /// Secret name
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum SkillsAction {
    /// Install the skills into the current project
    Install {
        /// Destination directory (default: .claude/skills, flat — one folder per skill)
        #[arg(long)]
        dest: Option<String>,
        /// Also write a pointer for this agent so it discovers the skills
        /// (claude needs none; codex→AGENTS.md, gemini→GEMINI.md, all→both,
        /// auto→detect)
        #[arg(long = "for", value_enum, default_value = "claude")]
        agent: skills::AgentTarget,
    },
    /// Refresh a previously installed copy
    Update {
        /// Destination directory (default: .claude/skills, flat — one folder per skill)
        #[arg(long)]
        dest: Option<String>,
        /// Also refresh the pointer for this agent (see `install --for`)
        #[arg(long = "for", value_enum, default_value = "claude")]
        agent: skills::AgentTarget,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Login => login::run(&cli.host).await?,
        Commands::Build { path } => build::run(&path).await?,
        Commands::Deploy {
            manifest,
            replace,
            smoke,
            smoke_selector,
            smoke_timeout,
        } => {
            let token = resolve_token(&cli.token)?;
            let smoke_opts = smoke::SmokeOptions {
                enabled: smoke,
                selector: smoke_selector,
                timeout_ms: smoke_timeout,
            };
            deploy::run(&cli.host, &token, &manifest, replace, smoke_opts).await?
        }
        Commands::Publish {
            manifest,
            provision,
            replace,
            smoke,
            smoke_selector,
            smoke_timeout,
        } => {
            let token = resolve_token(&cli.token)?;
            let smoke_opts = smoke::SmokeOptions {
                enabled: smoke,
                selector: smoke_selector,
                timeout_ms: smoke_timeout,
            };
            provision::publish(&cli.host, &token, &manifest, provision, replace, smoke_opts).await?
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
            match owner {
                // Admin: remove another owner's service via the admin endpoint.
                Some(owner) => manage::remove_admin(&cli.host, &token, &owner, &service_id).await?,
                // Default: remove your own service via the owner-scoped endpoint
                // (ownership derived from the token — no admin scope needed).
                None => manage::remove(&cli.host, &token, &service_id).await?,
            }
        }
        Commands::Secret { action } => match action {
            SecretAction::Set {
                service,
                name,
                value,
                value_stdin,
            } => {
                let token = resolve_token(&cli.token)?;
                let value = if value_stdin {
                    secret::read_value_from_stdin()?
                } else {
                    value.context("provide a value argument or pass --value-stdin")?
                };
                secret::set(&cli.host, &token, &service, &name, &value).await?
            }
            SecretAction::Rm { service, name } => {
                let token = resolve_token(&cli.token)?;
                secret::rm(&cli.host, &token, &service, &name).await?
            }
        },
        Commands::Skills { action } => match action {
            SkillsAction::Install { dest, agent } => {
                skills::run(dest.as_deref(), "installed", agent)?
            }
            SkillsAction::Update { dest, agent } => {
                skills::run(dest.as_deref(), "updated", agent)?
            }
        },
        Commands::Check { path } => check::run(path.as_deref())?,
        Commands::Domain(args) => {
            let token = resolve_token(&cli.token)?;
            domain::run(&cli.host, &token, args).await?
        }
    }

    Ok(())
}

/// Resolve the bearer token from `--token` / `BOOGY_TOKEN` / credentials file,
/// erroring clearly if none is set (not logged in).
fn resolve_token(token: &Option<String>) -> anyhow::Result<String> {
    config::resolve_token(token.as_deref())
        .context("not logged in: set --token, BOOGY_TOKEN, or run `boogy login`")
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

        // --- with no env + no flag, the default is the hosted platform, NOT
        //     localhost (a localhost default is wrong for the common case). ---
        let cli = Cli::try_parse_from(["boogy", "list"]).expect("parse");
        assert_eq!(
            cli.host, "https://boogy.ai",
            "default --host must be the hosted platform, not localhost"
        );
    }

    /// `deploy --smoke` flags: defaults present, overrides parsed.
    #[test]
    fn deploy_smoke_flags_parse() {
        // Default: smoke off, selector `#app`, timeout 10000.
        let cli = Cli::try_parse_from(["boogy", "deploy", "app.boogy.toml"]).expect("parse");
        match cli.command {
            Commands::Deploy { smoke, smoke_selector, smoke_timeout, .. } => {
                assert!(!smoke, "smoke must default off (explicit opt-in)");
                assert_eq!(smoke_selector, "#app");
                assert_eq!(smoke_timeout, 10_000);
            }
            other => panic!("expected Deploy, got {other:?}"),
        }

        // Overrides.
        let cli = Cli::try_parse_from([
            "boogy", "deploy", "app.boogy.toml",
            "--smoke", "--smoke-selector", "#root", "--smoke-timeout", "20000",
        ])
        .expect("parse");
        match cli.command {
            Commands::Deploy { smoke, smoke_selector, smoke_timeout, .. } => {
                assert!(smoke);
                assert_eq!(smoke_selector, "#root");
                assert_eq!(smoke_timeout, 20_000);
            }
            other => panic!("expected Deploy, got {other:?}"),
        }
    }

    /// `publish --smoke` also carries the flags.
    #[test]
    fn publish_smoke_flags_parse() {
        let cli = Cli::try_parse_from([
            "boogy", "publish", "app.boogy.toml", "--provision", "--smoke",
        ])
        .expect("parse");
        match cli.command {
            Commands::Publish { provision, smoke, smoke_selector, .. } => {
                assert!(provision);
                assert!(smoke);
                assert_eq!(smoke_selector, "#app");
            }
            other => panic!("expected Publish, got {other:?}"),
        }
    }

    /// `--version` / `-V` is wired to the crate version.
    #[test]
    fn version_flag_is_set() {
        use clap::CommandFactory;
        let v = Cli::command().get_version().map(|s| s.to_string());
        assert_eq!(
            v.as_deref(),
            Some(env!("CARGO_PKG_VERSION")),
            "boogy --version should report the crate version"
        );
    }
}
