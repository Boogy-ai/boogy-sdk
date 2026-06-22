//! Post-deploy headless-browser smoke test (`boogy deploy --smoke`).
//!
//! Opt-in, best-effort, client-side: after a successful provision, drive a
//! detected (externally-installed) Chrome/Chromium over CDP against the live
//! deployed URL and assert the page renders with a clean console. No browser
//! found ⇒ a clear warning and a clean exit — never blocks a deploy.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::log::{EventEntryAdded, LogEntryLevel, LogEntrySource};
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams as NetworkEnableParams, EventResponseReceived,
};
use chromiumoxide::cdp::js_protocol::runtime::EventExceptionThrown;
use futures::StreamExt;

/// Env var pointing at a specific browser binary, checked before any search.
const BROWSER_ENV: &str = "BOOGY_SMOKE_BROWSER";

/// Resolve a browser binary from an explicit search list — the pure core of
/// detection (no env, no real `PATH`), so it is exhaustively testable.
///
/// Returns the first `dir/name` that exists as a file, scanning `dirs` in order
/// and `names` in order within each dir.
pub fn resolve_browser_in(names: &[&str], dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Derive the live deployed URL from the provision response's `service` URI.
///
/// `("https://boogy.ai", "boogy://alice/services/todos")` →
/// `Some("https://boogy.ai/alice/todos/")`. Returns `None` for any URI that is
/// not a 3-component `boogy://<owner>/services/<id>` (e.g. a `modules` URI).
pub fn deployed_url(host: &str, service_uri: &str) -> Option<String> {
    let rest = service_uri.strip_prefix("boogy://")?;
    // Exactly three components: <owner>/<kind>/<id>, with kind == "services".
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let kind = parts.next()?;
    let id = parts.next().filter(|s| !s.is_empty())?;
    if kind != "services" || id.contains('/') {
        return None;
    }
    let host = host.trim_end_matches('/');
    Some(format!("{host}/{owner}/{id}/"))
}

/// Known browser executable names, in preference order.
const BROWSER_NAMES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "chrome",
];

/// Locate a headless-capable browser: the `BOOGY_SMOKE_BROWSER` override first
/// (trusted verbatim, not existence-checked), then known browser names across
/// the `PATH` directories and common install locations.
pub fn detect_browser() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(BROWSER_ENV) {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    // Common install locations not always on a non-interactive `PATH`.
    for extra in [
        "/usr/bin",
        "/usr/local/bin",
        "/snap/bin",
        "/Applications/Google Chrome.app/Contents/MacOS",
        "/Applications/Chromium.app/Contents/MacOS",
    ] {
        dirs.push(PathBuf::from(extra));
    }
    // macOS app bundles use spaced display names as the executable.
    let mac_names: &[&str] = &["Google Chrome", "Chromium"];
    resolve_browser_in(BROWSER_NAMES, &dirs).or_else(|| resolve_browser_in(mac_names, &dirs))
}

/// Outcome of a single smoke run against a deployed URL.
#[derive(Debug, Default, Clone)]
pub struct SmokeReport {
    /// Overall pass: page rendered AND no console errors AND no failed
    /// same-origin sub-resource loads.
    pub ok: bool,
    /// High-level assertion failures (e.g. "page did not render within 10s").
    pub failures: Vec<String>,
    /// `console.error` calls + uncaught JS exceptions captured during the run.
    pub console_errors: Vec<String>,
    /// Same-origin sub-resource responses with an HTTP status ≥ 400.
    pub failed_requests: Vec<String>,
}

impl SmokeReport {
    /// A human-readable multi-line summary for the terminal.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.ok {
            out.push_str("smoke: PASS — page rendered, console clean, no failed requests");
            return out;
        }
        out.push_str("smoke: FAIL");
        for f in &self.failures {
            out.push_str(&format!("\n  - {f}"));
        }
        for e in &self.console_errors {
            out.push_str(&format!("\n  - console error: {e}"));
        }
        for r in &self.failed_requests {
            out.push_str(&format!("\n  - failed request: {r}"));
        }
        out
    }
}

/// Options for the post-deploy smoke, threaded from the CLI flags.
#[derive(Debug, Clone)]
pub struct SmokeOptions {
    /// `--smoke`: run the smoke at all.
    pub enabled: bool,
    /// `--smoke-selector`: the element expected to render non-empty.
    pub selector: String,
    /// `--smoke-timeout`: render-wait budget in milliseconds.
    pub timeout_ms: u64,
}

impl Default for SmokeOptions {
    fn default() -> Self {
        Self { enabled: false, selector: "#app".to_string(), timeout_ms: 10_000 }
    }
}

/// Derive the provisioned service URI from a published module URI.
/// `boogy://alice/modules/todos@1.2.0` → `Some("boogy://alice/services/todos")`.
/// (`deploy` / `publish --provision` provision the publisher's own service:
/// owner == publisher, service id == module name.)
pub fn service_uri_from_module(module_uri: &str) -> Option<String> {
    let rest = module_uri.strip_prefix("boogy://")?;
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let kind = parts.next()?;
    let name_at_version = parts.next()?;
    if kind != "modules" {
        return None;
    }
    let name = name_at_version.split('@').next().filter(|s| !s.is_empty())?;
    Some(format!("boogy://{owner}/services/{name}"))
}

/// Run the opt-in post-deploy smoke. Best-effort: a missing browser, a
/// non-frontend deployment, or an underivable URL is a clear note and a clean
/// return — only a browser that ran and failed its assertions is an error.
pub async fn run_post_deploy_smoke(
    opts: &SmokeOptions,
    host: &str,
    service_uri: Option<&str>,
    has_frontend: bool,
) -> anyhow::Result<()> {
    if !opts.enabled {
        return Ok(());
    }
    if !has_frontend {
        println!("  Smoke: skipped (no [frontend] to render)");
        return Ok(());
    }
    let url = match service_uri.and_then(|u| deployed_url(host, u)) {
        Some(u) => u,
        None => {
            println!("  Smoke: skipped (could not derive a deployed URL)");
            return Ok(());
        }
    };
    let browser = match detect_browser() {
        Some(b) => b,
        None => {
            println!(
                "  Smoke: skipped — no headless browser found \
                 (set BOOGY_SMOKE_BROWSER or install Chrome/Chromium)"
            );
            return Ok(());
        }
    };
    println!("  Smoke: loading {url} via {} ...", browser.display());
    let report = run_smoke(
        &browser,
        &url,
        &opts.selector,
        Duration::from_millis(opts.timeout_ms),
    )
    .await?;
    println!("{}", report.render());
    if !report.ok {
        anyhow::bail!("smoke test failed for {url}");
    }
    Ok(())
}

/// Extract the `scheme://host[:port]` origin of a URL (string-only; no dep).
/// `"https://boogy.ai/alice/todos/"` → `Some("https://boogy.ai")`.
fn origin_of(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let authority_start = scheme_end + 3;
    let after = &url[authority_start..];
    let host_len = after.find('/').unwrap_or(after.len());
    if host_len == 0 {
        return None;
    }
    Some(url[..authority_start + host_len].to_string())
}

/// JS expression that returns `true` when the target selector (or `body`) has
/// rendered non-empty content. The selector is JSON-encoded to neutralize
/// quotes.
fn render_probe_js(selector: &str) -> String {
    let sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"#app\"".to_string());
    format!(
        r#"(() => {{
  const el = document.querySelector({sel}) || document.body;
  if (!el) return false;
  const text = ((el.innerText || el.textContent) || "").trim();
  const kids = el.children ? el.children.length : 0;
  return text.length > 0 || kids > 0;
}})()"#
    )
}

/// Drive a detected headless Chrome to `url` over CDP and assert it renders
/// with a clean console and no failed same-origin sub-resource loads.
///
/// Launches the browser at `browser` (headless, no-sandbox), subscribes to the
/// CDP console/exception/network event streams, navigates, waits up to
/// `timeout` for `selector` (or `body`) to have non-empty content, then collects
/// the captured signals. The browser process is closed on exit.
pub async fn run_smoke(
    browser: &Path,
    url: &str,
    selector: &str,
    timeout: Duration,
) -> anyhow::Result<SmokeReport> {
    let config = BrowserConfig::builder()
        .chrome_executable(browser)
        .new_headless_mode()
        .no_sandbox()
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .build()
        .map_err(|e| anyhow::anyhow!("browser config: {e}"))?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .context("launching headless browser")?;

    // The handler future must be polled continuously for CDP to make progress.
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    // Run the actual smoke in a closure so we always tear the browser down.
    let result = async {
        let page = browser
            .new_page("about:blank")
            .await
            .context("opening page")?;
        page.enable_log().await.context("enabling log domain")?;
        page.enable_runtime()
            .await
            .context("enabling runtime domain")?;
        page.execute(NetworkEnableParams::default())
            .await
            .context("enabling network domain")?;

        let console_errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let failed_requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let origin = origin_of(url);

        // Console errors + uncaught exceptions arrive as Log entries; we keep
        // non-network error entries here and route network failures below.
        let mut log_stream = page.event_listener::<EventEntryAdded>().await?;
        let ce = console_errors.clone();
        let log_task = tokio::spawn(async move {
            while let Some(ev) = log_stream.next().await {
                if matches!(ev.entry.level, LogEntryLevel::Error)
                    && !matches!(ev.entry.source, LogEntrySource::Network)
                {
                    ce.lock().unwrap().push(ev.entry.text.clone());
                }
            }
        });

        let mut exc_stream = page.event_listener::<EventExceptionThrown>().await?;
        let ce2 = console_errors.clone();
        let exc_task = tokio::spawn(async move {
            while let Some(ev) = exc_stream.next().await {
                let msg = ev
                    .exception_details
                    .exception
                    .as_ref()
                    .and_then(|o| o.description.clone())
                    .unwrap_or_else(|| ev.exception_details.text.clone());
                ce2.lock().unwrap().push(msg);
            }
        });

        let mut net_stream = page.event_listener::<EventResponseReceived>().await?;
        let fr = failed_requests.clone();
        let origin_for_net = origin.clone();
        let net_task = tokio::spawn(async move {
            while let Some(ev) = net_stream.next().await {
                let status = ev.response.status;
                let resp_url = &ev.response.url;
                let same_origin = match &origin_for_net {
                    Some(o) => resp_url.starts_with(o.as_str()),
                    None => true,
                };
                // The browser auto-requests /favicon.ico; a 404 there is not a
                // page-declared sub-resource failure and must not fail a smoke.
                let is_favicon = resp_url
                    .split(['?', '#'])
                    .next()
                    .unwrap_or(resp_url)
                    .ends_with("/favicon.ico");
                if status >= 400 && same_origin && !is_favicon {
                    fr.lock().unwrap().push(format!("{status} {resp_url}"));
                }
            }
        });

        // Navigate and wait (bounded) for content to render.
        page.goto(url).await.context("navigating to deployed URL")?;
        let probe = render_probe_js(selector);
        let deadline = Instant::now() + timeout;
        let mut rendered = false;
        while Instant::now() < deadline {
            if let Ok(eval) = page.evaluate(probe.clone()).await {
                if eval.into_value::<bool>().unwrap_or(false) {
                    rendered = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        // Brief settle so late console errors / failed requests are captured.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Stop the collectors and snapshot what they gathered.
        log_task.abort();
        exc_task.abort();
        net_task.abort();
        let console_errors = std::mem::take(&mut *console_errors.lock().unwrap());
        let failed_requests = std::mem::take(&mut *failed_requests.lock().unwrap());

        let mut failures = Vec::new();
        if !rendered {
            failures.push(format!(
                "page did not render non-empty content (selector `{selector}`) within {}ms",
                timeout.as_millis()
            ));
        }
        let ok = rendered && console_errors.is_empty() && failed_requests.is_empty();
        Ok::<SmokeReport, anyhow::Error>(SmokeReport {
            ok,
            failures,
            console_errors,
            failed_requests,
        })
    }
    .await;

    let _ = browser.close().await;
    let _ = browser.wait().await;
    handler_task.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_browser_prefers_env_override() {
        // Safety: single-threaded test access to the env var.
        unsafe { std::env::set_var(BROWSER_ENV, "/custom/chrome"); }
        assert_eq!(detect_browser(), Some(PathBuf::from("/custom/chrome")));
        unsafe { std::env::remove_var(BROWSER_ENV); }
    }

    #[test]
    fn detect_browser_none_when_absent() {
        // With the env unset and an empty search list, detection returns None.
        unsafe { std::env::remove_var(BROWSER_ENV); }
        assert!(resolve_browser_in(&[], &[]).is_none());
    }

    #[test]
    fn resolve_browser_in_finds_first_match_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Create a fake "chromium" executable; "google-chrome" is absent.
        let chromium = dir.path().join("chromium");
        std::fs::write(&chromium, b"#!/bin/sh\n").expect("write");
        let found = resolve_browser_in(
            &["google-chrome", "chromium", "chrome"],
            &[dir.path().to_path_buf()],
        );
        assert_eq!(found.as_deref(), Some(chromium.as_path()));
    }

    #[test]
    fn deployed_url_from_service_uri() {
        assert_eq!(
            deployed_url("https://boogy.ai", "boogy://alice/services/todos").as_deref(),
            Some("https://boogy.ai/alice/todos/"),
        );
        // a trailing slash on the host is tolerated.
        assert_eq!(
            deployed_url("https://boogy.ai/", "boogy://alice/services/todos").as_deref(),
            Some("https://boogy.ai/alice/todos/"),
        );
        // non-service URIs (modules) → None.
        assert!(deployed_url("https://boogy.ai", "boogy://alice/modules/todos@1.0.0").is_none());
        // missing scheme / wrong shape → None.
        assert!(deployed_url("https://boogy.ai", "alice/services/todos").is_none());
        assert!(deployed_url("https://boogy.ai", "boogy://alice/services").is_none());
    }

    #[test]
    fn service_uri_from_module_derives_services_uri() {
        assert_eq!(
            service_uri_from_module("boogy://alice/modules/todos@1.2.0").as_deref(),
            Some("boogy://alice/services/todos"),
        );
        // version is optional in the derivation.
        assert_eq!(
            service_uri_from_module("boogy://alice/modules/todos").as_deref(),
            Some("boogy://alice/services/todos"),
        );
        // a services URI is not a module URI.
        assert!(service_uri_from_module("boogy://alice/services/todos").is_none());
        assert!(service_uri_from_module("nonsense").is_none());
    }

    #[test]
    fn origin_of_strips_path() {
        assert_eq!(origin_of("https://boogy.ai/alice/todos/").as_deref(), Some("https://boogy.ai"));
        assert_eq!(origin_of("https://boogy.ai").as_deref(), Some("https://boogy.ai"));
        assert_eq!(origin_of("http://localhost:3000/a/b/").as_deref(), Some("http://localhost:3000"));
        assert!(origin_of("not-a-url").is_none());
        assert!(origin_of("https:///nohost").is_none());
    }

    #[test]
    fn render_probe_js_embeds_escaped_selector() {
        let js = render_probe_js("#app");
        assert!(js.contains("\"#app\""), "selector should be JSON-encoded: {js}");
        // a selector with a quote must not break out of the JS string literal.
        let js = render_probe_js("a[title=\"x\"]");
        assert!(js.contains(r#""a[title=\"x\"]""#), "quotes must be escaped: {js}");
    }

    /// Browser-gated end-to-end smoke (like the live-Redis tests): runs only
    /// when a real headless browser is present, otherwise skips with a note.
    /// Serves a known-good and a deliberately-broken page from a tiny local
    /// server and asserts `run_smoke` PASSES / FAILS accordingly.
    ///
    /// Run with: `cargo test -p boogy-cli -- --ignored smoke_e2e`
    /// (needs Chrome/Chromium on PATH or `BOOGY_SMOKE_BROWSER`).
    #[tokio::test]
    #[ignore = "requires a headless browser (Chrome/Chromium)"]
    async fn smoke_e2e_good_passes_broken_fails() {
        let Some(browser) = detect_browser() else {
            eprintln!("skipping smoke_e2e: no headless browser found");
            return;
        };

        // Tiny local HTTP server: `/good` renders #app; `/broken` leaves #app
        // empty, throws in an inline script, and fetches a 404 sub-resource.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/");
                    let (status, body): (&str, String) = match path {
                        "/good" => ("200 OK",
                            "<!doctype html><html><body><div id=\"app\">Hello Boogy</div></body></html>".into()),
                        "/broken" => ("200 OK",
                            "<!doctype html><html><body><div id=\"app\"></div>\
                             <script>console.error(\"kaboom\"); fetch(\"/missing-asset.js\");\
                             throw new Error(\"render boom\");</script></body></html>".into()),
                        _ => ("404 Not Found", "missing".into()),
                    };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });

        let base = format!("http://127.0.0.1:{port}");
        let timeout = Duration::from_secs(8);

        let good = run_smoke(&browser, &format!("{base}/good"), "#app", timeout)
            .await
            .expect("run_smoke good");
        assert!(good.ok, "good page should pass, got: {good:?}");

        let broken = run_smoke(&browser, &format!("{base}/broken"), "#app", timeout)
            .await
            .expect("run_smoke broken");
        assert!(!broken.ok, "broken page should fail");
        // The failure must be captured in the report (empty render, console
        // error, and/or a failed sub-resource).
        assert!(
            !broken.failures.is_empty()
                || !broken.console_errors.is_empty()
                || !broken.failed_requests.is_empty(),
            "broken page should surface a captured failure: {broken:?}"
        );
    }

    #[test]
    fn smoke_report_render_pass_and_fail() {
        let pass = SmokeReport { ok: true, ..Default::default() };
        assert!(pass.render().contains("PASS"));

        let fail = SmokeReport {
            ok: false,
            failures: vec!["page did not render".into()],
            console_errors: vec!["boom".into()],
            failed_requests: vec!["404 https://boogy.ai/alice/todos/app.js".into()],
            ..Default::default()
        };
        let r = fail.render();
        assert!(r.contains("FAIL"));
        assert!(r.contains("page did not render"));
        assert!(r.contains("console error: boom"));
        assert!(r.contains("failed request: 404"));
    }

    #[test]
    fn resolve_browser_in_skips_missing_dirs() {
        let found = resolve_browser_in(
            &["chromium"],
            &[PathBuf::from("/no/such/dir/anywhere")],
        );
        assert!(found.is_none());
    }
}
