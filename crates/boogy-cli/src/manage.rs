use anyhow::{Context, Result};

pub async fn list(host: &str, token: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{host}/_admin/services"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("failed to reach host")?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(
            "list failed (401 Unauthorized): token invalid or missing admin scope — \
             set --token or BOOGY_TOKEN to a valid admin token"
        );
    }

    if !resp.status().is_success() {
        anyhow::bail!("list failed ({})", resp.status());
    }

    let body: serde_json::Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

/// Build the owner-scoped `/v1/services/{service_id}` URL for self-remove.
/// Factored out for unit testing.
pub fn remove_url(host: &str, service_id: &str) -> String {
    format!("{host}/v1/services/{service_id}")
}

/// Build the `/_admin/services/{owner}/{service_id}` URL for admin remove.
/// Factored out for unit testing.
pub fn admin_remove_url(host: &str, owner: &str, service_id: &str) -> String {
    format!("{host}/_admin/services/{owner}/{service_id}")
}

/// Remove a service the caller OWNS, via the owner-scoped `/v1` endpoint.
/// Ownership is derived from the token — no admin scope required.
pub async fn remove(host: &str, token: &str, service_id: &str) -> Result<()> {
    let url = remove_url(host, service_id);
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("failed to reach host")?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(
            "remove failed (401 Unauthorized): token invalid or expired — \
             set --token or BOOGY_TOKEN to a valid token"
        );
    }

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "remove failed (404 Not Found): no service '{service_id}' owned by you. \
             To remove another owner's service (admin), pass --owner <owner>."
        );
    }

    if resp.status().is_success() {
        println!("Removed service: {service_id}");
    } else {
        anyhow::bail!("failed to remove service: {}", resp.status());
    }

    Ok(())
}

/// Remove ANY owner's service, via the `/_admin` endpoint (requires admin scope).
pub async fn remove_admin(host: &str, token: &str, owner: &str, service_id: &str) -> Result<()> {
    let url = admin_remove_url(host, owner, service_id);
    // A1: destructive DELETEs must echo the exact resource path in
    // `X-Boogy-Confirm` (the host rejects an unconfirmed delete with 428).
    let confirm_path = format!("/_admin/services/{owner}/{service_id}");
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("x-boogy-confirm", &confirm_path)
        .send()
        .await
        .context("failed to reach host")?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED || resp.status() == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!(
            "remove failed ({}): token invalid or missing admin scope — \
             removing another owner's service requires an admin token. \
             To remove your OWN service, omit --owner.",
            resp.status()
        );
    }

    if resp.status().is_success() {
        println!("Removed service: {owner}/{service_id}");
    } else {
        anyhow::bail!("failed to remove service: {}", resp.status());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_url_owner_scoped_v1_path() {
        // Self-remove uses the owner-scoped /v1 endpoint (no owner segment;
        // ownership derived from the token) — NOT the admin route.
        let url = remove_url("http://localhost:3000", "my-svc");
        assert_eq!(url, "http://localhost:3000/v1/services/my-svc");
    }

    #[test]
    fn admin_remove_url_two_segment_path() {
        let url = admin_remove_url("http://localhost:3000", "alice", "my-svc");
        assert_eq!(url, "http://localhost:3000/_admin/services/alice/my-svc");
    }
}
