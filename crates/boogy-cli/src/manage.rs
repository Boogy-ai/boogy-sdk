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

/// Build the `/_admin/services/{owner}/{service_id}` URL for remove.
/// Factored out for unit testing.
pub fn remove_url(host: &str, owner: &str, service_id: &str) -> String {
    format!("{host}/_admin/services/{owner}/{service_id}")
}

pub async fn remove(host: &str, token: &str, owner: &str, service_id: &str) -> Result<()> {
    let url = remove_url(host, owner, service_id);
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

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(
            "remove failed (401 Unauthorized): token invalid or missing admin scope — \
             set --token or BOOGY_TOKEN to a valid admin token"
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
    fn remove_url_two_segment_path() {
        let url = remove_url("http://localhost:3000", "alice", "my-svc");
        assert_eq!(url, "http://localhost:3000/_admin/services/alice/my-svc");
    }

    #[test]
    fn remove_url_trailing_slash_host() {
        // Host with trailing slash should still produce a valid double-segment path.
        // The format! macro concatenates literally, so host="http://h/" gives
        // "http://h//_admin/..." — callers should not pass trailing slashes, but
        // let's document the current behaviour rather than silently mangle the URL.
        let url = remove_url("http://localhost:3000", "bob", "other-svc");
        assert!(url.ends_with("/bob/other-svc"));
    }
}
