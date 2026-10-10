//! 5-hour and weekly usage for `mcc list`.
//!
//! Each profile's own OAuth access token — from `<config dir>/.credentials.json`,
//! or the macOS login Keychain — is sent to Anthropic's OAuth usage endpoint,
//! the one Claude Code's `/usage` screen reads. That endpoint is undocumented,
//! so every response field is optional and any failure degrades to an
//! "unavailable" status with a [`Reason`]; it never fails the command.
//!
//! Token hygiene: a token lives only in memory. [`AccessToken`] has a redacted
//! `Debug` and no `Display`; it is never logged, written to disk, or put into
//! a user-facing string, and HTTP errors are reduced to [`Reason`] codes so no
//! raw error text (which could echo headers) is ever surfaced. mcc never
//! refreshes tokens: an expired one is reported, not sent.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const DEFAULT_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA: &str = "oauth-2025-04-20";
/// Keychain service of the default profile; other config dirs get a hash suffix.
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Unavailable,
}

/// Why usage is unavailable. Serialized as snake_case in `mcc list --json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// No OAuth access token for the profile (logged out, or an API-key login).
    NotLoggedIn,
    /// The stored token is past its `expiresAt`, or the API rejected it (401).
    TokenExpired,
    /// The API refused the token (403), e.g. one without the profile scope.
    Unauthorized,
    /// HTTP 429.
    RateLimited,
    /// Connection failure or timeout.
    Network,
    /// Any other HTTP status.
    HttpError,
    /// The response was not the expected JSON.
    ParseError,
    /// Not fetched (`--no-usage` / `MCC_NO_USAGE`).
    Disabled,
}

impl Reason {
    /// Stable code, as in the JSON output.
    pub fn code(self) -> &'static str {
        match self {
            Reason::NotLoggedIn => "not_logged_in",
            Reason::TokenExpired => "token_expired",
            Reason::Unauthorized => "unauthorized",
            Reason::RateLimited => "rate_limited",
            Reason::Network => "network",
            Reason::HttpError => "http_error",
            Reason::ParseError => "parse_error",
            Reason::Disabled => "disabled",
        }
    }

    /// Human-readable explanation for `mcc list --long`.
    pub fn describe(self) -> &'static str {
        match self {
            Reason::NotLoggedIn => "not logged in",
            Reason::TokenExpired => "token expired; start the profile in Claude Code to refresh it",
            Reason::Unauthorized => "this login can't read usage",
            Reason::RateLimited => "rate limited; try again in a minute",
            Reason::Network => "offline or the usage API timed out",
            Reason::HttpError => "unexpected response from the usage API",
            Reason::ParseError => "unrecognized response from the usage API",
            Reason::Disabled => "not fetched: --no-usage",
        }
    }
}

/// One rate-limit window: percent used and when it resets.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Window {
    pub percent: f64,
    #[serde(serialize_with = "ser_time")]
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Usage {
    pub status: Status,
    pub reason: Option<Reason>,
    pub five_hour: Option<Window>,
    pub weekly: Option<Window>,
}

impl Usage {
    pub fn unavailable(reason: Reason) -> Self {
        Self {
            status: Status::Unavailable,
            reason: Some(reason),
            five_hour: None,
            weekly: None,
        }
    }

    fn ok(five_hour: Option<Window>, weekly: Option<Window>) -> Self {
        Self {
            status: Status::Ok,
            reason: None,
            five_hour,
            weekly,
        }
    }
}

/// Not fetched until `mcc list` asks for it.
impl Default for Usage {
    fn default() -> Self {
        Self::unavailable(Reason::Disabled)
    }
}

fn ser_time<S: Serializer>(t: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
    match t {
        Some(t) => s.serialize_str(&t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        None => s.serialize_none(),
    }
}

/// An OAuth access token. Deliberately has no `Display`; `Debug` is redacted.
pub struct AccessToken(String);

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

#[derive(Debug)]
enum Credential {
    Valid(AccessToken),
    Expired,
    Missing,
}

/// Parse a Claude Code credentials blob (the file, or the keychain secret):
/// `{"claudeAiOauth": {"accessToken": …, "expiresAt": <epoch ms>, …}}`.
fn parse_credential(bytes: &[u8], now: DateTime<Utc>) -> Credential {
    let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
        return Credential::Missing;
    };
    let oauth = v.get("claudeAiOauth");
    let Some(token) = oauth
        .and_then(|o| o.get("accessToken"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        return Credential::Missing;
    };
    let expires_ms = oauth
        .and_then(|o| o.get("expiresAt"))
        .and_then(Value::as_f64);
    match expires_ms {
        Some(ms) if ms <= now.timestamp_millis() as f64 => Credential::Expired,
        _ => Credential::Valid(AccessToken(token.to_string())),
    }
}

/// Keychain service name Claude Code uses for a config dir: unsuffixed for the
/// default (`CLAUDE_CONFIG_DIR` unset), otherwise suffixed with the first 8 hex
/// chars of SHA-256 of the `CLAUDE_CONFIG_DIR` string exactly as passed.
pub fn keychain_service(config_dir: &Path, is_default: bool) -> String {
    if is_default {
        return KEYCHAIN_SERVICE.to_string();
    }
    let digest = Sha256::digest(config_dir.to_string_lossy().as_bytes());
    let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{KEYCHAIN_SERVICE}-{hex}")
}

/// The keychain secret for `service`, via `MCC_SECURITY_BIN` or (macOS only)
/// `/usr/bin/security`. Output is captured, never inherited.
fn keychain_secret(service: &str) -> Option<Vec<u8>> {
    let bin = match std::env::var_os("MCC_SECURITY_BIN").filter(|v| !v.is_empty()) {
        Some(b) => PathBuf::from(b),
        None if cfg!(target_os = "macos") => PathBuf::from("/usr/bin/security"),
        None => return None,
    };
    let account = std::env::var("USER").ok().filter(|u| !u.is_empty())?;
    let out = Command::new(bin)
        .args(["find-generic-password", "-s", service, "-a", &account, "-w"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// The profile's token: the credentials file first, then the keychain (looked
/// up through `keychain`, so tests never reach the real one). A valid token
/// from either wins over an expired one from the other.
fn find_token(
    config_dir: &Path,
    is_default: bool,
    now: DateTime<Utc>,
    keychain: impl Fn(&str) -> Option<Vec<u8>>,
) -> Credential {
    let from_file = std::fs::read(config_dir.join(".credentials.json"))
        .map(|b| parse_credential(&b, now))
        .unwrap_or(Credential::Missing);
    if matches!(from_file, Credential::Valid(_)) {
        return from_file;
    }
    let from_keychain = keychain(&keychain_service(config_dir, is_default))
        .map(|b| parse_credential(&b, now))
        .unwrap_or(Credential::Missing);
    match (from_file, from_keychain) {
        (_, valid @ Credential::Valid(_)) => valid,
        (Credential::Expired, _) | (_, Credential::Expired) => Credential::Expired,
        _ => Credential::Missing,
    }
}

/// `MCC_USAGE_URL` (a test/debug override) or the real endpoint. The override
/// receives bearer tokens, so plain `http://` is only honored for loopback; a
/// refused override sends nothing anywhere (`None`) rather than falling back.
fn endpoint() -> Option<String> {
    match std::env::var("MCC_USAGE_URL") {
        Ok(url) if !url.is_empty() => {
            if url_allowed(&url) {
                Some(url)
            } else {
                crate::log::write(
                    "WARN",
                    "MCC_USAGE_URL refused (only https:// or loopback http:// URLs are allowed); usage not fetched",
                );
                None
            }
        }
        _ => Some(DEFAULT_URL.to_string()),
    }
}

fn url_allowed(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// `MCC_NO_USAGE` set to anything but "" / "0": usage is not fetched.
pub fn disabled_by_env() -> bool {
    std::env::var_os("MCC_NO_USAGE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// One profile to fetch usage for.
pub struct Target {
    /// Profile name, for the log.
    pub name: String,
    pub config_dir: PathBuf,
    pub is_default: bool,
}

/// Fetch usage for every target in parallel; results are in target order.
/// Each outcome is logged (reason codes only, never token material).
pub fn fetch_all(targets: &[Target]) -> Vec<Usage> {
    crate::log::write(
        "INFO",
        &format!("fetching 5h/weekly usage for {} profiles", targets.len()),
    );
    let all = fetch_parallel(targets);
    for (t, u) in targets.iter().zip(&all) {
        let outcome = match u.reason {
            Some(r) => format!("unavailable ({})", r.code()),
            None => "ok".into(),
        };
        crate::log::write("INFO", &format!("usage {}: {outcome}", t.name));
    }
    all
}

fn fetch_parallel(targets: &[Target]) -> Vec<Usage> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(4))
        // Never follow a redirect with the bearer token attached.
        .redirects(0)
        .user_agent(concat!("mcc/", env!("CARGO_PKG_VERSION")))
        .build();
    let url = endpoint();
    let now = Utc::now();
    std::thread::scope(|s| {
        let handles: Vec<_> = targets
            .iter()
            .map(|t| {
                let (agent, url) = (&agent, url.as_deref());
                s.spawn(move || fetch_one(agent, url, t, now))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Usage::unavailable(Reason::Network))
            })
            .collect()
    })
}

fn fetch_one(agent: &ureq::Agent, url: Option<&str>, t: &Target, now: DateTime<Utc>) -> Usage {
    let token = match find_token(&t.config_dir, t.is_default, now, keychain_secret) {
        Credential::Valid(token) => token,
        Credential::Expired => return Usage::unavailable(Reason::TokenExpired),
        Credential::Missing => return Usage::unavailable(Reason::NotLoggedIn),
    };
    let Some(url) = url else {
        return Usage::unavailable(Reason::Network);
    };
    let res = agent
        .get(url)
        .set("Authorization", &format!("Bearer {}", token.0))
        .set("anthropic-beta", BETA)
        .set("Accept", "application/json")
        .call();
    drop(token);
    match res {
        Ok(resp) if resp.status() == 200 => match resp.into_string() {
            Ok(body) => parse_response(&body).unwrap_or(Usage::unavailable(Reason::ParseError)),
            Err(_) => Usage::unavailable(Reason::Network),
        },
        Ok(_) => Usage::unavailable(Reason::HttpError),
        Err(ureq::Error::Status(code, _)) => Usage::unavailable(status_reason(code)),
        Err(ureq::Error::Transport(_)) => Usage::unavailable(Reason::Network),
    }
}

fn status_reason(code: u16) -> Reason {
    match code {
        401 => Reason::TokenExpired,
        403 => Reason::Unauthorized,
        429 => Reason::RateLimited,
        _ => Reason::HttpError,
    }
}

/// Parse the usage response: the flat `five_hour` / `seven_day` objects
/// (`utilization`, `resets_at`), falling back to `limits[]` entries of kind
/// `session` / `weekly_all` (`percent`, `resets_at`). `None` = not usage JSON.
fn parse_response(body: &str) -> Option<Usage> {
    let v: Value = serde_json::from_str(body).ok()?;
    let obj = v.as_object()?;
    let limit = |kind: &str| {
        v.get("limits")
            .and_then(Value::as_array)?
            .iter()
            .find(|l| l.get("kind").and_then(Value::as_str) == Some(kind))
            .and_then(|l| window(l, "percent"))
    };
    let five_hour = v
        .get("five_hour")
        .and_then(|w| window(w, "utilization"))
        .or_else(|| limit("session"));
    let weekly = v
        .get("seven_day")
        .and_then(|w| window(w, "utilization"))
        .or_else(|| limit("weekly_all"));
    let known = ["five_hour", "seven_day", "limits"]
        .iter()
        .any(|k| obj.contains_key(*k));
    (known || five_hour.is_some() || weekly.is_some()).then(|| Usage::ok(five_hour, weekly))
}

fn window(v: &Value, key: &str) -> Option<Window> {
    let percent = v.get(key)?.as_f64()?;
    let resets_at = v
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc));
    Some(Window { percent, resets_at })
}

/// "42%" (rounded).
pub fn percent(p: f64) -> String {
    format!("{}%", p.round() as i64)
}

/// Time left until `t`: "35m", "2h 14m", "3d 4h"; "now" once past.
pub fn until(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (t - now).num_seconds();
    if secs <= 0 {
        return "now".into();
    }
    let mins = (secs + 59) / 60;
    let (d, h, m) = (mins / 1440, mins % 1440 / 60, mins % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// The largest unit of [`until`]: "35m", "2h", "3d", "now".
pub fn until_short(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    until(t, now)
        .split(' ')
        .next()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn parses_full_response() {
        let body = r#"{
            "five_hour": {"utilization": 42, "resets_at": "2026-10-10T16:00:00.123+00:00"},
            "seven_day": {"utilization": 18.5, "resets_at": "2026-10-14T00:00:00Z"},
            "seven_day_opus": null,
            "limits": [{"kind": "session", "percent": 99, "resets_at": null}]
        }"#;
        let u = parse_response(body).unwrap();
        assert_eq!(u.status, Status::Ok);
        assert_eq!(u.reason, None);
        let five = u.five_hour.unwrap();
        assert_eq!(five.percent, 42.0);
        assert_eq!(five.resets_at, Some(at("2026-10-10T16:00:00.123Z")));
        let week = u.weekly.unwrap();
        assert_eq!(week.percent, 18.5);
        assert_eq!(week.resets_at, Some(at("2026-10-14T00:00:00Z")));
    }

    #[test]
    fn null_window_is_ok_without_data() {
        let u = parse_response(
            r#"{"five_hour": null, "seven_day": {"utilization": 3, "resets_at": null}}"#,
        )
        .unwrap();
        assert_eq!(u.status, Status::Ok);
        assert_eq!(u.five_hour, None);
        assert_eq!(
            u.weekly,
            Some(Window {
                percent: 3.0,
                resets_at: None
            })
        );
    }

    #[test]
    fn falls_back_to_limits() {
        let u = parse_response(
            r#"{"limits": [
                {"kind": "weekly_scoped", "percent": 70, "resets_at": "2026-10-12T00:00:00Z"},
                {"kind": "weekly_all", "percent": 25.25, "resets_at": "2026-10-13T00:00:00Z"},
                {"kind": "session", "percent": 7, "resets_at": "2026-10-10T05:00:00Z"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(u.five_hour.unwrap().percent, 7.0);
        let w = u.weekly.unwrap();
        assert_eq!(w.percent, 25.25);
        assert_eq!(w.resets_at, Some(at("2026-10-13T00:00:00Z")));
    }

    #[test]
    fn rejects_malformed_or_unknown_json() {
        assert_eq!(parse_response("not json"), None);
        assert_eq!(parse_response("[1, 2]"), None);
        assert_eq!(parse_response(r#"{"error": "nope"}"#), None);
        assert_eq!(
            parse_response(r#"{"five_hour": {"utilization": "x"}}"#)
                .unwrap()
                .five_hour,
            None
        );
    }

    #[test]
    fn json_shape() {
        let u = Usage::ok(
            Some(Window {
                percent: 42.0,
                resets_at: Some(at("2026-10-10T16:00:00Z")),
            }),
            None,
        );
        assert_eq!(
            serde_json::to_value(&u).unwrap(),
            serde_json::json!({
                "status": "ok",
                "reason": null,
                "five_hour": {"percent": 42.0, "resets_at": "2026-10-10T16:00:00Z"},
                "weekly": null
            })
        );
        assert_eq!(
            serde_json::to_value(Usage::unavailable(Reason::TokenExpired)).unwrap(),
            serde_json::json!({"status": "unavailable", "reason": "token_expired", "five_hour": null, "weekly": null})
        );
        for r in [
            Reason::NotLoggedIn,
            Reason::TokenExpired,
            Reason::Unauthorized,
            Reason::RateLimited,
            Reason::Network,
            Reason::HttpError,
            Reason::ParseError,
            Reason::Disabled,
        ] {
            assert_eq!(serde_json::to_value(r).unwrap(), r.code());
        }
        assert_eq!(Usage::default().reason, Some(Reason::Disabled));
    }

    #[test]
    fn keychain_service_names() {
        assert_eq!(
            keychain_service(Path::new("/Users/me/.claude"), true),
            "Claude Code-credentials"
        );
        // printf '%s' '/Users/me/.mcc/profiles/work' | shasum -a 256 → 8fa2c68f…
        assert_eq!(
            keychain_service(Path::new("/Users/me/.mcc/profiles/work"), false),
            "Claude Code-credentials-8fa2c68f"
        );
    }

    #[test]
    fn credential_expiry() {
        let now = at("2026-10-10T12:00:00Z");
        let blob = |exp: i64| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"tok-x","refreshToken":"r","expiresAt":{exp}}}}}"#
            )
        };
        let ms = now.timestamp_millis();
        assert!(matches!(
            parse_credential(blob(ms + 60_000).as_bytes(), now),
            Credential::Valid(AccessToken(ref t)) if t == "tok-x"
        ));
        assert!(matches!(
            parse_credential(blob(ms - 1).as_bytes(), now),
            Credential::Expired
        ));
        assert!(matches!(
            parse_credential(br#"{"claudeAiOauth":{"accessToken":"tok-x"}}"#, now),
            Credential::Valid(_)
        ));
        for missing in [
            &br#"{"secret":"x"}"#[..],
            br#"{"claudeAiOauth":{"accessToken":""}}"#,
            b"garbage",
        ] {
            assert!(matches!(
                parse_credential(missing, now),
                Credential::Missing
            ));
        }
    }

    #[test]
    fn find_token_sources_and_precedence() {
        let d = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let blob = |tok: &str, exp: i64| {
            format!(r#"{{"claudeAiOauth":{{"accessToken":"{tok}","expiresAt":{exp}}}}}"#)
        };
        let past = now.timestamp_millis() - 1000;
        let future = now.timestamp_millis() + 3_600_000;
        let none = |_: &str| None;
        let asked = std::cell::RefCell::new(Vec::new());
        let keychain = |service: &str| {
            asked.borrow_mut().push(service.to_string());
            Some(blob("from-keychain", future).into_bytes())
        };
        let token = |c: Credential| match c {
            Credential::Valid(AccessToken(t)) => t,
            other => format!("{other:?}"),
        };

        assert_eq!(token(find_token(d.path(), false, now, none)), "Missing");
        // Nothing in the file: the keychain item for this dir is used.
        assert_eq!(
            token(find_token(d.path(), false, now, keychain)),
            "from-keychain"
        );
        assert_eq!(
            asked.borrow().as_slice(),
            [keychain_service(d.path(), false)]
        );
        // An expired file token loses to a valid keychain one…
        std::fs::write(d.path().join(".credentials.json"), blob("old", past)).unwrap();
        assert_eq!(token(find_token(d.path(), false, now, none)), "Expired");
        assert_eq!(
            token(find_token(d.path(), true, now, keychain)),
            "from-keychain"
        );
        assert_eq!(asked.borrow().last().unwrap(), "Claude Code-credentials");
        // …and a valid file token is used without asking the keychain.
        std::fs::write(
            d.path().join(".credentials.json"),
            blob("from-file", future),
        )
        .unwrap();
        let before = asked.borrow().len();
        assert_eq!(
            token(find_token(d.path(), false, now, keychain)),
            "from-file"
        );
        assert_eq!(asked.borrow().len(), before);
    }

    #[test]
    fn override_url_must_be_https_or_loopback() {
        for ok in [
            "https://example.com/u",
            "http://127.0.0.1:9/",
            "http://localhost:8080/api/oauth/usage",
            "http://[::1]:7/",
        ] {
            assert!(url_allowed(ok), "{ok}");
        }
        for bad in [
            "http://example.com/",
            "http://127.0.0.1.evil.com/",
            "http://localhost@evil.com/",
            "ftp://127.0.0.1/",
            "127.0.0.1:9",
        ] {
            assert!(!url_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn http_status_mapping() {
        assert_eq!(status_reason(401), Reason::TokenExpired);
        assert_eq!(status_reason(403), Reason::Unauthorized);
        assert_eq!(status_reason(429), Reason::RateLimited);
        assert_eq!(status_reason(500), Reason::HttpError);
    }

    #[test]
    fn formats_time_left_and_percent() {
        let now = at("2026-10-10T12:00:00Z");
        let plus = |s: i64| now + chrono::Duration::seconds(s);
        assert_eq!(until(plus(-5), now), "now");
        assert_eq!(until(plus(30), now), "1m");
        assert_eq!(until(plus(35 * 60), now), "35m");
        assert_eq!(until(plus(2 * 3600 + 14 * 60), now), "2h 14m");
        assert_eq!(until(plus(3 * 86400 + 4 * 3600), now), "3d 4h");
        assert_eq!(until_short(plus(3 * 86400 + 4 * 3600), now), "3d");
        assert_eq!(until_short(plus(2 * 3600 + 14 * 60), now), "2h");
        assert_eq!(percent(41.6), "42%");
        assert_eq!(percent(0.0), "0%");
    }

    #[test]
    fn token_debug_is_redacted() {
        let t = AccessToken("sekrit-value".into());
        let shown = format!(
            "{t:?} {:?}",
            Credential::Valid(AccessToken("sekrit-value".into()))
        );
        assert!(!shown.contains("sekrit"), "{shown}");
        assert!(shown.contains("<redacted>"));
    }
}
