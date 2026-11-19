//! Devin: a provider mcc shows beside the Claude Code profiles (#124).
//!
//! Devin is not a profile. It has one login, the Devin CLI's own, kept where
//! the CLI keeps it ([`crate::paths::devin_credentials`]); mcc never signs
//! in, never moves or writes that file, and starts the CLI as is (`mcc
//! devin`). Devin shows up in `mcc list`, `mcc usage` and bare `mcc` only
//! when that credentials file exists ([`detected`]).
//!
//! Usage, as custats-macos reads it: the session token (the top-level
//! `windsurf_api_key` of `credentials.toml`, sent whole as a bearer token)
//! goes to `GET {base}/organizations` (the primary organization, else the
//! first) and then `GET {base}/{org}/billing/quota/usage`, with base
//! `https://app.devin.ai/api`. The answer has a weekly window, a daily one
//! unless the plan hides it, and an overage balance in dollars.
//!
//! Like the Claude lookup ([`crate::usage`]), it runs only with `[usage]
//! live = true` (after a one-time [`crate::usage::DEVIN_LIVE_DISCLOSURE`]) or
//! for `mcc usage --all`, never with `MCC_NO_USAGE`. Otherwise mcc shows what
//! the last lookup saved in `~/.mcc/state/devin/quota.json` (the numbers and
//! the organization, never the token), and reads no token at all.
//!
//! Token hygiene: [`SessionToken`] has a redacted `Debug` and no `Display`.
//! It lives in memory for one lookup, re-read from the file each time, and
//! is never logged, cached or written; failures are reduced to [`Reason`]
//! codes so no raw error text (which could echo headers) is ever shown.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::CliError;
use crate::paths::Paths;
use crate::usage::{Reason, Source, Usage, Window};

/// The name Devin goes by in mcc's tables and JSON, and its command.
pub const NAME: &str = "devin";

/// Devin's API, as custats-macos calls it.
pub const DEFAULT_API_URL: &str = "https://app.devin.ai/api";

/// The `detail` Devin answers a 401/403 with when the login has no
/// organization: a scope problem, not a rejected token.
const NO_ORGANIZATIONS_DETAIL: &str = "No organizations found for auth1 user";

/// The key of the session token in `credentials.toml` (a legacy name).
const TOKEN_KEY: &str = "windsurf_api_key";

/// Whether Devin is set up here: its CLI's credentials file exists. Only the
/// file's existence is checked; nothing is read.
pub fn detected(paths: &Paths) -> bool {
    paths.devin_credentials.exists()
}

// ---------------------------------------------------------------- the login

/// Devin's session token. Deliberately has no `Display`; `Debug` is redacted.
pub struct SessionToken(String);

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// Why no token came out of the credentials file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialsError {
    NotFound,
    PermissionDenied,
    /// Not UTF-8 text (or unreadable for another reason).
    DecodeFailed,
    /// No `windsurf_api_key`, or an empty one.
    MissingToken,
}

impl CredentialsError {
    pub fn reason(self) -> Reason {
        match self {
            CredentialsError::NotFound => Reason::NotLoggedIn,
            CredentialsError::PermissionDenied => Reason::PermissionDenied,
            CredentialsError::DecodeFailed => Reason::DecodeFailed,
            CredentialsError::MissingToken => Reason::MissingToken,
        }
    }
}

/// Read the session token from `path`, opened read-only and read afresh
/// each time (the Devin CLI owns it and may rotate the token).
pub fn load_token(path: &Path) -> Result<SessionToken, CredentialsError> {
    let bytes = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CredentialsError::NotFound,
        std::io::ErrorKind::PermissionDenied => CredentialsError::PermissionDenied,
        _ => CredentialsError::DecodeFailed,
    })?;
    parse_credentials(&bytes)
}

/// The token in a `credentials.toml`, read the way custats-macos reads it:
/// the first line whose key is exactly `windsurf_api_key` (only whitespace
/// before the `=`), its value with one pair of matching quotes removed.
/// Every other key (`api_server_url`, `devin_webapp_host`, ...) is ignored.
pub fn parse_credentials(bytes: &[u8]) -> Result<SessionToken, CredentialsError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CredentialsError::DecodeFailed)?;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix(TOKEN_KEY) else {
            continue;
        };
        let Some((gap, value)) = rest.split_once('=') else {
            continue;
        };
        if !gap.chars().all(|c| c == ' ' || c == '\t') {
            continue;
        }
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        let token = value.trim();
        if token.is_empty() {
            return Err(CredentialsError::MissingToken);
        }
        return Ok(SessionToken(token.to_string()));
    }
    Err(CredentialsError::MissingToken)
}

// ---------------------------------------------------------------- the CLI

/// Locate the devin binary: `MCC_DEVIN_BIN`, then `$PATH`, then
/// `~/.local/bin/devin`, where Devin's installer links it.
pub fn find_binary(paths: &Paths) -> Option<PathBuf> {
    find_binary_in(
        std::env::var_os("MCC_DEVIN_BIN"),
        std::env::var_os("PATH"),
        &paths.local_bin,
    )
}

fn find_binary_in(
    override_bin: Option<OsString>,
    path: Option<OsString>,
    local_bin: &Path,
) -> Option<PathBuf> {
    if let Some(p) = override_bin.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    path.iter()
        .flat_map(std::env::split_paths)
        .chain([local_bin.to_path_buf()])
        .map(|d| d.join(NAME))
        .find(|p| crate::fsutil::is_executable(p))
}

pub fn require_binary(paths: &Paths) -> anyhow::Result<PathBuf> {
    find_binary(paths).ok_or_else(|| {
        CliError::runtime("the `devin` command was not found on your PATH or in ~/.local/bin")
            .hint("install the Devin CLI (https://docs.devin.ai), or set MCC_DEVIN_BIN=/path/to/devin")
            .into()
    })
}

// ---------------------------------------------------------------- the API

/// `MCC_DEVIN_API_URL` (a test/debug override) or [`DEFAULT_API_URL`]. The
/// override receives the session token, so plain `http://` is only honored
/// for loopback; a refused override sends nothing anywhere (`None`).
fn api_base() -> Option<String> {
    match std::env::var("MCC_DEVIN_API_URL") {
        Ok(url) if !url.is_empty() => {
            if crate::usage::url_allowed(&url) {
                Some(url.trim_end_matches('/').to_string())
            } else {
                crate::log::write(
                    "WARN",
                    "MCC_DEVIN_API_URL refused (only https:// or loopback http:// URLs are allowed); Devin usage not fetched",
                );
                None
            }
        }
        _ => Some(DEFAULT_API_URL.to_string()),
    }
}

/// The HTTP client for Devin's API, which carries the session token: the
/// guarantees of the Claude usage request (2 s to connect, 4 s in all, no
/// redirects, no proxy from the environment, an honest `mcc/<version>`
/// User-Agent), but with HTTP errors returned as responses, so a 401/403's
/// body can tell "no organization" from a rejected token.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(Some(Duration::from_secs(4)))
        .max_redirects(0)
        .proxy(None)
        .http_status_as_error(false)
        .user_agent(concat!("mcc/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// GET `url` with the token (and `x-cog-org-id` when `org` is given); the
/// body of a 200, else why not. 401/403 are [`Reason::Unauthorized`] unless
/// the body's `detail` says the login has no organization; any other status
/// (a redirect included: they are never followed) is [`Reason::HttpError`].
fn get(
    agent: &ureq::Agent,
    url: &str,
    token: &SessionToken,
    org: Option<&str>,
) -> Result<String, Reason> {
    let mut req = agent
        .get(url)
        .header("Authorization", &format!("Bearer {}", token.0))
        .header("Accept", "application/json");
    if let Some(org) = org {
        req = req.header("x-cog-org-id", org);
    }
    let mut resp = req.call().map_err(|_| Reason::Network)?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string();
    match status {
        200 => body.map_err(|_| Reason::Network),
        401 | 403 if body.as_deref().is_ok_and(no_organizations_detail) => {
            Err(Reason::NoOrganizations)
        }
        401 | 403 => Err(Reason::Unauthorized),
        _ => Err(Reason::HttpError),
    }
}

/// Whether an error body is Devin's "No organizations found" answer.
fn no_organizations_detail(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("detail")?
                .as_str()
                .map(|d| d == NO_ORGANIZATIONS_DETAIL)
        })
        .unwrap_or(false)
}

/// One entry of `GET /organizations`.
#[derive(Debug, Clone, PartialEq)]
struct Org {
    id: String,
    name: Option<String>,
    primary: bool,
}

/// The organizations in a `/organizations` body; `None` when it is not a
/// JSON array. Entries without a non-empty `org_id` are skipped; a name
/// with control characters (which could drive the terminal) is dropped.
fn parse_orgs(body: &str) -> Option<Vec<Org>> {
    let v: Value = serde_json::from_str(body).ok()?;
    let orgs = v
        .as_array()?
        .iter()
        .filter_map(|o| {
            let o = o.as_object()?;
            let id = o.get("org_id")?.as_str().filter(|s| !s.is_empty())?;
            let name = ["display_name", "name"]
                .iter()
                .find_map(|k| o.get(*k)?.as_str())
                .filter(|n| !n.chars().any(char::is_control))
                .map(str::to_string);
            Some(Org {
                id: id.to_string(),
                name,
                primary: o
                    .get("is_primary_org")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect();
    Some(orgs)
}

/// The primary organization, else the first, as the Devin web app scopes a
/// session.
fn select_org(orgs: &[Org]) -> Option<&Org> {
    orgs.iter().find(|o| o.primary).or_else(|| orgs.first())
}

/// The `org-…` form of an organization id the API uses (`org_…` → `org-…`).
fn canonical_org_id(id: &str) -> String {
    match id.strip_prefix("org_") {
        Some(rest) => format!("org-{rest}"),
        None => id.to_string(),
    }
}

/// Devin's quota, as `GET /{org}/billing/quota/usage` reports it.
#[derive(Debug, Clone, PartialEq)]
struct Quota {
    daily: Option<Window>,
    weekly: Option<Window>,
    overage_usd: Option<f64>,
}

/// Parse a quota body as custats-macos does: percentages as numbers or
/// numeric strings (truncated to whole percents), the daily window only
/// when `hide_daily_quota` is not true, resets as RFC 3339 times (with an
/// offset) or Unix seconds, and `overage_balance` in dollars, else
/// `overage_balance_cents` / 100. Not a JSON object: [`Reason::ParseError`];
/// neither window: [`Reason::MissingQuota`].
fn parse_quota(body: &str) -> Result<Quota, Reason> {
    let v: Value = serde_json::from_str(body).map_err(|_| Reason::ParseError)?;
    let obj = v.as_object().ok_or(Reason::ParseError)?;
    let hides_daily = obj
        .get("hide_daily_quota")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let window = |pct: &str, reset: &str| {
        let percent = obj.get(pct).and_then(number)?;
        if !(i64::MIN as f64..=i64::MAX as f64).contains(&percent) {
            return None;
        }
        Some(Window {
            percent: percent.trunc(),
            resets_at: obj.get(reset).and_then(reset_time),
        })
    };
    let daily = if hides_daily {
        None
    } else {
        window("daily_percentage", "daily_reset_at")
    };
    let weekly = window("weekly_percentage", "weekly_reset_at");
    if daily.is_none() && weekly.is_none() {
        return Err(Reason::MissingQuota);
    }
    let overage_usd = obj.get("overage_balance").and_then(number).or_else(|| {
        obj.get("overage_balance_cents")
            .and_then(number)
            .map(|c| c / 100.0)
    });
    Ok(Quota {
        daily,
        weekly,
        overage_usd,
    })
}

/// A number, or a string holding one; never a boolean.
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

/// A reset time: RFC 3339 (`2026-09-15T00:00:00-08:00`), or Unix seconds
/// as a number or a numeric string.
fn reset_time(v: &Value) -> Option<DateTime<Utc>> {
    let secs = match v {
        Value::String(s) => {
            if let Ok(t) = DateTime::parse_from_rfc3339(s) {
                return Some(t.with_timezone(&Utc));
            }
            number(v)?
        }
        _ => number(v)?,
    };
    (secs.is_finite() && secs.abs() < 1e13)
        .then(|| DateTime::from_timestamp(secs as i64, 0))
        .flatten()
}

/// The organization a lookup used.
#[derive(Debug, Clone, PartialEq)]
struct OrgRef {
    id: String,
    name: Option<String>,
}

/// One lookup: the organization (`known`, else discovered), then its quota.
fn fetch(
    agent: &ureq::Agent,
    base: &str,
    token: &SessionToken,
    known: Option<OrgRef>,
) -> Result<(Quota, OrgRef), Reason> {
    let org = match known {
        Some(org) => org,
        None => {
            let body = get(agent, &format!("{base}/organizations"), token, None)?;
            let orgs = parse_orgs(&body).ok_or(Reason::ParseError)?;
            let o = select_org(&orgs).ok_or(Reason::NoOrganizations)?;
            OrgRef {
                id: canonical_org_id(&o.id),
                name: o.name.clone(),
            }
        }
    };
    let url = format!("{base}/{}/billing/quota/usage", org.id);
    let body = get(agent, &url, token, Some(&org.id))?;
    Ok((parse_quota(&body)?, org))
}

// ---------------------------------------------------------------- the cache

/// What `~/.mcc/state/devin/quota.json` keeps of the last successful lookup:
/// the numbers and the organization, never the token. Times are Unix
/// seconds, as in the statusline cache.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cached {
    updated_at: i64,
    org_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    org_name: Option<String>,
    #[serde(default)]
    daily: Option<CachedWindow>,
    #[serde(default)]
    weekly: Option<CachedWindow>,
    #[serde(default)]
    overage_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CachedWindow {
    percent: f64,
    #[serde(default)]
    resets_at: Option<i64>,
}

impl From<&Window> for CachedWindow {
    fn from(w: &Window) -> Self {
        Self {
            percent: w.percent,
            resets_at: w.resets_at.map(|t| t.timestamp()),
        }
    }
}

fn read_cache(paths: &Paths) -> Option<Cached> {
    serde_json::from_slice(&std::fs::read(paths.devin_quota_cache()).ok()?).ok()
}

/// Best effort: a failed write is logged, never an error.
fn write_cache(paths: &Paths, c: &Cached) {
    let path = paths.devin_quota_cache();
    let written = serde_json::to_value(c)
        .map_err(anyhow::Error::from)
        .and_then(|v| crate::commands::statusline::write_json(&path, &v));
    match written {
        Ok(()) => crate::log::write("INFO", "usage devin: saved to the cache"),
        Err(e) => crate::log::write(
            "WARN",
            &format!("usage devin: not saved to the cache: {e:#}"),
        ),
    }
}

/// The cached usage still current at `now`: a window that has reset (or,
/// without a reset time, is a whole window old) is dropped; with neither
/// left it is [`Reason::NotLookedUp`].
fn cached_usage(c: &Cached, now: DateTime<Utc>) -> Usage {
    let updated_at = DateTime::from_timestamp(c.updated_at, 0);
    let current = |w: &Option<CachedWindow>, length: chrono::Duration| {
        let w = w.as_ref()?;
        let resets_at = w.resets_at.and_then(|t| DateTime::from_timestamp(t, 0));
        match resets_at.or(updated_at.map(|t| t + length)) {
            Some(t) if t > now => Some(Window {
                percent: w.percent,
                resets_at,
            }),
            _ => None,
        }
    };
    let daily = current(&c.daily, chrono::Duration::days(1));
    let weekly = current(&c.weekly, chrono::Duration::days(7));
    if daily.is_none() && weekly.is_none() {
        return Usage::unavailable(Reason::NotLookedUp).with_source(Source::Cache);
    }
    let mut u = Usage::ok(None, weekly).with_source(Source::Cache);
    u.daily = daily;
    u.overage_usd = c.overage_usd;
    u.updated_at = updated_at;
    u
}

// ---------------------------------------------------------------- lookups

/// How a command gets Devin's usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// `MCC_NO_USAGE` (or `--no-usage`): nothing read.
    Disabled,
    /// The last lookup's numbers; no token, no network.
    Cached,
    /// Ask Devin's API now (`[usage] live = true`, `mcc usage --all`).
    Live,
}

impl Lookup {
    /// [`Lookup::Live`] with `live`, else [`Lookup::Cached`];
    /// [`Lookup::Disabled`] whenever `MCC_NO_USAGE` is set.
    pub fn of(live: bool) -> Self {
        if crate::usage::disabled_by_env() {
            Lookup::Disabled
        } else if live {
            Lookup::Live
        } else {
            Lookup::Cached
        }
    }
}

/// Devin as a command shows it.
#[derive(Debug, Clone)]
pub struct Devin {
    pub usage: Usage,
    /// The organization the quota is for (from the lookup, else the cache).
    pub org_id: Option<String>,
    pub org_name: Option<String>,
}

/// Devin's usage, looked up as `how` says. A live lookup that succeeds is
/// saved to the cache; one that fails leaves the cache as it was. Never an
/// error: failures are an "unavailable" usage with a [`Reason`].
pub fn lookup(paths: &Paths, how: Lookup) -> Devin {
    let cached = (how != Lookup::Disabled)
        .then(|| read_cache(paths))
        .flatten();
    let (org_id, org_name) = cached.as_ref().map_or((None, None), |c| {
        (Some(c.org_id.clone()), c.org_name.clone())
    });
    let mut d = Devin {
        usage: Usage::default(),
        org_id,
        org_name,
    };
    match how {
        Lookup::Disabled => {}
        Lookup::Cached => {
            d.usage = match &cached {
                Some(c) => cached_usage(c, Utc::now()),
                None => Usage::unavailable(Reason::NotLookedUp).with_source(Source::Cache),
            };
        }
        Lookup::Live => {
            let known = cached.as_ref().map(|c| OrgRef {
                id: c.org_id.clone(),
                name: c.org_name.clone(),
            });
            let result = load_token(&paths.devin_credentials)
                .map_err(CredentialsError::reason)
                .and_then(|token| {
                    let base = api_base().ok_or(Reason::Network)?;
                    fetch(&agent(), &base, &token, known)
                });
            d.usage = match result {
                Ok((q, org)) => {
                    let now = Utc::now();
                    write_cache(
                        paths,
                        &Cached {
                            updated_at: now.timestamp(),
                            org_id: org.id.clone(),
                            org_name: org.name.clone(),
                            daily: q.daily.as_ref().map(CachedWindow::from),
                            weekly: q.weekly.as_ref().map(CachedWindow::from),
                            overage_usd: q.overage_usd,
                        },
                    );
                    d.org_id = Some(org.id);
                    d.org_name = org.name;
                    let mut u = Usage::ok(None, q.weekly);
                    u.daily = q.daily;
                    u.overage_usd = q.overage_usd;
                    u.with_source(Source::Live)
                }
                Err(r) => Usage::unavailable(r).with_source(Source::Live),
            };
        }
    }
    let kind = match how {
        Lookup::Disabled => "disabled",
        Lookup::Cached => "cached",
        Lookup::Live => "live",
    };
    let outcome = match d.usage.reason {
        Some(r) => format!("unavailable ({})", r.code()),
        None => "ok".into(),
    };
    crate::log::write("INFO", &format!("usage {NAME} ({kind}): {outcome}"));
    d
}

/// Why Devin's usage is unavailable, in words, where Devin's fix differs
/// from what [`Reason::describe`] says for a Claude Code profile.
pub fn describe(r: Reason) -> &'static str {
    match r {
        Reason::NotLoggedIn => "not logged in; run `devin auth login`",
        Reason::Unauthorized | Reason::TokenExpired => {
            "Devin rejected the session token; run `devin auth login`"
        }
        Reason::Network => "offline or Devin's API timed out",
        Reason::HttpError => "unexpected response from Devin's API",
        Reason::ParseError => "unrecognized response from Devin's API",
        Reason::RateLimited => "rate limited; try again in a minute",
        other => other.describe(),
    }
}

/// "$3.50".
pub fn dollars(v: f64) -> String {
    format!("${v:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn token(r: Result<SessionToken, CredentialsError>) -> Result<String, CredentialsError> {
        r.map(|t| t.0)
    }

    #[test]
    fn reads_only_the_top_level_session_token() {
        let file = "api_server_url = \"https://example.invalid\"\n\
                    windsurf_api_key_extra = \"nope\"\n\
                    windsurf_api_key = \"test-token\"\n\
                    devin_webapp_host = \"x\"\n\
                    windsurf_api_key = \"second\"\n";
        assert_eq!(
            token(parse_credentials(file.as_bytes())),
            Ok("test-token".into())
        );
        // Single quotes, tabs, surrounding spaces, no quotes at all.
        for (line, want) in [
            ("windsurf_api_key = 'test-token'", "test-token"),
            ("  windsurf_api_key\t=\t\"test-token\"  ", "test-token"),
            ("windsurf_api_key=test-token", "test-token"),
            ("windsurf_api_key = \"test$token\"", "test$token"),
        ] {
            assert_eq!(
                token(parse_credentials(line.as_bytes())),
                Ok(want.into()),
                "{line}"
            );
        }
    }

    #[test]
    fn credentials_errors() {
        for missing in [
            "",
            "api_server_url = \"x\"\n",
            "windsurf_api_key = \"\"\n",
            "windsurf_api_key = '  '\n",
            "windsurf_api_key\n",
            "windsurf_api_key_x = \"test-token\"\n",
        ] {
            assert_eq!(
                token(parse_credentials(missing.as_bytes())),
                Err(CredentialsError::MissingToken),
                "{missing:?}"
            );
        }
        assert_eq!(
            token(parse_credentials(&[0xff, 0xfe, b'x'])),
            Err(CredentialsError::DecodeFailed)
        );
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            token(load_token(&d.path().join("credentials.toml"))),
            Err(CredentialsError::NotFound)
        );
        assert_eq!(CredentialsError::NotFound.reason(), Reason::NotLoggedIn);
        assert_eq!(
            CredentialsError::MissingToken.reason().code(),
            "missing_token"
        );
    }

    #[test]
    fn credentials_file_is_only_read() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("credentials.toml");
        let body = "windsurf_api_key = \"test-token\"\n";
        std::fs::write(&path, body).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(token(load_token(&path)), Ok("test-token".into()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        // Re-read each time: a rotated token is picked up.
        std::fs::write(&path, "windsurf_api_key = \"test-token-2\"\n").unwrap();
        assert_eq!(token(load_token(&path)), Ok("test-token-2".into()));
    }

    #[test]
    fn token_debug_is_redacted() {
        let t = SessionToken("test-token".into());
        assert_eq!(format!("{t:?}"), "SessionToken(<redacted>)");
    }

    #[test]
    fn binary_override_then_path_then_local_bin() {
        let d = tempfile::tempdir().unwrap();
        let (on_path, local) = (d.path().join("path"), d.path().join("local"));
        for dir in [&on_path, &local] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let path = Some(OsString::from(&on_path));
        assert_eq!(find_binary_in(None, path.clone(), &local), None);
        let make = |dir: &Path| {
            let p = dir.join(NAME);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let local_devin = make(&local);
        assert_eq!(
            find_binary_in(None, path.clone(), &local),
            Some(local_devin)
        );
        let path_devin = make(&on_path);
        assert_eq!(find_binary_in(None, path.clone(), &local), Some(path_devin));
        assert_eq!(
            find_binary_in(Some("/x/devin".into()), path.clone(), &local),
            Some(PathBuf::from("/x/devin"))
        );
        assert!(find_binary_in(Some("".into()), path, &local).is_some());
    }

    #[test]
    fn organizations_primary_first_else_the_first() {
        let orgs = parse_orgs(
            r#"[
                {"org_id": "", "name": "empty"},
                {"name": "no id"},
                {"org_id": "org_a", "name": "A"},
                {"org_id": "org-b", "display_name": "B team", "name": "b", "is_primary_org": true}
            ]"#,
        )
        .unwrap();
        assert_eq!(orgs.len(), 2);
        let o = select_org(&orgs).unwrap();
        assert_eq!(
            (o.id.as_str(), o.name.as_deref()),
            ("org-b", Some("B team"))
        );
        let first =
            parse_orgs(r#"[{"org_id": "org_a", "name": "A"}, {"org_id": "org-c"}]"#).unwrap();
        assert_eq!(select_org(&first).unwrap().id, "org_a");
        assert_eq!(select_org(&parse_orgs("[]").unwrap()), None);
        assert_eq!(parse_orgs(r#"{"org_id": "x"}"#), None);
        assert_eq!(parse_orgs("nope"), None);
        // A name that could drive the terminal is dropped, the org kept.
        let bad = parse_orgs("[{\"org_id\": \"o\", \"name\": \"A\\u001b[2J\"}]").unwrap();
        assert_eq!(bad[0].name, None);
        assert_eq!(canonical_org_id("org_abc"), "org-abc");
        assert_eq!(canonical_org_id("org-abc"), "org-abc");
        assert_eq!(canonical_org_id("x_org_"), "x_org_");
    }

    /// custats-macos's fixture: weekly used, the daily window hidden.
    #[test]
    fn real_payload_has_weekly_and_hides_daily() {
        let q = parse_quota(
            r#"{
                "is_quota_plan": true,
                "has_quota_allocation": true,
                "daily_percentage": 0,
                "weekly_percentage": 54,
                "daily_reset_at": "2026-09-15T00:00:00-08:00",
                "weekly_reset_at": "2026-09-20T00:00:00-08:00",
                "overage_balance": 0.0,
                "hide_daily_quota": true
            }"#,
        )
        .unwrap();
        assert_eq!(q.daily, None);
        assert_eq!(
            q.weekly,
            Some(Window {
                percent: 54.0,
                resets_at: Some(at("2026-09-20T08:00:00Z"))
            })
        );
        assert_eq!(q.overage_usd, Some(0.0));
    }

    #[test]
    fn visible_daily_window() {
        let q = parse_quota(
            r#"{"daily_percentage": 12, "weekly_percentage": 30,
                "daily_reset_at": "2026-09-15T00:00:00-08:00",
                "weekly_reset_at": 1789200000, "hide_daily_quota": false}"#,
        )
        .unwrap();
        assert_eq!(
            q.daily,
            Some(Window {
                percent: 12.0,
                resets_at: Some(at("2026-09-15T08:00:00Z"))
            })
        );
        let w = q.weekly.unwrap();
        assert_eq!(w.resets_at, DateTime::from_timestamp(1_789_200_000, 0));
        // A numeric-string reset is Unix seconds too.
        let q =
            parse_quota(r#"{"weekly_percentage": 1, "weekly_reset_at": "1789200000"}"#).unwrap();
        assert_eq!(
            q.weekly.unwrap().resets_at,
            DateTime::from_timestamp(1_789_200_000, 0)
        );
    }

    #[test]
    fn overage_balance_then_cents() {
        let q = |b: &str| parse_quota(b).unwrap().overage_usd;
        assert_eq!(
            q(r#"{"weekly_percentage": 10, "overage_balance": 3.5, "hide_daily_quota": true}"#),
            Some(3.5)
        );
        assert_eq!(
            q(
                r#"{"weekly_percentage": 10, "overage_balance_cents": 250, "hide_daily_quota": true}"#
            ),
            Some(2.5)
        );
        assert_eq!(
            q(
                r#"{"weekly_percentage": 10, "overage_balance": "1.25", "overage_balance_cents": 999}"#
            ),
            Some(1.25)
        );
        assert_eq!(q(r#"{"weekly_percentage": 10}"#), None);
    }

    #[test]
    fn percentages_accept_strings_and_doubles_and_truncate() {
        let q = parse_quota(
            r#"{"weekly_percentage": "54", "daily_percentage": 54.9, "hide_daily_quota": false}"#,
        )
        .unwrap();
        assert_eq!(q.weekly.unwrap().percent, 54.0);
        assert_eq!(q.daily.unwrap().percent, 54.0);
        let q = parse_quota(r#"{"weekly_percentage": "12.7"}"#).unwrap();
        assert_eq!(q.weekly.unwrap().percent, 12.0);
    }

    #[test]
    fn booleans_are_not_numbers() {
        assert_eq!(
            parse_quota(
                r#"{"weekly_percentage": true, "daily_percentage": false, "hide_daily_quota": false}"#
            ),
            Err(Reason::MissingQuota)
        );
        let q = parse_quota(
            r#"{"weekly_percentage": 54, "overage_balance": true, "hide_daily_quota": true}"#,
        )
        .unwrap();
        assert_eq!(q.overage_usd, None);
    }

    #[test]
    fn missing_quota_and_invalid_json() {
        assert_eq!(
            parse_quota(r#"{"is_quota_plan": true}"#),
            Err(Reason::MissingQuota)
        );
        // A hidden daily window doesn't count as one.
        assert_eq!(
            parse_quota(r#"{"daily_percentage": 5, "hide_daily_quota": true}"#),
            Err(Reason::MissingQuota)
        );
        for bad in ["not json", "[1, 2]", "\"x\"", "null"] {
            assert_eq!(parse_quota(bad), Err(Reason::ParseError), "{bad}");
        }
    }

    #[test]
    fn no_organizations_detail_only_exactly() {
        assert!(no_organizations_detail(
            r#"{"detail": "No organizations found for auth1 user"}"#
        ));
        for other in [
            r#"{"detail": "Unauthorized"}"#,
            r#"{"detail": ["No organizations found for auth1 user"]}"#,
            "No organizations found for auth1 user",
            "",
        ] {
            assert!(!no_organizations_detail(other), "{other}");
        }
    }

    #[test]
    fn cached_windows_that_have_reset_are_dropped() {
        let now = at("2026-10-10T12:00:00Z");
        let w = |percent, resets_at: Option<DateTime<Utc>>| {
            Some(CachedWindow {
                percent,
                resets_at: resets_at.map(|t| t.timestamp()),
            })
        };
        let mut c = Cached {
            updated_at: at("2026-10-10T11:00:00Z").timestamp(),
            org_id: "org-a".into(),
            org_name: None,
            daily: w(5.0, Some(at("2026-10-10T11:30:00Z"))),
            weekly: w(40.0, Some(at("2026-10-12T00:00:00Z"))),
            overage_usd: Some(2.0),
        };
        let u = cached_usage(&c, now);
        assert_eq!(u.reason, None);
        assert_eq!(u.daily, None);
        assert_eq!(u.five_hour, None);
        assert_eq!(u.weekly.unwrap().percent, 40.0);
        assert_eq!(u.overage_usd, Some(2.0));
        assert_eq!(u.source, Some(Source::Cache));
        assert_eq!(u.updated_at, Some(at("2026-10-10T11:00:00Z")));
        // No reset time: a whole window after it was saved.
        c.weekly = w(40.0, None);
        c.updated_at = at("2026-10-01T00:00:00Z").timestamp();
        let u = cached_usage(&c, now);
        assert_eq!(u.reason, Some(Reason::NotLookedUp));
    }

    // ---- HTTP, against a loopback server

    /// A loopback server answering each request with `handler(request line,
    /// headers)`; returns its base URL and the raw requests received.
    fn serve(
        handler: impl Fn(&str) -> (u16, String) + Send + 'static,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut req = String::new();
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line == "\r\n" => break,
                        Ok(_) => req.push_str(&line),
                    }
                }
                let (status, body) = handler(&req);
                log.lock().unwrap().push(req);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
            }
        });
        (base, seen)
    }

    fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
        req.lines().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }

    fn test_token() -> SessionToken {
        SessionToken("test-token".into())
    }

    #[test]
    fn agent_keeps_the_request_guarantees() {
        let agent = agent();
        let config = agent.config();
        assert_eq!(config.timeouts().connect, Some(Duration::from_secs(2)));
        assert_eq!(config.timeouts().global, Some(Duration::from_secs(4)));
        assert_eq!(config.max_redirects(), 0);
        assert!(config.proxy().is_none());
        assert!(!config.http_status_as_error());
    }

    #[test]
    fn fetch_finds_the_organization_then_its_quota() {
        let (base, seen) = serve(|req| {
            if req.starts_with("GET /api/organizations ") {
                (
                    200,
                    r#"[{"org_id":"org_x","name":"Other"},{"org_id":"org_main","display_name":"Main","is_primary_org":true}]"#.into(),
                )
            } else if req.starts_with("GET /api/org-main/billing/quota/usage ") {
                (
                    200,
                    r#"{"daily_percentage":12,"weekly_percentage":"54","hide_daily_quota":false,"overage_balance_cents":350}"#.into(),
                )
            } else {
                (404, "{}".into())
            }
        });
        let (q, org) = fetch(&agent(), &base, &test_token(), None).unwrap();
        assert_eq!(
            org,
            OrgRef {
                id: "org-main".into(),
                name: Some("Main".into())
            }
        );
        assert_eq!(q.daily.unwrap().percent, 12.0);
        assert_eq!(q.weekly.unwrap().percent, 54.0);
        assert_eq!(q.overage_usd, Some(3.5));
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        let ua = format!("mcc/{}", env!("CARGO_PKG_VERSION"));
        for req in &reqs {
            assert_eq!(header(req, "authorization"), Some("Bearer test-token"));
            assert_eq!(header(req, "accept"), Some("application/json"));
            assert_eq!(header(req, "user-agent"), Some(ua.as_str()));
        }
        assert_eq!(header(&reqs[0], "x-cog-org-id"), None);
        assert_eq!(header(&reqs[1], "x-cog-org-id"), Some("org-main"));

        // A known organization skips the discovery.
        let known = OrgRef {
            id: "org-main".into(),
            name: None,
        };
        fetch(&agent(), &base, &test_token(), Some(known)).unwrap();
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 3);
        assert!(reqs[2].starts_with("GET /api/org-main/billing/quota/usage "));
    }

    #[test]
    fn fetch_maps_statuses_like_custats() {
        let cases: [(u16, &str, Reason); 7] = [
            (401, r#"{"detail":"Unauthorized"}"#, Reason::Unauthorized),
            (403, "", Reason::Unauthorized),
            (
                403,
                r#"{"detail":"No organizations found for auth1 user"}"#,
                Reason::NoOrganizations,
            ),
            (
                401,
                r#"{"detail":"No organizations found for auth1 user"}"#,
                Reason::NoOrganizations,
            ),
            (429, "{}", Reason::HttpError),
            (500, "{}", Reason::HttpError),
            (302, "", Reason::HttpError),
        ];
        for (status, body, want) in cases {
            // On the organizations request…
            let b = body.to_string();
            let (base, _) = serve(move |_| (status, b.clone()));
            assert_eq!(
                fetch(&agent(), &base, &test_token(), None).unwrap_err(),
                want,
                "{status} {body}"
            );
            // …and on the quota request.
            let b = body.to_string();
            let (base, _) = serve(move |req| {
                if req.starts_with("GET /api/organizations ") {
                    (200, r#"[{"org_id":"org-a"}]"#.into())
                } else {
                    (status, b.clone())
                }
            });
            assert_eq!(
                fetch(&agent(), &base, &test_token(), None).unwrap_err(),
                want,
                "quota {status} {body}"
            );
        }
        let (base, _) = serve(|_| (200, "[]".into()));
        assert_eq!(
            fetch(&agent(), &base, &test_token(), None).unwrap_err(),
            Reason::NoOrganizations
        );
        let (base, _) = serve(|_| (200, "{}".into()));
        assert_eq!(
            fetch(&agent(), &base, &test_token(), None).unwrap_err(),
            Reason::ParseError
        );
        let (base, _) = serve(|req| {
            if req.starts_with("GET /api/organizations ") {
                (200, r#"[{"org_id":"org-a"}]"#.into())
            } else {
                (200, "not json".into())
            }
        });
        assert_eq!(
            fetch(&agent(), &base, &test_token(), None).unwrap_err(),
            Reason::ParseError
        );
        // Nothing listening.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert_eq!(
            fetch(
                &agent(),
                &format!("http://127.0.0.1:{port}/api"),
                &test_token(),
                None
            )
            .unwrap_err(),
            Reason::Network
        );
    }

    #[test]
    fn fetch_never_follows_a_redirect() {
        let (elsewhere, elsewhere_seen) = serve(|_| (200, "[]".into()));
        let target = format!("{elsewhere}/organizations");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line == "\r\n" => break,
                        Ok(_) => {}
                    }
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.flush();
            }
        });
        assert_eq!(
            fetch(&agent(), &base, &test_token(), None).unwrap_err(),
            Reason::HttpError
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(elsewhere_seen.lock().unwrap().is_empty());
    }

    #[test]
    fn describe_points_at_devin_auth_login() {
        assert!(describe(Reason::Unauthorized).contains("devin auth login"));
        assert!(describe(Reason::NotLoggedIn).contains("devin auth login"));
        assert!(!describe(Reason::NotLookedUp).contains("Claude"));
        assert_eq!(dollars(3.5), "$3.50");
    }
}
