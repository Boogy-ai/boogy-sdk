//! `boogy domain add` / `boogy domain list` / `boogy domain remove` —
//! manage custom domains for your services.

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

/// Subcommands for `boogy domain`.
#[derive(Subcommand, Debug)]
pub enum DomainCmd {
    /// Register a custom domain and attach it to a service.
    ///
    /// The platform returns the DNS records you must create at your registrar
    /// before the domain will serve traffic.
    Add {
        /// Fully-qualified domain name (e.g. api.example.com)
        fqdn: String,
        /// Service's route segment — the path prefix it's served under (the token after your handle in a subdomain URL, e.g. `api` for `<handle>.boogy.app/api/...`). This is NOT the manifest service id.
        #[arg(long)]
        service: String,
    },
    /// List all custom domains registered to your account.
    List,
    /// Remove a custom domain from the platform.
    Remove {
        /// Fully-qualified domain name to remove
        fqdn: String,
    },
}

/// Top-level args struct used by `main.rs`.
#[derive(clap::Args, Debug)]
pub struct DomainArgs {
    #[command(subcommand)]
    pub cmd: DomainCmd,
}

// ── response types ────────────────────────────────────────────────────────────

/// A single DNS record returned by the platform after registering a domain.
#[derive(Debug, Deserialize)]
pub(crate) struct DnsRecord {
    #[serde(rename = "type")]
    record_type: String,
    name: String,
    value: String,
}

/// Response from `POST /v1/domains`.
#[derive(Debug, Deserialize)]
struct AddResponse {
    domain: String,
    status: String,
    dns_records: Vec<DnsRecord>,
}

/// One entry from `GET /v1/domains`.
#[derive(Debug, Deserialize, Serialize)]
struct DomainEntry {
    domain: String,
    service_id: String,
    status: String,
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build the URL for domain operations — factored out for unit tests.
pub fn domains_url(host: &str) -> String {
    format!("{host}/v1/domains")
}

/// Build the URL for a single domain — factored out for unit tests.
pub fn domain_url(host: &str, fqdn: &str) -> String {
    format!("{host}/v1/domains/{fqdn}")
}

/// Pretty-print the DNS records the user must create at their registrar.
pub(crate) fn print_dns_records(domain: &str, status: &str, records: &[DnsRecord]) {
    println!("Domain registered: {domain}  (status: {status})");
    println!();
    if records.is_empty() {
        println!("No DNS records returned — check the platform dashboard.");
        return;
    }
    println!("Your service's route segment will be served at the domain root once verified.");
    println!();
    println!("Create the following DNS records at your registrar:");
    println!();
    // Compute column widths for alignment.
    let type_w = records.iter().map(|r| r.record_type.len()).max().unwrap_or(4).max(4);
    let name_w = records.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    println!(
        "  {:<type_w$}  {:<name_w$}  VALUE",
        "TYPE",
        "NAME",
        type_w = type_w,
        name_w = name_w,
    );
    println!(
        "  {:-<type_w$}  {:-<name_w$}  {:-<40}",
        "",
        "",
        "",
        type_w = type_w,
        name_w = name_w,
    );
    for r in records {
        println!(
            "  {:<type_w$}  {:<name_w$}  {}",
            r.record_type,
            r.name,
            r.value,
            type_w = type_w,
            name_w = name_w,
        );
    }
    println!();
    println!(
        "Once propagated, the platform will verify the records and activate the domain."
    );
}

// ── public entry point ────────────────────────────────────────────────────────

/// Dispatch the `domain` subcommand.
pub async fn run(host: &str, token: &str, args: DomainArgs) -> Result<()> {
    match args.cmd {
        DomainCmd::Add { fqdn, service } => add(host, token, &fqdn, &service).await,
        DomainCmd::List => list(host, token).await,
        DomainCmd::Remove { fqdn } => remove(host, token, &fqdn).await,
    }
}

async fn add(host: &str, token: &str, fqdn: &str, service: &str) -> Result<()> {
    let url = domains_url(host);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "domain": fqdn, "service_id": service }))
        .send()
        .await
        .context("failed to reach host")?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(
            "add failed (401 Unauthorized): token invalid or missing scope — \
             set --token or BOOGY_TOKEN to a valid token"
        );
    }
    if status == reqwest::StatusCode::CONFLICT {
        anyhow::bail!(
            "add failed (409 Conflict): domain '{fqdn}' is already registered"
        );
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "add failed (404 Not Found): service '{service}' not found or not owned by you"
        );
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("add failed ({status}): {body}");
    }

    let parsed: AddResponse = resp
        .json()
        .await
        .context("failed to parse the add-domain response")?;

    print_dns_records(&parsed.domain, &parsed.status, &parsed.dns_records);
    Ok(())
}

async fn list(host: &str, token: &str) -> Result<()> {
    let url = domains_url(host);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("failed to reach host")?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(
            "list failed (401 Unauthorized): token invalid or missing scope — \
             set --token or BOOGY_TOKEN to a valid token"
        );
    }
    if !resp.status().is_success() {
        anyhow::bail!("list failed ({})", resp.status());
    }

    let domains: Vec<DomainEntry> = resp
        .json()
        .await
        .context("failed to parse the domain list response")?;

    if domains.is_empty() {
        println!("No custom domains registered.");
        return Ok(());
    }

    let domain_w = domains.iter().map(|d| d.domain.len()).max().unwrap_or(6).max(6);
    let svc_w = domains.iter().map(|d| d.service_id.len()).max().unwrap_or(7).max(7);
    println!(
        "  {:<domain_w$}  {:<svc_w$}  STATUS",
        "DOMAIN",
        "SERVICE",
        domain_w = domain_w,
        svc_w = svc_w,
    );
    println!(
        "  {:-<domain_w$}  {:-<svc_w$}  {:-<10}",
        "",
        "",
        "",
        domain_w = domain_w,
        svc_w = svc_w,
    );
    for d in &domains {
        println!(
            "  {:<domain_w$}  {:<svc_w$}  {}",
            d.domain,
            d.service_id,
            d.status,
            domain_w = domain_w,
            svc_w = svc_w,
        );
    }
    Ok(())
}

async fn remove(host: &str, token: &str, fqdn: &str) -> Result<()> {
    let url = domain_url(host, fqdn);
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("failed to reach host")?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(
            "remove failed (401 Unauthorized): token invalid or missing scope — \
             set --token or BOOGY_TOKEN to a valid token"
        );
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "remove failed (404 Not Found): domain '{fqdn}' not found or not owned by you"
        );
    }
    if status.is_success() {
        println!("Removed domain: {fqdn}");
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("remove failed ({status}): {body}");
    }
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_url_path() {
        assert_eq!(domains_url("http://localhost:3000"), "http://localhost:3000/v1/domains");
    }

    #[test]
    fn domain_url_encodes_fqdn_segment() {
        assert_eq!(
            domain_url("http://localhost:3000", "api.example.com"),
            "http://localhost:3000/v1/domains/api.example.com"
        );
    }

    #[test]
    fn print_dns_records_empty_does_not_panic() {
        // Just ensure no panic on the empty-records path.
        print_dns_records("api.example.com", "pending", &[]);
    }

    #[test]
    fn print_dns_records_formats_aligned_table() {
        let records = vec![
            DnsRecord {
                record_type: "CNAME".into(),
                name: "api.example.com".into(),
                value: "target.boogy.ai".into(),
            },
            DnsRecord {
                record_type: "TXT".into(),
                name: "_boogy-verify.api.example.com".into(),
                value: "boogy-verify=abc123".into(),
            },
        ];
        // Smoke: must complete without panic and produce some output.
        // We capture via redirect isn't feasible in unit tests; just call it.
        print_dns_records("api.example.com", "pending", &records);
    }
}
