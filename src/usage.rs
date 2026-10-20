//! 5-hour and weekly usage for `mcc list`, `mcc usage` and `mcc run`.
//!
//! By default usage comes from the rate limits `mcc statusline` caches in
//! `~/.mcc/state/<profile>/usage.json` ([`read_all`]): no token is read and
//! nothing touches the network. A window whose `resets_at` has passed is
//! obsolete and dropped; a profile with nothing left shows [`Reason::NoData`].
//!
//! The live lookup ([`fetch_all`]) runs only with `[usage] live = true`, after
//! a one-time [`LIVE_DISCLOSURE`]. It sends each profile's own OAuth access
//! token — from `<config dir>/.credentials.json`, or the macOS login Keychain —
//! to Anthropic's OAuth usage endpoint, the one Claude Code's `/usage` screen
//! reads. That endpoint is undocumented, so every response field is optional
//! and any failure degrades to an "unavailable" status with a [`Reason`]; it
//! never fails the command.
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

use crate::commands::statusline::USAGE_FILE;
use crate::paths::Paths;

pub const DEFAULT_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA: &str = "oauth-2025-04-20";
/// Keychain service of the default profile; other config dirs get a hash suffix.
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Printed on stderr the first time a command looks usage up live
/// (`[usage] live = true`); `[usage] live_disclosed_at` records that it was.
pub const LIVE_DISCLOSURE: &str = "⚠ Live usage lookup is on ([usage] live = true in ~/.mcc/config.toml): mcc reads each profile's Claude Code login (its OAuth access token, from .credentials.json or the macOS Keychain) and sends it to Anthropic's undocumented, rate-limited usage endpoint (https://api.anthropic.com/api/oauth/usage). Anthropic says third-party tools may not collect or intermediate Claude.ai credentials (https://code.claude.com/docs/en/legal-and-compliance), so use this at your own discretion. Set live = false to read usage only from the `mcc statusline` cache. This note is shown once.";

/// Where a [`Usage`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The rate limits `mcc statusline` cached.
    Cache,
    /// The OAuth usage endpoint (`[usage] live = true`).
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Unavailable,
}

/// Why usage is unavailable. Serialized as snake_case in the `--json` output.
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
    /// Nothing current in the statusline cache: never cached (Team or
    /// Enterprise seats, idle profiles, no `mcc statusline`), or every cached
    /// window has reset since.
    NoData,
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
            Reason::NoData => "no_data",
        }
    }

    /// Human-readable explanation for `mcc list --long` and `mcc usage`.
    pub fn describe(self) -> &'static str {
        match self {
            Reason::NotLoggedIn => "not logged in",
            Reason::TokenExpired => "token expired; start the profile in Claude Code to refresh it",
            Reason::Unauthorized => "this login can't read usage",
            Reason::RateLimited => "rate limited; try again in a minute",
            Reason::Network => "offline or the usage API timed out",
            Reason::HttpError => "unexpected response from the usage API",
            Reason::ParseError => "unrecognized response from the usage API",
            Reason::Disabled => "not fetched: --no-usage or MCC_NO_USAGE",
            Reason::NoData => "no recent usage cached; run /usage in Claude Code",
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
    /// `None` when nothing was looked up (`disabled`).
    pub source: Option<Source>,
    /// When the statusline cached these numbers (`None` for live lookups).
    #[serde(serialize_with = "ser_time")]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Usage {
    pub fn unavailable(reason: Reason) -> Self {
        Self {
            status: Status::Unavailable,
            reason: Some(reason),
            five_hour: None,
            weekly: None,
            source: None,
            updated_at: None,
        }
    }

    fn ok(five_hour: Option<Window>, weekly: Option<Window>) -> Self {
        Self {
            status: Status::Ok,
            reason: None,
            five_hour,
            weekly,
            source: None,
            updated_at: None,
        }
    }

    pub fn with_source(mut self, source: Source) -> Self {
        self.source = Some(source);
        self
    }

    /// "12m ago" for cached usage, `None` otherwise.
    pub fn age(&self, now: DateTime<Utc>) -> Option<String> {
        self.updated_at.map(|t| age(t, now))
    }
}

/// Not fetched until a command asks for it.
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
    let account = account_name(std::env::var("USER").ok())?;
    let out = Command::new(bin)
        .args(["find-generic-password", "-s", service, "-a", &account, "-w"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// The Keychain account Claude Code stores its login under: the login name.
/// `$USER` when set (`user`), else the password database entry for this uid
/// (`getpwuid_r`), else `id -un` — launchd jobs, cron and some IDE tasks run
/// without `USER`.
fn account_name(user: Option<String>) -> Option<String> {
    user.filter(|u| !u.is_empty())
        .or_else(passwd_name)
        .or_else(id_un)
}

/// The current uid's login name from the password database.
fn passwd_name() -> Option<String> {
    let size = match unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) } {
        n if n > 0 => n as usize,
        _ => 16 * 1024,
    };
    let mut buf: Vec<libc::c_char> = vec![0; size];
    // SAFETY: `passwd` is plain data; getpwuid_r fills it, pointing into `buf`,
    // which outlives every read below.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        )
    };
    if rc != 0 || found.is_null() || pwd.pw_name.is_null() {
        return None;
    }
    // SAFETY: on success pw_name is a NUL-terminated string inside `buf`.
    let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
    name.to_str()
        .ok()
        .filter(|n| !n.is_empty())
        .map(str::to_string)
}

/// `id -un`, the last resort.
fn id_un() -> Option<String> {
    let out = Command::new("id")
        .arg("-un")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !name.is_empty()).then_some(name)
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

/// Whether a profile has a usable OAuth access token, without the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenState {
    Valid,
    /// Past its `expiresAt`.
    Expired,
    /// No OAuth token (logged out, or an API-key login).
    Missing,
}

/// The profile's [`TokenState`] (for `mcc doctor`): read like the live
/// lookup reads it (credentials file, then the keychain), so callers must
/// only call it with `[usage] live = true` and `MCC_NO_USAGE` unset. The
/// token itself is dropped here, never returned.
pub fn token_state(config_dir: &Path, is_default: bool) -> TokenState {
    match find_token(config_dir, is_default, Utc::now(), keychain_secret) {
        Credential::Valid(_) => TokenState::Valid,
        Credential::Expired => TokenState::Expired,
        Credential::Missing => TokenState::Missing,
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
    /// Profile name: its `~/.mcc/state/` directory, and for the log.
    pub name: String,
    pub config_dir: PathBuf,
    pub is_default: bool,
}

/// Every target's usage, in target order: looked up live with `live`
/// (`[usage] live = true`), else read from the statusline cache.
pub fn lookup(paths: &Paths, targets: &[Target], live: bool) -> Vec<Usage> {
    if live {
        fetch_all(targets)
    } else {
        read_all(paths, targets)
    }
}

/// Every target's usage from the statusline cache
/// (`~/.mcc/state/<name>/usage.json`); no token, no network.
pub fn read_all(paths: &Paths, targets: &[Target]) -> Vec<Usage> {
    let now = Utc::now();
    let all: Vec<Usage> = targets
        .iter()
        .map(|t| {
            let path = paths.profile_state_dir(&t.name).join(USAGE_FILE);
            read_cached(&path, now)
        })
        .collect();
    log_outcomes("cached", targets, &all);
    all
}

/// One profile's cached usage. Windows that have reset since are obsolete
/// and dropped; nothing left (or no readable cache) is [`Reason::NoData`].
fn read_cached(path: &Path, now: DateTime<Utc>) -> Usage {
    let no_data = || Usage::unavailable(Reason::NoData).with_source(Source::Cache);
    let Some(v) = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return no_data();
    };
    let updated_at = v.get("updated_at").and_then(timestamp);
    // A window without a reset time is obsolete once a whole window has
    // passed since it was cached.
    let current = |key: &str, length: chrono::Duration| {
        let w = v.get(key)?;
        let window = Window {
            percent: w.get("used_percentage")?.as_f64()?,
            resets_at: w.get("resets_at").and_then(timestamp),
        };
        let ends = window.resets_at.or(updated_at.map(|t| t + length));
        match ends {
            Some(t) if t <= now => None,
            _ => Some(window),
        }
    };
    let (five_hour, weekly) = (
        current("five_hour", chrono::Duration::hours(5)),
        current("seven_day", chrono::Duration::days(7)),
    );
    if five_hour.is_none() && weekly.is_none() {
        return no_data();
    }
    let mut u = Usage::ok(five_hour, weekly).with_source(Source::Cache);
    u.updated_at = updated_at;
    u
}

/// A time as Claude Code's status line gives it (epoch seconds) or as an
/// RFC 3339 string.
fn timestamp(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::Number(n) => DateTime::from_timestamp(n.as_f64()? as i64, 0),
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        _ => None,
    }
}

/// Fetch usage for every target in parallel; results are in target order.
/// Each outcome is logged (reason codes only, never token material).
pub fn fetch_all(targets: &[Target]) -> Vec<Usage> {
    crate::log::write(
        "INFO",
        &format!("fetching 5h/weekly usage for {} profiles", targets.len()),
    );
    let all: Vec<Usage> = fetch_parallel(targets)
        .into_iter()
        .map(|u| u.with_source(Source::Live))
        .collect();
    log_outcomes("live", targets, &all);
    all
}

fn log_outcomes(kind: &str, targets: &[Target], all: &[Usage]) {
    for (t, u) in targets.iter().zip(all) {
        let outcome = match u.reason {
            Some(r) => format!("unavailable ({})", r.code()),
            None => "ok".into(),
        };
        crate::log::write("INFO", &format!("usage {} ({kind}): {outcome}", t.name));
    }
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

/// `t` in local time, e.g. "Sat Oct 10 16:00".
pub fn local_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&chrono::Local)
        .format("%a %b %-d %H:%M")
        .to_string()
}

/// The hint under a table where some profiles have no cached usage
/// ([`Reason::NoData`]), naming them; `None` when every profile has some.
pub fn no_data_hint(names: &[&str], usages: &[&Usage]) -> Option<String> {
    let missing: Vec<String> = names
        .iter()
        .zip(usages)
        .filter(|(_, u)| u.reason == Some(Reason::NoData))
        .map(|(n, _)| format!("`{n}`"))
        .collect();
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "no recent usage cached for {}: run /usage in Claude Code to see it; `mcc statusline install` caches it as you work (Team and Enterprise seats may report none)",
        missing.join(", ")
    ))
}

/// How long ago `t` was: "just now", "12m ago", "3h ago", "2d ago".
pub fn age(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match (now - t).num_seconds().max(0) {
        0..60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
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
                "weekly": null,
                "source": null,
                "updated_at": null
            })
        );
        assert_eq!(
            serde_json::to_value(
                Usage::unavailable(Reason::TokenExpired).with_source(Source::Live)
            )
            .unwrap(),
            serde_json::json!({"status": "unavailable", "reason": "token_expired", "five_hour": null, "weekly": null, "source": "live", "updated_at": null})
        );
        let mut cached = Usage::unavailable(Reason::NoData).with_source(Source::Cache);
        cached.updated_at = Some(at("2026-10-10T12:00:00Z"));
        assert_eq!(
            serde_json::to_value(&cached).unwrap()["updated_at"],
            "2026-10-10T12:00:00Z"
        );
        assert_eq!(serde_json::to_value(Source::Cache).unwrap(), "cache");
        for r in [
            Reason::NotLoggedIn,
            Reason::TokenExpired,
            Reason::Unauthorized,
            Reason::RateLimited,
            Reason::Network,
            Reason::HttpError,
            Reason::ParseError,
            Reason::Disabled,
            Reason::NoData,
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
        assert!(
            regex::Regex::new(r"^[A-Z][a-z]{2} [A-Z][a-z]{2} \d{1,2} \d{2}:\d{2}$")
                .unwrap()
                .is_match(&local_time(now)),
            "{}",
            local_time(now)
        );
        assert_eq!(percent(0.0), "0%");
    }

    #[test]
    fn reads_the_statusline_cache() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("usage.json");
        let now = at("2026-10-10T12:00:00Z");
        let epoch = |t: &str| at(t).timestamp();
        std::fs::write(
            &path,
            format!(
                r#"{{"profile":"work","updated_at":"2026-10-10T11:48:00Z","five_hour":{{"used_percentage":42.4,"resets_at":{}}},"seven_day":{{"used_percentage":18,"resets_at":{}}}}}"#,
                epoch("2026-10-10T14:00:00Z"),
                epoch("2026-10-14T00:00:00Z")
            ),
        )
        .unwrap();
        let u = read_cached(&path, now);
        assert_eq!(u.status, Status::Ok);
        assert_eq!(u.source, Some(Source::Cache));
        assert_eq!(
            u.five_hour,
            Some(Window {
                percent: 42.4,
                resets_at: Some(at("2026-10-10T14:00:00Z"))
            })
        );
        assert_eq!(u.weekly.as_ref().unwrap().percent, 18.0);
        assert_eq!(u.age(now).as_deref(), Some("12m ago"));
    }

    #[test]
    fn cached_windows_that_have_reset_are_obsolete() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("usage.json");
        let now = at("2026-10-10T12:00:00Z");
        // The 5-hour window reset an hour ago; the weekly one is current
        // (RFC 3339 times are read too).
        std::fs::write(
            &path,
            r#"{"updated_at":"2026-10-10T06:00:00Z","five_hour":{"used_percentage":99,"resets_at":"2026-10-10T11:00:00Z"},"seven_day":{"used_percentage":30,"resets_at":"2026-10-12T00:00:00Z"}}"#,
        )
        .unwrap();
        let u = read_cached(&path, now);
        assert_eq!(u.status, Status::Ok);
        assert_eq!(u.five_hour, None);
        assert_eq!(u.weekly.unwrap().percent, 30.0);
        // Both reset: nothing current is left.
        std::fs::write(
            &path,
            r#"{"updated_at":"2026-10-01T06:00:00Z","five_hour":{"used_percentage":99,"resets_at":"2026-10-01T11:00:00Z"},"seven_day":{"used_percentage":30,"resets_at":"2026-10-08T00:00:00Z"}}"#,
        )
        .unwrap();
        let u = read_cached(&path, now);
        assert_eq!(u.status, Status::Unavailable);
        assert_eq!(u.reason, Some(Reason::NoData));
        assert_eq!(u.source, Some(Source::Cache)); // No reset time: obsolete a full window after it was cached.
        std::fs::write(
            &path,
            r#"{"updated_at":"2026-10-10T06:00:00Z","five_hour":{"used_percentage":100,"resets_at":null},"seven_day":{"used_percentage":30,"resets_at":null}}"#,
        )
        .unwrap();
        let u = read_cached(&path, now);
        assert_eq!(u.five_hour, None);
        assert_eq!(u.weekly.unwrap().resets_at, None);
    }

    #[test]
    fn missing_or_broken_cache_is_no_data() {
        let d = tempfile::tempdir().unwrap();
        let now = at("2026-10-10T12:00:00Z");
        let path = d.path().join("usage.json");
        assert_eq!(read_cached(&path, now).reason, Some(Reason::NoData));
        for body in ["not json", "{}", r#"{"five_hour":{"resets_at":1}}"#] {
            std::fs::write(&path, body).unwrap();
            assert_eq!(
                read_cached(&path, now).reason,
                Some(Reason::NoData),
                "{body}"
            );
        }
    }

    #[test]
    fn ages() {
        let now = at("2026-10-10T12:00:00Z");
        let ago = |s: i64| age(now - chrono::Duration::seconds(s), now);
        assert_eq!(ago(-30), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(12 * 60), "12m ago");
        assert_eq!(ago(3 * 3600 + 5), "3h ago");
        assert_eq!(ago(2 * 86400), "2d ago");
    }

    #[test]
    fn no_data_hint_names_the_profiles_without_cached_usage() {
        let none = Usage::unavailable(Reason::NoData);
        let off = Usage::unavailable(Reason::Disabled);
        assert_eq!(no_data_hint(&["a"], &[&off]), None);
        let hint = no_data_hint(&["a", "b", "c"], &[&none, &off, &none]).unwrap();
        assert!(
            hint.starts_with("no recent usage cached for `a`, `c`:"),
            "{hint}"
        );
        assert!(hint.contains("/usage"), "{hint}");
    }

    #[test]
    fn account_name_falls_back_when_user_is_unset() {
        assert_eq!(
            account_name(Some("someone".into())).as_deref(),
            Some("someone")
        );
        // A uid without a passwd entry (some containers) has no name to find.
        let Some(expected) = id_un() else { return };
        // Unset or empty USER: the password database (or `id -un`) names
        // the same login.
        assert_eq!(account_name(None).as_deref(), Some(expected.as_str()));
        assert_eq!(
            account_name(Some(String::new())).as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(passwd_name().as_deref(), Some(expected.as_str()));
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
