//! GitHub Releases API client: latest-release discovery, with a prerelease-list fallback.
//!
//! All HTTP goes through `ureq` (rustls, no system OpenSSL). The pure helpers
//! ([`tag_name_from_release_json`], [`release_from_json`], [`release_from_list_json`]) are
//! unit-tested directly against fixture JSON; the functions that actually hit the network are
//! thin wrappers only exercised at resolve time.
//!
//! This module resolves against the **public GitHub Releases API only**. It does not speak the
//! signed `updates.dig.net` feed (dig-updater's own concern — issue #513); it is the documented
//! GitHub-native fallback/bootstrap source every consumer can rely on even before that feed
//! exists, and stays available afterward as an independent cross-check.

use crate::repo::{version_from_tag, Repo};

/// GitHub requires a User-Agent on API requests.
const USER_AGENT: &str = concat!("dig-release-resolver/", env!("CARGO_PKG_VERSION"));

/// The environment variable an optional GitHub token is read from (unauthenticated
/// `api.github.com` calls are capped at 60/hour per source IP, a limit CI runners — which share a
/// huge, heavily-used IP pool — hit routinely; a token raises it to 5,000/hour). Matches the name
/// GitHub Actions already exposes as `secrets.GITHUB_TOKEN` and the `gh` CLI convention, so a
/// caller's CI needs no new secret. Entirely optional: every call works unauthenticated exactly
/// as before when it is unset.
const GITHUB_TOKEN_ENV: &str = "GITHUB_TOKEN";

/// A GitHub release reduced to what a resolver needs: the tag and the names of every uploaded
/// asset (so a caller's own asset matcher can pick the right one, instead of betting on a single
/// guessed filename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag_name: String,
    pub asset_names: Vec<String>,
}

/// Parse the `tag_name` out of a GitHub release JSON payload. Pure — takes the raw body, returns
/// the tag (e.g. `v0.6.0`).
pub fn tag_name_from_release_json(body: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("parse release JSON: {e}"))?;
    v.get("tag_name")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "release JSON had no tag_name".to_string())
}

/// Extract a [`Release`] (tag + asset names) from a single release JSON object
/// (`serde_json::Value`). Shared by [`release_from_json`] (a single-release API response) and
/// [`release_from_list_json`] (one entry of a releases-list response) so both parse identically.
fn release_from_value(v: &serde_json::Value) -> Result<Release, String> {
    let tag_name = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "release JSON had no tag_name".to_string())?;
    let asset_names = v
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        tag_name,
        asset_names,
    })
}

/// Parse a GitHub release JSON payload into a [`Release`] (tag + asset names). Pure — the heart
/// of the resolution logic, unit-tested without a network.
pub fn release_from_json(body: &str) -> Result<Release, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("parse release JSON: {e}"))?;
    release_from_value(&v)
}

/// Parse a GitHub *releases list* JSON payload (an array, newest first) into the newest
/// [`Release`], regardless of its prerelease/draft flags.
///
/// This is the fallback for [`latest_release`] when `/releases/latest` 404s: that endpoint
/// excludes prereleases AND drafts, so a repo whose newest (or only) release is
/// prerelease-flagged — e.g. DIG Browser's alpha channel — never appears there even though a
/// real, asset-bearing release exists. The list endpoint has no such filter, so its first entry is
/// the newest release GitHub knows about.
pub fn release_from_list_json(body: &str) -> Result<Release, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("parse releases list JSON: {e}"))?;
    let arr = v
        .as_array()
        .ok_or_else(|| "releases list JSON was not an array".to_string())?;
    let first = arr
        .first()
        .ok_or_else(|| "no releases published".to_string())?;
    release_from_value(first)
}

/// True when a release-lookup error indicates "no such release" (HTTP 404) — the signal that
/// `/releases/latest` found nothing published, so the caller should fall back to the full
/// releases list ([`release_from_list_json`]) rather than treating it as a transport failure.
fn is_release_not_found(err: &str) -> bool {
    err.contains("404") || err.contains("Not Found")
}

/// Discover the latest published tag for a repo via the GitHub API.
pub fn latest_tag(repo: &Repo) -> Result<String, String> {
    Ok(latest_release(repo)?.tag_name)
}

/// Resolve the latest published **version** (bare semver, e.g. `"0.15.0"`) for a component's
/// [`Repo`] — [`latest_release`] plus [`version_from_tag`] in one call, the single entry point
/// most callers want when all they need is a string to feed [`crate::decision::decide`].
pub fn latest_version(repo: &Repo) -> Result<String, String> {
    Ok(version_from_tag(&latest_release(repo)?.tag_name))
}

/// Fetch the latest release (tag + asset list) for a repo via the GitHub API.
///
/// Tries `/releases/latest` first; that endpoint excludes prereleases and drafts, so it 404s for
/// a repo whose newest release is prerelease-only (DIG Browser's alpha channel). On a 404, fall
/// back to the full releases list ([`release_from_list_json`]) and take the newest entry
/// regardless of prerelease status. Repos that always ship a non-prerelease "latest" (the common
/// case) never hit the fallback.
pub fn latest_release(repo: &Repo) -> Result<Release, String> {
    latest_release_with(repo, get_text)
}

/// [`latest_release`] with an injectable GET — the seam that lets the 404-then-fall-back-to-the-
/// releases-list branch run against a fixture in tests instead of the real GitHub API. Production
/// wires this to [`get_text`]; see `latest_release`.
fn latest_release_with(
    repo: &Repo,
    get: impl Fn(&str) -> Result<String, String>,
) -> Result<Release, String> {
    match get(&repo.latest_release_api()) {
        Ok(body) => release_from_json(&body),
        Err(e) if is_release_not_found(&e) => {
            let body = get(&repo.releases_list_api())?;
            release_from_list_json(&body)
        }
        Err(e) => Err(e),
    }
}

/// Fetch a specific release by tag (tag + asset list) via the GitHub API.
pub fn release_by_tag(repo: &Repo, tag: &str) -> Result<Release, String> {
    let url = repo.release_by_tag_api(tag);
    let body = get_text(&url)?;
    release_from_json(&body)
}

/// GET a URL as text with the GitHub API headers, optionally authenticated via
/// [`GITHUB_TOKEN_ENV`] (see [`get_text_with_token`]). Internal helper — the production entry
/// point every `latest_release`/`release_by_tag` call goes through.
fn get_text(url: &str) -> Result<String, String> {
    get_text_with_token(url, std::env::var(GITHUB_TOKEN_ENV).ok().as_deref())
}

/// [`get_text`] with an injectable token — the pure-ish core so the Authorization-header decision
/// is unit-tested (against a real local socket) without mutating the process environment.
/// `token: None` sends the SAME anonymous request as before this option existed.
fn get_text_with_token(url: &str, token: Option<&str>) -> Result<String, String> {
    let mut req = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json");
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let resp = req.call().map_err(|e| format!("GET {url}: {e}"))?;
    resp.into_string().map_err(|e| format!("read {url}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_tag_name() {
        let body = r#"{"tag_name":"v0.6.0","name":"digstore v0.6.0","draft":false}"#;
        assert_eq!(tag_name_from_release_json(body).unwrap(), "v0.6.0");
    }

    #[test]
    fn errors_without_tag_name() {
        assert!(tag_name_from_release_json(r#"{"name":"x"}"#).is_err());
        assert!(tag_name_from_release_json("not json").is_err());
    }

    #[test]
    fn release_from_json_extracts_tag_and_asset_names() {
        let body = r#"{
            "tag_name": "v0.6.0",
            "assets": [
                {"name": "digstore-0.6.0-linux-x64", "size": 123},
                {"name": "digstore-0.6.0-windows-x64.exe"},
                {"name": "digstore-0.6.0-macos-arm64"}
            ]
        }"#;
        let r = release_from_json(body).unwrap();
        assert_eq!(r.tag_name, "v0.6.0");
        assert_eq!(
            r.asset_names,
            vec![
                "digstore-0.6.0-linux-x64".to_string(),
                "digstore-0.6.0-windows-x64.exe".to_string(),
                "digstore-0.6.0-macos-arm64".to_string(),
            ]
        );
    }

    #[test]
    fn release_from_json_tolerates_no_assets() {
        let r = release_from_json(r#"{"tag_name":"v1.0.0"}"#).unwrap();
        assert_eq!(r.tag_name, "v1.0.0");
        assert!(r.asset_names.is_empty());
    }

    #[test]
    fn release_from_json_errors_without_tag() {
        assert!(release_from_json(r#"{"assets":[]}"#).is_err());
        assert!(release_from_json("not json").is_err());
    }

    #[test]
    fn release_from_json_skips_assets_without_a_name() {
        // An asset entry missing `name` is filtered out (not a crash, not an empty string) — only
        // well-formed asset names survive.
        let body = r#"{
            "tag_name": "v1.2.3",
            "assets": [
                {"size": 10},
                {"name": "good-1.2.3-linux-x64"},
                {"name": 42}
            ]
        }"#;
        let r = release_from_json(body).unwrap();
        assert_eq!(r.tag_name, "v1.2.3");
        assert_eq!(r.asset_names, vec!["good-1.2.3-linux-x64".to_string()]);
    }

    #[test]
    fn release_from_json_treats_non_array_assets_as_empty() {
        // `assets` present but not an array → no asset names (no panic).
        let r = release_from_json(r#"{"tag_name":"v1.0.0","assets":"oops"}"#).unwrap();
        assert!(r.asset_names.is_empty());
    }

    #[test]
    fn is_release_not_found_detects_404_variants() {
        // ureq's Status Display is "{url}: status code {code}"; get_text wraps it as
        // "GET {url}: {ureq display}" — both forms must be recognised, plus the plain-English
        // "Not Found" GitHub itself sometimes returns.
        assert!(is_release_not_found(
            "GET https://api.github.com/x: https://api.github.com/x: status code 404"
        ));
        assert!(is_release_not_found(
            "GET https://api.github.com/x: 404 Not Found"
        ));
        assert!(!is_release_not_found(
            "GET https://api.github.com/x: status code 500"
        ));
        assert!(!is_release_not_found(
            "GET https://api.github.com/x: timed out"
        ));
    }

    #[test]
    fn release_from_list_json_takes_the_newest_entry_regardless_of_prerelease() {
        // Regression: DIG Browser's only release (149.0.7827.155-1.1-alpha) is
        // prerelease-flagged, so GitHub's `/releases/latest` (which excludes
        // prereleases/drafts) 404s even though a real release exists. The fallback list-parse
        // must pick the newest (first) entry regardless of its prerelease flag.
        let body = r#"[
            {
                "tag_name": "149.0.7827.155-1.1-alpha",
                "prerelease": true,
                "draft": false,
                "assets": [
                    {"name": "ungoogled-chromium_149.0.7827.155-1.1_installer_x64.exe"},
                    {"name": "ungoogled-chromium_149.0.7827.155-1.1_windows_x64.zip"}
                ]
            },
            {
                "tag_name": "148.0.0.0-1.0-alpha",
                "prerelease": true,
                "draft": false,
                "assets": []
            }
        ]"#;
        let r = release_from_list_json(body).unwrap();
        assert_eq!(r.tag_name, "149.0.7827.155-1.1-alpha");
        assert_eq!(
            r.asset_names,
            vec![
                "ungoogled-chromium_149.0.7827.155-1.1_installer_x64.exe".to_string(),
                "ungoogled-chromium_149.0.7827.155-1.1_windows_x64.zip".to_string(),
            ]
        );
    }

    #[test]
    fn release_from_list_json_errors_on_empty_list() {
        let err = release_from_list_json("[]").unwrap_err();
        assert!(err.contains("no releases"), "got: {err}");
    }

    #[test]
    fn release_from_list_json_errors_on_non_array() {
        assert!(release_from_list_json(r#"{"tag_name":"v1.0.0"}"#).is_err());
        assert!(release_from_list_json("not json").is_err());
    }

    // -- get_text_with_token: the optional GitHub-auth header --------------------------------
    //
    // Drives the REAL `ureq` request against a one-shot local server that echoes back whatever
    // `Authorization` header it received (or `NONE`), so the assertion is on the actual wire
    // request `get_text_with_token` sends — not a re-statement of its own `if let` branch. Uses
    // an injected `token: Option<&str>` (never a real env var), so these run safely under Rust's
    // parallel test harness with no shared mutable state.

    /// A one-shot HTTP/1.1 server that reads the request line + headers, replies 200 with the
    /// received `Authorization` header value (or `NONE`) as the body, then exits.
    fn one_shot_echo_auth_server() -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(std::time::Duration::from_millis(500)))
                    .ok();
                let mut buf = [0u8; 4096];
                let mut request = Vec::new();
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            request.extend_from_slice(&buf[..n]);
                            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let text = String::from_utf8_lossy(&request);
                let auth = text
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .map(|l| l.split_once(':').map_or("", |(_, v)| v).trim().to_string())
                    .unwrap_or_else(|| "NONE".to_string());
                let body = format!("{{\"tag_name\":\"v0.0.0\",\"__auth\":\"{auth}\"}}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
        port
    }

    #[test]
    fn get_text_with_token_sends_no_authorization_header_when_token_is_none() {
        let port = one_shot_echo_auth_server();
        let body = get_text_with_token(&format!("http://127.0.0.1:{port}/"), None).unwrap();
        assert!(body.contains(r#""__auth":"NONE""#), "got: {body}");
    }

    #[test]
    fn get_text_with_token_sends_no_authorization_header_when_token_is_empty() {
        // An empty string is treated the same as absent — never sends a hollow
        // `Authorization: Bearer` header.
        let port = one_shot_echo_auth_server();
        let body = get_text_with_token(&format!("http://127.0.0.1:{port}/"), Some("")).unwrap();
        assert!(body.contains(r#""__auth":"NONE""#), "got: {body}");
    }

    #[test]
    fn get_text_with_token_sends_a_bearer_authorization_header_when_present() {
        let port = one_shot_echo_auth_server();
        let body =
            get_text_with_token(&format!("http://127.0.0.1:{port}/"), Some("ghp_test123")).unwrap();
        assert!(
            body.contains(r#""__auth":"Bearer ghp_test123""#),
            "got: {body}"
        );
    }

    #[test]
    fn get_text_reads_the_real_github_token_env_var() {
        // get_text (the production entry point) reads GITHUB_TOKEN_ENV itself; this only proves
        // the constant names the variable CI already exposes (`secrets.GITHUB_TOKEN`) — the
        // header-sending behavior itself is covered token-injected above, never via a real env
        // mutation (parallel-test-safe).
        assert_eq!(GITHUB_TOKEN_ENV, "GITHUB_TOKEN");
    }

    // -- latest_release_with: the 404 -> releases-list fallback, actually exercised ----------
    //
    // The production `latest_release` hits the real `api.github.com`, which can't be redirected
    // to a fixture — so the fallback branch is proven here against `latest_release_with`'s
    // injected `get`, using a fixed repo and asserting on WHICH url each call received.

    fn some_repo() -> Repo {
        Repo::new("DIG-Network", "digstore", "digstore")
    }

    #[test]
    fn latest_release_with_returns_the_latest_endpoints_release_when_it_succeeds() {
        let repo = some_repo();
        let release = latest_release_with(&repo, |url| {
            assert_eq!(
                url,
                repo.latest_release_api(),
                "must try /releases/latest first"
            );
            Ok(
                r#"{"tag_name":"v1.2.3","assets":[{"name":"digstore-1.2.3-linux-x64"}]}"#
                    .to_string(),
            )
        })
        .unwrap();
        assert_eq!(release.tag_name, "v1.2.3");
    }

    #[test]
    fn latest_release_with_falls_back_to_the_releases_list_on_404() {
        let repo = some_repo();
        let release = latest_release_with(&repo, |url| {
            if url == repo.latest_release_api() {
                Err("GET ...: status code 404".to_string())
            } else {
                assert_eq!(
                    url,
                    repo.releases_list_api(),
                    "a 404 on /latest must fall back to the full releases list"
                );
                Ok(r#"[{"tag_name":"v0.9.0-alpha","prerelease":true,"assets":[]}]"#.to_string())
            }
        })
        .unwrap();
        assert_eq!(
            release.tag_name, "v0.9.0-alpha",
            "the prerelease is still returned"
        );
    }

    #[test]
    fn latest_release_with_propagates_a_non_404_transport_error_without_falling_back() {
        let repo = some_repo();
        let err =
            latest_release_with(&repo, |_| Err("GET ...: timed out".to_string())).unwrap_err();
        assert!(err.contains("timed out"), "got: {err}");
    }

    #[test]
    fn latest_version_strips_the_v_prefix_from_the_resolved_tag() {
        // latest_version() = version_from_tag(latest_release()?.tag_name); prove the composition
        // by driving the same injectable seam latest_release_with uses.
        let repo = some_repo();
        let release = latest_release_with(&repo, |_| {
            Ok(r#"{"tag_name":"v0.15.0","assets":[]}"#.to_string())
        })
        .unwrap();
        assert_eq!(version_from_tag(&release.tag_name), "0.15.0");
    }
}
