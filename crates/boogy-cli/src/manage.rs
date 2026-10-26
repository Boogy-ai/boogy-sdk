use anyhow::{Context, Result};

/// List deployed services.
///
/// Without `all`: the caller's OWN services via the owner-scoped
/// `GET /v1/services` (any signed-in user; ownership derived from the token),
/// following its pages to the end.
/// With `all`: every owner's services via `GET /_admin/services`, which
/// needs admin scope. Until 2026-09-12 `boogy list` only ever hit the admin
/// endpoint, so the quickstart's "verify with `boogy list`" step answered a
/// normal user with `403 Forbidden`.
pub async fn list(host: &str, token: &str, all: bool) -> Result<()> {
    let client = crate::http::client();
    let (base, what) = if all {
        (format!("{host}/_admin/services"), "admin")
    } else {
        (format!("{host}/v1/services"), "owner")
    };

    // The owner listing is paged (`{items, next_cursor}`); a person at a
    // terminal wants all of it, so follow the cursor and print one list.
    // `--all` is one unpaged document and is printed as the host sent it.
    let mut cursor: Option<String> = None;
    let mut items: Vec<serde_json::Value> = Vec::new();
    loop {
        let url = page_url(&base, cursor.as_deref());
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .context("failed to reach host")?;

        match resp.status() {
            reqwest::StatusCode::UNAUTHORIZED => anyhow::bail!(
                "list failed (401 Unauthorized): token invalid or expired — \
                 set --token or BOOGY_TOKEN to a valid token, or run `boogy login`"
            ),
            reqwest::StatusCode::FORBIDDEN if all => anyhow::bail!(
                "list --all failed (403 Forbidden): listing every owner's services needs \
                 admin scope. Drop --all to list your own services."
            ),
            s if !s.is_success() => anyhow::bail!("list failed ({s}, {what} listing)"),
            _ => {}
        }

        let body: serde_json::Value = resp.json().await?;
        if all {
            println!("{}", serde_json::to_string_pretty(&body)?);
            return Ok(());
        }
        cursor = take_page(&body, &mut items)?;
        if cursor.is_none() {
            break;
        }
    }
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "items": items }))?);
    Ok(())
}

/// `base` with `cursor` (an opaque URL-safe token) as the paging parameter.
fn page_url(base: &str, cursor: Option<&str>) -> String {
    match cursor {
        Some(c) => format!("{base}?cursor={c}"),
        None => base.to_string(),
    }
}

/// Append one page's `items` to `out` and return its `next_cursor`.
///
/// A host that does not page this listing yet answers `{services}`: that body
/// is the whole list, so it is read as one page with no cursor.
fn take_page(body: &serde_json::Value, out: &mut Vec<serde_json::Value>) -> Result<Option<String>> {
    if let Some(page) = body.get("items").and_then(|v| v.as_array()) {
        out.extend(page.iter().cloned());
        return Ok(body.get("next_cursor").and_then(|v| v.as_str()).map(str::to_string));
    }
    let all = body
        .get("services")
        .and_then(|v| v.as_array())
        .context("list response has neither an `items` nor a `services` array")?;
    out.extend(all.iter().cloned());
    Ok(None)
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
    let client = crate::http::client();
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
        // The route accepts (202) and begins a background delete — it does
        // not remove the service synchronously. See
        // docs/superpowers/specs/2026-09-22-draining-service-delete-design.md.
        println!("Service is being deleted: {service_id}");
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
    let client = crate::http::client();
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
        // Same two-phase delete as the owner-scoped route above: accepted
        // (202), not removed synchronously.
        println!("Service is being deleted: {owner}/{service_id}");
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

    #[test]
    fn page_url_appends_the_cursor_only_when_there_is_one() {
        assert_eq!(page_url("http://h/v1/services", None), "http://h/v1/services");
        assert_eq!(page_url("http://h/v1/services", Some("abc_-9")), "http://h/v1/services?cursor=abc_-9");
    }

    #[test]
    fn take_page_collects_items_and_returns_the_next_cursor() {
        let mut out = Vec::new();
        let next = take_page(&serde_json::json!({"items": [{"service_id": "a"}], "next_cursor": "c1"}), &mut out).unwrap();
        assert_eq!(next.as_deref(), Some("c1"));
        let next = take_page(&serde_json::json!({"items": [{"service_id": "b"}], "next_cursor": null}), &mut out).unwrap();
        assert_eq!(next, None);
        assert_eq!(out.len(), 2);
        assert!(take_page(&serde_json::json!({"neither": []}), &mut out).is_err());
    }

    #[test]
    fn take_page_reads_an_unpaged_services_body_as_one_page() {
        // A host from before paging answers `{services}`: one page, no cursor.
        let mut out = Vec::new();
        let body = serde_json::json!({"services": [{"service_id": "a"}, {"service_id": "b"}]});
        assert_eq!(take_page(&body, &mut out).unwrap(), None);
        assert_eq!(out, vec![serde_json::json!({"service_id": "a"}), serde_json::json!({"service_id": "b"})]);
        // `items` wins when both are present.
        let mut out = Vec::new();
        let both = serde_json::json!({"items": [{"service_id": "c"}], "services": [{"service_id": "x"}], "next_cursor": "n"});
        assert_eq!(take_page(&both, &mut out).unwrap().as_deref(), Some("n"));
        assert_eq!(out, vec![serde_json::json!({"service_id": "c"})]);
    }
}
