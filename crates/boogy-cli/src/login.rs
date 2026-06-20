use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::config::{self, Credentials};

// ── RFC 8628 authorize response ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AuthorizeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

// ── RFC 8628 token response ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TokenSuccess {
    token: String,
    expires_at: u64,
    agent: AgentInfo,
}

#[derive(Debug, Deserialize)]
struct AgentInfo {
    handle: String,
}

#[derive(Debug, Deserialize)]
struct TokenError {
    error: String,
}

// ── Token request body ────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct TokenRequest<'a> {
    device_code: &'a str,
}

// ── Pure helper (testable) ────────────────────────────────────────────────────

/// Returns the next polling interval, bumped by 5 s on `slow_down` (RFC 8628 §3.5).
pub fn next_interval(current: u64, slow_down: bool) -> u64 {
    if slow_down { current + 5 } else { current }
}

// ── Browser open (best-effort, platform shell-out) ────────────────────────────

fn try_open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

// ── Main entry point ──────────────────────────────────────────────────────────

pub async fn run(host: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();

    // Step 1: obtain device + user code.
    let auth_resp: AuthorizeResponse = client
        .post(format!("{host}/_agents/oauth/device/authorize"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .context("failed to reach the authorization endpoint")?
        .error_for_status()
        .context("authorization endpoint returned an error")?
        .json()
        .await
        .context("unexpected response from authorization endpoint")?;

    // Step 2: print instructions.
    eprintln!();
    eprintln!("To sign in, open:  {}", auth_resp.verification_uri_complete);
    eprintln!("and confirm this code:  {}", auth_resp.user_code);
    eprintln!("(fallback URL: {})", auth_resp.verification_uri);
    eprintln!("Waiting for approval…");

    // Step 3: best-effort browser open.
    try_open_browser(&auth_resp.verification_uri_complete);

    // Step 4: poll loop.
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(auth_resp.expires_in);
    let mut interval = auth_resp.interval;

    loop {
        // Poll first — RFC 8628 permits an immediate first poll; first response
        // may already be approved if the user acted quickly.
        let resp = client
            .post(format!("{host}/_agents/oauth/device/token"))
            .json(&TokenRequest { device_code: &auth_resp.device_code })
            .send()
            .await
            .context("failed to reach the token endpoint")?;

        match resp.status().as_u16() {
            200 => {
                let ok: TokenSuccess = resp
                    .json()
                    .await
                    .context("unexpected token response body")?;
                config::save(&Credentials {
                    token: ok.token,
                    handle: ok.agent.handle.clone(),
                    expires: ok.expires_at,
                })?;
                println!("✓ Logged in as {}", ok.agent.handle);
                return Ok(());
            }
            400 => {
                let err: TokenError = resp
                    .json()
                    .await
                    .context("unexpected error response body")?;
                match err.error.as_str() {
                    "authorization_pending" => {
                        // fall through to sleep + deadline check
                    }
                    "slow_down" => {
                        interval = next_interval(interval, true);
                    }
                    "expired_token" => {
                        anyhow::bail!("the login code expired; run `boogy login` again");
                    }
                    "access_denied" => {
                        anyhow::bail!("login was denied");
                    }
                    other => {
                        anyhow::bail!("unexpected error from token endpoint: {other}");
                    }
                }
            }
            status => {
                anyhow::bail!("token endpoint returned unexpected status {status}");
            }
        }

        // Sleep then deadline-check before the next poll.  Checking after the
        // sleep (not before) means we always attempt at least one poll even
        // when expires_in <= interval.
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for approval; run `boogy login` again");
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_down_bumps_interval() {
        assert_eq!(next_interval(5, false), 5);
        assert_eq!(next_interval(5, true), 10);
    }
}
