//! `settings.json` synced key by key (#129). Each key of a profile's
//! `settings.json` (top-level, with `env` split into one key per variable,
//! `env.NAME`) falls in one [`Category`]:
//!
//! - **basic**: no secret; a change in any profile reaches every profile;
//! - **shared secret**: an API key or token not tied to an account; `mcc
//!   sync` asks before copying it, from a profile into the default or from
//!   the default into a profile, and never shows its value (auto sync
//!   leaves it to `mcc sync`);
//! - **account-bound**: a credential or provider of one account (the
//!   variables mcc strips from children, `apiKeyHelper`, …); never copied.
//!
//! `copy::plan_with` plans each profile's `settings.json` from the default
//! with these keys (never the default's account-bound ones); [`decide`]
//! picks what `mcc sync` takes from the profiles into the default first.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::config::SyncConfig;
use crate::manifest;

/// The prefix of a flattened `env` variable's key: `env.NAME`.
pub const ENV_PREFIX: &str = "env.";

/// `settings.json` keys tied to one account, besides the variables in
/// [`crate::refresh::STRIPPED_ENV`] (as `env.NAME`): the login helpers,
/// and the credentials and endpoints of the Bedrock, Vertex and Foundry
/// providers (a plain `AWS_REGION` is not one).
const ACCOUNT_KEYS: &[&str] = &[
    "apiKeyHelper",
    "awsAuthRefresh",
    "awsCredentialExport",
    "forceLoginMethod",
    "forceLoginOrgUUID",
    "env.AWS_BEARER_TOKEN_BEDROCK",
    "env.AWS_ACCESS_KEY_ID",
    "env.AWS_SECRET_ACCESS_KEY",
    "env.AWS_SESSION_TOKEN",
    "env.AWS_PROFILE",
    "env.ANTHROPIC_BEDROCK_BASE_URL",
    "env.ANTHROPIC_VERTEX_BASE_URL",
    "env.ANTHROPIC_VERTEX_PROJECT_ID",
    "env.CLOUD_ML_REGION",
    "env.GOOGLE_APPLICATION_CREDENTIALS",
    "env.ANTHROPIC_FOUNDRY_API_KEY",
    "env.ANTHROPIC_FOUNDRY_BASE_URL",
    "env.ANTHROPIC_FOUNDRY_RESOURCE",
    "env.CLAUDE_CODE_USE_FOUNDRY",
];

/// An `env` variable named like a secret: `PASSWORD`, `PASSWD`,
/// `PASSPHRASE`, `SECRET`, `CREDENTIAL`, `PRIVATE_KEY` or `WEBHOOK`
/// anywhere in it (`PGPASSWORD`); one of the shorter words as a whole
/// `_`-separated part, `HEADERS` among them (`OTEL_EXPORTER_OTLP_HEADERS`,
/// `ANTHROPIC_CUSTOM_HEADERS` carry auth headers), so `MAX_THINKING_TOKENS`,
/// `BYPASS_PROXY` and `OLDPWD` are not one;
/// or `KEY` as its last part (`SERPAPI_KEY`).
static SECRET_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)PASSWORD|PASSWD|PASSPHRASE|SECRET|CREDENTIAL|PRIVATE_?KEY|WEBHOOK|(?:^|_)(?:API_?KEY|TOKEN|ACCESS_KEY|AUTH|PASS|PWD|DSN|HEADERS)(?:_|$)|(?:^|_)KEY$",
    )
    .expect("static regex")
});

/// A URL, as it stands in a command line or a value.
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s'"`<>]+"#).expect("static regex"));

/// A URL that carries a secret whatever its length ([`URL`] matches are
/// checked from their start): a webhook (Slack, Discord, Teams, Telegram,
/// Zapier, IFTTT, or `webhook`, `/hooks/`, `/hook/` in it), a Sentry DSN
/// (its key as the user), or a query parameter named like a key, token or
/// password.
static SECRET_URL: LazyLock<Regex> = LazyLock::new(|| {
    let auth = r"(?:[^/?#@]*@)?";
    let hosts = [
        r"hooks\.slack\.com/(?:services|workflows)/",
        r"(?:[a-z0-9-]+\.)?discord(?:app)?\.com/api/webhooks/",
        r"outlook\.office\.com/webhook",
        r"[a-z0-9.-]*\.webhook\.office\.com",
        r"api\.telegram\.org/bot",
        r"hooks\.zapier\.com",
        r"maker\.ifttt\.com/trigger/[^?#]*/with/key/",
    ]
    .join("|");
    let rules = [
        format!("{auth}(?:{hosts})"),
        r"[^/?#]*@[a-z0-9.-]*\.ingest\.(?:[a-z]+\.)?sentry\.io".to_string(),
        r"[^?#]*webhook".to_string(),
        r"[^/?#]*(?:/[^?#]*)?/hooks?/".to_string(),
        // A parameter whose last `_`/`-` part names a credential
        // (`token`, `api-key`, `client_secret`), not `keyword` or `passage`.
        r"[^?#]*\?(?:[^#]*[&;])?(?:[a-z0-9]+[_-])*(?:token|key|apikey|secret|sig|signature|auth|password|passwd|pass|pwd|credentials?)=".to_string(),
    ]
    .join("|");
    Regex::new(&format!(r"(?i)^[a-z][a-z0-9+.-]*://(?:{rules})")).expect("static regex")
});

/// A URL with a password in it, before its path: `scheme://user:pass@host`,
/// or with no user name (`redis://:pass@host`).
static CREDENTIAL_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z][A-Za-z0-9+.-]*://[^/\s:@]*:[^/\s@]+@").expect("static regex")
});

/// An HTTP credential in a command line: an `Authorization:` header with
/// its scheme and credential, a header named like a key or token
/// (`X-API-Key:`, `DD-API-KEY:`, `X-Auth-Token:`, `PRIVATE-TOKEN:`, or
/// after `-H`/`--header` any name such as `apikey:`) with a value of 8+
/// credential characters (so neither `print-access-token:*)` in a
/// permission rule nor `cache-key: %s` is one; `-H` is case-sensitive, so
/// `du -h` isn't either), or `Bearer` and 16+ characters anywhere. Each
/// group is the
/// credential: one that only names a shell variable (`$TOKEN`) is not in
/// the file ([`holds_auth_header`]).
static AUTH_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\bauthorization\s*:\s*(?:bearer|basic|token|bot)\s+([^\s'"]+)|(?:^|[\s'"])(?:[a-z0-9]+-)+(?:api-?key|key|token|secret|auth)\s*:\s*([A-Za-z0-9._~+/=-]{8,})(?:[\s'"]|$)|(?:^|\s)(?-i:-H|--header)[\s=]*['"]?[a-z0-9_-]*(?:key|token|secret|auth)\s*:\s*([A-Za-z0-9._~+/=-]{8,})(?:[\s'"]|$)|\bbearer\s+([$A-Za-z0-9._~+/=-]{16,})"#,
    )
    .expect("static regex")
});

/// A password given to curl: `-u user:pass`, `-uuser:pass`, `--user
/// user:pass`, `--user=user:pass`; the groups are the user and password.
static CURL_USER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|\s)(?:-u\s*|--user[=\s]+)['"]?([^\s:'"]+):([^\s'"]+)"#)
        .expect("static regex")
});

/// A shell assignment in a command line: `NAME=value`, maybe after
/// `export`; the groups are the name and the value (maybe quoted).
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:^|[\s;&|(])(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)=([^\s'";&|]+|'[^']*'|"[^"]*")"#,
    )
    .expect("static regex")
});

/// A run of 32 or more hex digits standing alone: in an `env` value, an
/// API key more likely than a hash (SerpAPI keys are 64 lowercase hex).
static HEX_RUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^A-Za-z0-9])[0-9A-Fa-f]{32,}(?:$|[^A-Za-z0-9])").expect("static regex")
});

/// Text shaped like a well-known token, wherever it is.
static TOKEN_SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"sk-ant-[A-Za-z0-9_-]{8,}|\bsk-[A-Za-z0-9_-]{20,}|\bghp_[A-Za-z0-9]{20,}|\bgithub_pat_[A-Za-z0-9_]{20,}|\bxox[bp]-[A-Za-z0-9-]{10,}|\bAKIA[0-9A-Z]{16}\b",
    )
    .expect("static regex")
});

/// A long run that may be a random token: checked for mixed case and
/// digits by [`holds_token`]. `/` and `.` are left out, so paths are not,
/// and a run that is a filesystem path's segment (next to a `/` or `\`,
/// such as Claude Code's `-Users-me-src-project2`) is never one; a URL's
/// segment always may be (a webhook's token).
static LONG_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+=-]{32,}").expect("static regex"));

/// `settings.json` as flat keys: each top-level key, except an `env`
/// object, whose variables are keys of their own (`env.NAME`).
pub fn flatten(obj: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in obj {
        match (k.as_str(), v) {
            ("env", Value::Object(vars)) => {
                for (name, val) in vars {
                    out.insert(format!("{ENV_PREFIX}{name}"), val.clone());
                }
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

/// Set (`Some`) or remove (`None`) the flat key `key` in the settings
/// object `obj`, keeping the order of the keys already there.
pub fn set_flat(obj: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    let Some(name) = key.strip_prefix(ENV_PREFIX) else {
        match value {
            Some(v) => {
                obj.insert(key.to_string(), v);
            }
            None => {
                obj.shift_remove(key);
            }
        }
        return;
    };
    match value {
        Some(v) => {
            if !matches!(obj.get("env"), Some(Value::Object(_))) {
                obj.insert("env".into(), Value::Object(Map::new()));
            }
            if let Some(Value::Object(env)) = obj.get_mut("env") {
                env.insert(name.to_string(), v);
            }
        }
        None => {
            if let Some(Value::Object(env)) = obj.get_mut("env") {
                env.shift_remove(name);
            }
        }
    }
}

/// The settings object of the flat keys `flat`, in their order.
pub fn unflatten(flat: &Map<String, Value>) -> Map<String, Value> {
    let mut obj = Map::new();
    for (k, v) in flat {
        set_flat(&mut obj, k, Some(v.clone()));
    }
    obj
}

/// The hash a key's value is recorded with ([`manifest::Manifest::settings`]).
pub fn hash(v: Option<&Value>) -> Option<String> {
    v.map(manifest::hash_json)
}

/// What a `settings.json` key holds, for sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    /// No secret: synced to every profile.
    Basic,
    /// A secret not tied to an account: synced only once the user agrees.
    SharedSecret,
    /// A credential or provider of one account: never synced.
    AccountBound,
}

impl Category {
    /// Its name in sync's report: "basic", "shared secret", "account-bound".
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Basic => "basic",
            Category::SharedSecret => "shared secret",
            Category::AccountBound => "account-bound",
        }
    }
}

/// Whether a string in `v` (at any depth) looks like a token: a known
/// token prefix, a URL with a password, an HTTP credential, a webhook's or another URL that
/// carries a key ([`SECRET_URL`]), or a long run of mixed-case letters
/// and digits that isn't a filesystem path's segment.
fn holds_token(v: &Value) -> bool {
    match v {
        Value::String(s) => {
            TOKEN_SHAPE.is_match(s)
                || CREDENTIAL_URL.is_match(s)
                || holds_auth_header(s)
                || holds_curl_password(s)
                || URL.find_iter(s).any(|u| SECRET_URL.is_match(u.as_str()))
                || LONG_RUN.find_iter(s).any(|m| {
                    let run = m.as_str();
                    !in_path(s, m.start(), m.end())
                        && run.bytes().any(|b| b.is_ascii_uppercase())
                        && run.bytes().any(|b| b.is_ascii_lowercase())
                        && run.bytes().any(|b| b.is_ascii_digit())
                })
        }
        Value::Array(a) => a.iter().any(holds_token),
        Value::Object(m) => m.values().any(holds_token),
        _ => false,
    }
}

/// Whether `s` holds an HTTP credential ([`AUTH_HEADER`]) other than a
/// shell variable's name (`Bearer $TOKEN`, `X-API-Key: ${API_KEY}`).
fn holds_auth_header(s: &str) -> bool {
    AUTH_HEADER.captures_iter(s).any(|c| {
        c.iter()
            .skip(1)
            .flatten()
            .any(|m| !m.as_str().starts_with('$'))
    })
}

/// Whether `s` gives curl a password ([`CURL_USER`]) other than a shell
/// variable's, with a user that isn't a numeric id. The match must belong
/// to the last `curl` before it: no `;`, `|`, `&`, newline or `$(` between
/// them, so `curl …; date -u +%H:%M` or `docker --user node:pw` is not
/// one. A user starting with `+` or `%` is a `date` format, never a user.
/// A separator inside a curl argument (a quoted URL with `&`, `-d "$(…)"`
/// before `-u`) also ends the match, so such a password is missed;
/// `[sync] secret_keys` marks what detection misses.
fn holds_curl_password(s: &str) -> bool {
    CURL_USER.captures_iter(s).any(|c| {
        let (user, pass) = (&c[1], &c[2]);
        let before = &s[..c.get(0).map_or(0, |m| m.start())];
        let in_curl = before.rfind("curl").is_some_and(|i| {
            let between = &before[i..];
            !between.contains([';', '|', '&', '\n']) && !between.contains("$(")
        });
        in_curl
            && !user.starts_with(['+', '%'])
            && !pass.starts_with(['$', '`'])
            && !user.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Whether `s[start..end]` is a segment of a filesystem path: next to a
/// `/` or `\`, in a word (as a shell splits a command line) that is not
/// a URL (no `://` before it).
fn in_path(s: &str, start: usize, end: usize) -> bool {
    let sep = s[..start].ends_with(['/', '\\']) || s[end..].starts_with(['/', '\\']);
    // Past the boundary char, by its UTF-8 length (NBSP is 2 bytes).
    let word = s[..start]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace() || matches!(c, '\'' | '"' | '`'))
        .map_or(0, |(i, c)| i + c.len_utf8());
    sep && !s[word..start].contains("://")
}

/// Whether the `env` value `v` holds a long run of hex digits ([`HEX_RUN`]).
fn holds_hex(v: &Value) -> bool {
    matches!(v, Value::String(s) if HEX_RUN.is_match(s))
}

/// Sorts `settings.json` keys into [`Category`]s: the built-in rules plus
/// `[sync] account_keys` and `secret_keys`.
#[derive(Debug, Clone, Default)]
pub struct Classifier {
    account: Vec<String>,
    secret: Vec<String>,
}

impl Classifier {
    pub fn new(cfg: &SyncConfig) -> Self {
        Self {
            account: cfg.account_keys.clone(),
            secret: cfg.secret_keys.clone(),
        }
    }

    /// Whether `key` is tied to one account, whatever its value.
    pub fn is_account_bound(&self, key: &str) -> bool {
        key.strip_prefix(ENV_PREFIX)
            .is_some_and(|name| crate::refresh::STRIPPED_ENV.contains(&name))
            || ACCOUNT_KEYS.contains(&key)
            || self.account.iter().any(|k| k == key)
    }

    /// The category of `key` holding `value`. Account-bound wins over a
    /// shared secret. A long hex run is a secret only in an `env` value:
    /// elsewhere it is more likely a commit or a hash.
    pub fn category(&self, key: &str, value: Option<&Value>) -> Category {
        let env_name = key.strip_prefix(ENV_PREFIX);
        if self.is_account_bound(key) {
            Category::AccountBound
        } else if self.secret.iter().any(|k| k == key)
            || env_name.is_some_and(|name| SECRET_NAME.is_match(name))
            || value.is_some_and(holds_token)
            || (env_name.is_none() && value.is_some_and(|v| self.holds_secret_assignment(v)))
            || (env_name.is_some() && value.is_some_and(holds_hex))
        {
            Category::SharedSecret
        } else {
            Category::Basic
        }
    }

    /// Whether a string in `v` (at any depth) assigns a value to a shell
    /// variable named like a secret, or like an account-bound `env`
    /// variable (`PGPASSWORD=… psql`, `export GITHUB_TOKEN=…`): a value
    /// taken from a variable or a command (`API_TOKEN=$API_TOKEN`,
    /// `TOKEN=$(cat ~/.token)`) isn't one.
    fn holds_secret_assignment(&self, v: &Value) -> bool {
        match v {
            Value::String(s) => ASSIGNMENT.captures_iter(s).any(|c| {
                let name = &c[1];
                let value = c[2].trim_matches(['\'', '"']);
                (SECRET_NAME.is_match(name)
                    || self.is_account_bound(&format!("{ENV_PREFIX}{name}")))
                    && !value.is_empty()
                    // `$X`, `${X}`, `$(cmd)`, `$1` or a backtick
                    // substitution: the secret is not in the file.
                    && !value.starts_with(['$', '`'])
            }),
            Value::Array(a) => a.iter().any(|v| self.holds_secret_assignment(v)),
            Value::Object(m) => m.values().any(|v| self.holds_secret_assignment(v)),
            _ => false,
        }
    }

    /// The key `key` holding `value`, to be copied into other profiles;
    /// `None` when it is account-bound, or bare `"env"` (a non-object
    /// `env` that would replace an `env` object and its account-bound
    /// variables), which is never copied.
    pub fn shareable(&self, key: &str, value: Value) -> Option<Shareable> {
        (key != "env" && !self.is_account_bound(key)).then(|| Shareable {
            key: key.to_string(),
            value,
        })
    }

    /// The key `key`, to be removed from other profiles; `None` when it is
    /// account-bound or bare `"env"`, which sync never removes.
    pub fn removable(&self, key: &str) -> Option<SharedKey> {
        (key != "env" && !self.is_account_bound(key)).then(|| SharedKey(key.to_string()))
    }

    /// The flat settings `flat` without their account-bound keys or a bare
    /// non-object `"env"`: what may reach another profile.
    pub fn shared(&self, flat: Map<String, Value>) -> Shared {
        Shared(
            flat.into_iter()
                .filter(|(k, _)| k != "env" && !self.is_account_bound(k))
                .collect(),
        )
    }

    /// `flat` with the value of every secret key (shared or account-bound,
    /// here or in `other`) replaced by a placeholder: `<hidden>`, or with
    /// `mark` and a value `other` has otherwise, `<hidden, differs>`. So
    /// two sides tell only whether a secret differs.
    pub fn masked(
        &self,
        flat: &Map<String, Value>,
        other: &Map<String, Value>,
        mark: bool,
    ) -> Map<String, Value> {
        flat.iter()
            .map(|(k, v)| {
                let secret = self.category(k, Some(v)) != Category::Basic
                    || self.category(k, other.get(k)) != Category::Basic;
                let v = if !secret {
                    v.clone()
                } else if mark && other.get(k) != Some(v) {
                    Value::String("<hidden, differs>".into())
                } else {
                    Value::String("<hidden>".into())
                };
                (k.clone(), v)
            })
            .collect()
    }
}

/// A key and value that may be copied to other profiles: never
/// account-bound, as only [`Classifier::shareable`] makes one.
#[derive(Debug, Clone, PartialEq)]
pub struct Shareable {
    key: String,
    value: Value,
}

/// A key that may be removed from other profiles: never account-bound,
/// as only [`Classifier::removable`] makes one.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedKey(String);

/// Flat settings without account-bound keys, as only
/// [`Classifier::shared`] makes them: the default's side of a profile's
/// settings.
#[derive(Debug, Clone, Default)]
pub struct Shared(Map<String, Value>);

impl Shared {
    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }
}

/// Apply what [`decide`] took from the profiles to the default's
/// `settings.json` object `obj`: set each of `set`, remove each of `remove`.
/// Account-bound keys can't be among them.
pub fn apply_to_default(obj: &mut Map<String, Value>, set: &[Shareable], remove: &[SharedKey]) {
    for s in set {
        set_flat(obj, &s.key, Some(s.value.clone()));
    }
    for k in remove {
        set_flat(obj, &k.0, None);
    }
}

/// What a profile's sync record says about its `settings.json` keys.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyBase {
    /// No usable record: compared two-way.
    TwoWay,
    /// Each key's hash as mcc last wrote or found it.
    Keys(BTreeMap<String, String>),
    /// Only the whole file's hash, from before per-key records, and the
    /// profile has edited the file since: which side changed a key is
    /// guessed ([`key_state`]). `default_unchanged`: the default still
    /// gives what mcc wrote, so every key that differs was changed in the
    /// profile.
    Guess { default_unchanged: bool },
}

/// The base recorded for a key only the profile has, from a guess
/// ([`KeyState::Proposed`]): it stays a proposal, taken into the default
/// only once the user confirms it, until both sides agree on it.
pub const PROPOSED_BASE: &str = "proposed";

/// The prefix of the base recorded for a key a profile lacks while it
/// can't be told whether it removed or lost it ([`KeyState::Lost`]),
/// followed by the hash of the default's value then: never a removal.
pub const LOST_PREFIX: &str = "lost:";

/// The mark a declined removal of `key` is remembered by in
/// `declined_secrets`, for the default's value `default` (so a new value
/// there is asked about again).
pub fn declined_removal(default: Option<&Value>) -> String {
    format!("absent:{}", hash(default).unwrap_or_default())
}

/// The mark a shared secret the default sets, declined for a profile
/// (`mcc sync` asks before it reaches one), is remembered by in that
/// profile's `declined_secrets`, for the default's value `default` (so a
/// new value there is asked about again).
pub fn declined_from_default(default: Option<&Value>) -> String {
    format!("default:{}", hash(default).unwrap_or_default())
}

/// Where a key differs between the default's side (`w`) and the
/// profile's (`p`), who changed it since the record: what both
/// [`decide`] (taking profile changes into the default) and the profile's
/// plan (`copy::plan_with`) act on, so the two never disagree.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyState {
    /// Only the default changed it (or no record says otherwise): the
    /// profile takes the default's.
    Theirs,
    /// Only the profile changed it, the default is as recorded: the
    /// profile keeps it (recording `base` when the record has none) and
    /// [`decide`] takes it into the default.
    Ours { base: Option<String> },
    /// The profile lacks it, but whether it removed or lost it can't be
    /// told (a guess): the profile keeps it as it is (recording `base`, a
    /// [`LOST_PREFIX`] mark); never taken into the default, never removed
    /// from it.
    Lost { base: Option<String> },
    /// No record: the profile's own key, kept; never taken.
    Untracked,
    /// Only the profile has it and the default changed since a whole-file
    /// record: added in the profile, or dropped by the default? The
    /// profile keeps it ([`PROPOSED_BASE`]) and the user is asked whether
    /// the default should take it.
    Proposed,
    /// Changed on both sides: a conflict (`--force` gives the profile the
    /// default's side).
    Both,
}

/// The [`KeyState`] of `key`, which the default's side has as `w` and the
/// profile as `p` (they differ), against the record `base`.
pub fn key_state(base: &KeyBase, key: &str, w: Option<&Value>, p: Option<&Value>) -> KeyState {
    match base {
        KeyBase::TwoWay if w.is_none() => KeyState::Untracked,
        KeyBase::TwoWay => KeyState::Theirs,
        KeyBase::Keys(b) => {
            let last = b.get(key).map(String::as_str);
            if last == Some(PROPOSED_BASE) {
                match (w, p) {
                    (None, _) => KeyState::Proposed,
                    (Some(_), None) => KeyState::Theirs,
                    (Some(_), Some(_)) => KeyState::Both,
                }
            } else if let Some(real) = last.and_then(|l| l.strip_prefix(LOST_PREFIX)) {
                // Lost: kept so while the default still has its value then;
                // a new one there reaches the profile; a value the profile
                // adds follows the rules, against that value.
                match p {
                    None if hash(w).as_deref() == Some(real) => KeyState::Lost {
                        base: last.map(String::from),
                    },
                    None => KeyState::Theirs,
                    Some(_) => by_last(Some(real), w, p),
                }
            } else {
                by_last(last, w, p)
            }
        }
        // The default is as mcc wrote it: the profile changed the key. A
        // key it lacks may have been lost rather than removed.
        KeyBase::Guess {
            default_unchanged: true,
        } => {
            if p.is_some() {
                KeyState::Ours { base: hash(w) }
            } else {
                KeyState::Lost {
                    base: hash(w).map(|h| format!("{LOST_PREFIX}{h}")),
                }
            }
        }
        KeyBase::Guess {
            default_unchanged: false,
        } => match (w, p) {
            (_, None) => KeyState::Theirs,
            (None, Some(_)) => KeyState::Proposed,
            (Some(_), Some(_)) => KeyState::Both,
        },
    }
}

/// [`key_state`] against a key's last recorded hash `last`.
fn by_last(last: Option<&str>, w: Option<&Value>, p: Option<&Value>) -> KeyState {
    if hash(p).as_deref() == last {
        KeyState::Theirs
    } else if hash(w).as_deref() == last {
        KeyState::Ours { base: None }
    } else {
        KeyState::Both
    }
}

/// For a profile read by guess ([`KeyBase::Guess`]), the per-key record
/// that reads every key the same way against the same default: what
/// `mcc sync` saves before it changes the default, so the guess (which
/// compares with the default) can't flip for this profile. `None` for a
/// profile with a per-key record, or none.
pub fn per_key_record(view: &SettingsView, c: &Classifier) -> Option<BTreeMap<String, String>> {
    if !matches!(view.base, KeyBase::Guess { .. }) {
        return None;
    }
    let mut out = BTreeMap::new();
    for k in view.keys() {
        let (w, p) = (view.want.get(&k), view.have.get(&k));
        let base = if w == p || view.overlaid(&k) || c.is_account_bound(&k) {
            hash(p)
        } else {
            match key_state(&view.base, &k, w, p) {
                KeyState::Theirs | KeyState::Untracked => hash(p),
                KeyState::Ours { base } | KeyState::Lost { base } => base,
                KeyState::Proposed => Some(PROPOSED_BASE.into()),
                KeyState::Both => None,
            }
        };
        if let Some(b) = base {
            out.insert(k, b);
        }
    }
    Some(out)
}

/// A profile's `settings.json` next to the default's, key by key.
#[derive(Debug, Clone)]
pub struct SettingsView {
    /// The default's settings as the profile receives them: paths
    /// rewritten, account-bound keys left out, its overlay merged on.
    pub want: Map<String, Value>,
    /// The profile's own settings (empty when it has no settings.json).
    pub have: Map<String, Value>,
    /// The profile's settings.json object as it is, `None` when missing.
    pub have_obj: Option<Map<String, Value>>,
    pub base: KeyBase,
    /// Keys the profile's overlay sets or removes: its own.
    pub overlay_keys: BTreeSet<String>,
    /// The overlay replaces `env` whole: every `env.NAME` is its own.
    pub overlay_env: bool,
    /// The profile's settings.json is the default's own file (a symlink
    /// to it, as an older mcc left it): `have` is read through it, without
    /// the default's account-bound keys; it is always replaced by a file,
    /// and is never a source of changes for the default.
    pub linked: bool,
}

impl SettingsView {
    /// Whether the profile's overlay decides `key`.
    pub fn overlaid(&self, key: &str) -> bool {
        self.overlay_keys.contains(key)
            || (self.overlay_env && (key == "env" || key.starts_with(ENV_PREFIX)))
    }

    /// Every key either side has or the record knows, the default's first.
    pub fn keys(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut keys = Vec::new();
        let base = match &self.base {
            KeyBase::Keys(b) => b.keys().cloned().collect(),
            _ => Vec::new(),
        };
        for k in self
            .want
            .keys()
            .chain(self.have.keys())
            .cloned()
            .chain(base)
        {
            if seen.insert(k.clone()) {
                keys.push(k);
            }
        }
        keys
    }

    /// Whether the profile changed `key` ([`key_state`]), and how.
    /// Profiles without a record are never sources; neither is a key the
    /// overlay sets, or one that already matches the default.
    fn changed(&self, key: &str) -> Option<Change> {
        if key == "env" || self.overlaid(key) {
            return None;
        }
        let (w, p) = (self.want.get(key), self.have.get(key));
        if w == p {
            return None;
        }
        match key_state(&self.base, key, w, p) {
            KeyState::Theirs | KeyState::Lost { .. } | KeyState::Untracked => None,
            KeyState::Ours { .. } => Some(Change::Profile),
            KeyState::Proposed => Some(Change::Proposed),
            KeyState::Both => Some(Change::Both),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Change {
    /// Only the profile changed it.
    Profile,
    /// Only the profile has it; it may be one the default dropped.
    Proposed,
    /// Changed on both sides.
    Both,
}

/// One profile, as a source of settings changes for [`decide`].
pub struct Source<'a> {
    pub alias: &'a str,
    pub view: &'a SettingsView,
    /// The profile's value as the default would hold it (its paths mapped
    /// back); `None` when they can't be.
    pub to_default: &'a dyn Fn(&Value) -> Option<Value>,
}

/// What `mcc sync` takes from the profiles for one key.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Set in the default (and from there in every profile).
    Set(Shareable),
    /// Removed from the default (and from there from every profile).
    Remove(SharedKey),
    /// Account-bound: never taken.
    AccountBound,
    /// Only a profile has it, and the default changed since that
    /// profile's whole-file record: it may be new there, or one the
    /// default dropped. Taken only once the user confirms it.
    Propose(Shareable),
    /// Changed differently in several places (`with_default`: the default
    /// among them; `default_has`: the default has a value for it): nothing
    /// is taken.
    Conflict {
        with_default: bool,
        default_has: bool,
    },
    /// The profile's value holds paths that can't be mapped to the
    /// default's: nothing is taken.
    Unmappable,
}

/// A key some profile changed: its category, the profiles it changed in,
/// and what to do with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub key: String,
    pub category: Category,
    pub sources: Vec<String>,
    pub verdict: Verdict,
}

/// Pick, key by key, what the profiles `sources` changed that the
/// default (`default`, flattened, as it is on disk) should take. A key
/// changed in one place since its record is taken; changed differently
/// in two or more (the default included), it is a conflict. Without a
/// per-key record ([`KeyBase::Guess`]) and with the default changed
/// since, a key the profile alone has is a [`Verdict::Propose`], one both
/// have differently a conflict. A removal is returned as
/// [`Verdict::Remove`]; `mcc sync` carries it, or a proposal, into the
/// default only once the user confirms it.
pub fn decide(c: &Classifier, default: &Map<String, Value>, sources: &[Source]) -> Vec<Decision> {
    let mut keys = BTreeSet::new();
    for s in sources {
        keys.extend(s.view.keys());
    }
    let mut out = Vec::new();
    for key in keys {
        // (alias, value for the default, or None to remove; how it's known)
        let mut cands: Vec<(&str, Option<Value>, Change)> = Vec::new();
        let mut unmappable = Vec::new();
        for s in sources {
            let Some(change) = s.view.changed(&key) else {
                continue;
            };
            match s.view.have.get(&key) {
                None => cands.push((s.alias, None, change)),
                Some(v) => match (s.to_default)(v) {
                    Some(v) => cands.push((s.alias, Some(v), change)),
                    None => unmappable.push(s.alias.to_string()),
                },
            }
        }
        if cands.is_empty() && unmappable.is_empty() {
            continue;
        }
        let category = cands
            .iter()
            .map(|(_, v, _)| c.category(&key, v.as_ref()))
            .chain([c.category(&key, default.get(&key))])
            .max()
            .unwrap_or(Category::Basic);
        let aliases = |list: &[(&str, Option<Value>, Change)]| -> Vec<String> {
            list.iter().map(|(a, _, _)| a.to_string()).collect()
        };
        if category == Category::AccountBound {
            let mut all = aliases(&cands);
            all.extend(unmappable);
            out.push(Decision {
                key,
                category,
                sources: all,
                verdict: Verdict::AccountBound,
            });
            continue;
        }
        if !unmappable.is_empty() {
            out.push(Decision {
                key: key.clone(),
                category,
                sources: unmappable,
                verdict: Verdict::Unmappable,
            });
        }
        if cands.is_empty() {
            continue;
        }
        let proposals: BTreeSet<Option<String>> =
            cands.iter().map(|(_, v, _)| hash(v.as_ref())).collect();
        let with_default = cands.iter().any(|(_, _, ch)| *ch == Change::Both);
        let proposed = cands.iter().any(|(_, _, ch)| *ch == Change::Proposed);
        let verdict = if proposals.len() > 1 || with_default {
            Verdict::Conflict {
                with_default,
                default_has: default.contains_key(&key),
            }
        } else {
            match cands[0].1.clone() {
                Some(v) if proposed => {
                    Verdict::Propose(c.shareable(&key, v).expect("not account-bound"))
                }
                Some(v) => Verdict::Set(c.shareable(&key, v).expect("not account-bound")),
                None => Verdict::Remove(c.removable(&key).expect("not account-bound")),
            }
        };
        out.push(Decision {
            sources: aliases(&cands),
            key,
            category,
            verdict,
        });
    }
    out
}

/// What `mcc sync` did with a key some profile changed (its report).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Copied to the default, and from there to every profile.
    Synced,
    /// A shared secret, a proposal or a removal a real sync would ask
    /// about at a terminal (dry run).
    Asked,
    /// A shared secret or a proposal the user agreed to sync.
    Approved,
    /// A shared secret or a proposal the user declined (`earlier`: in a
    /// previous sync).
    Declined { earlier: bool },
    /// Left as it is, for this reason.
    Skipped(&'static str),
    /// Changed differently in several places; left as it is.
    Conflict {
        with_default: bool,
        default_has: bool,
    },
    /// Removed from the default, and from there from every profile.
    Removed,
}

impl Outcome {
    /// What happened, for people; what would happen in a dry run.
    pub fn describe(self, dry_run: bool, force: bool) -> String {
        let would = |done: &str, todo: &str| if dry_run { todo } else { done }.to_string();
        match self {
            Outcome::Synced => would("synced", "would be synced"),
            Outcome::Asked => "would be asked about".into(),
            Outcome::Approved => "asked → approved, synced".into(),
            Outcome::Declined { earlier: false } => {
                "asked → declined (not asked again until its value changes)".into()
            }
            Outcome::Declined { earlier: true } => {
                "declined earlier (not asked again until its value changes)".into()
            }
            Outcome::Skipped(why) => format!("skipped ({why})"),
            Outcome::Conflict {
                with_default: false,
                ..
            } => "conflict: changed differently in several profiles; skipped".into(),
            Outcome::Conflict {
                with_default: true,
                default_has,
            } => format!(
                "conflict: {}; skipped{}",
                if default_has {
                    "changed differently in the default and the profile"
                } else {
                    "changed in the profile, removed from the default"
                },
                match (force, default_has) {
                    (false, _) => "",
                    (true, true) => " (--force keeps the default's value)",
                    (true, false) => " (--force removes it from the profile)",
                }
            ),
            Outcome::Removed => would("removed", "would be removed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("not an object"),
        }
    }

    fn classifier() -> Classifier {
        Classifier::default()
    }

    #[test]
    fn env_is_split_per_variable_and_rebuilt_in_order() {
        let s = obj(json!({"model": "opus", "env": {"B": "2", "A": "1"}, "hooks": {}}));
        let flat = flatten(&s);
        assert_eq!(
            flat.keys().collect::<Vec<_>>(),
            ["model", "env.B", "env.A", "hooks"]
        );
        assert_eq!(unflatten(&flat), s);
        let mut s2 = s.clone();
        set_flat(&mut s2, "env.C", Some(json!("3")));
        set_flat(&mut s2, "env.B", None);
        set_flat(&mut s2, "model", None);
        set_flat(&mut s2, "zeta", Some(json!(true)));
        assert_eq!(
            Value::Object(s2).to_string(),
            r#"{"env":{"A":"1","C":"3"},"hooks":{},"zeta":true}"#
        );
        // Unusual names and values are kept exactly.
        let odd = obj(json!({"env": {"CLÉ_é": "a \"b\" \\ ü"}, "k\"ey": "<x>"}));
        let flat = flatten(&odd);
        assert_eq!(flat["env.CLÉ_é"], "a \"b\" \\ ü");
        assert_eq!(unflatten(&flat), odd);
        // An `env` that is not an object is one key.
        let odd = obj(json!({"env": "x"}));
        assert_eq!(flatten(&odd).keys().collect::<Vec<_>>(), ["env"]);
    }

    #[test]
    fn keys_fall_in_their_category() {
        let c = classifier();
        let basic = |k: &str, v: Value| c.category(k, Some(&v));
        assert_eq!(
            basic("env.CLAUDE_CODE_EXPERIMENTAL_CODE_MODE", json!("1")),
            Category::Basic
        );
        assert_eq!(
            basic("env.MAX_THINKING_TOKENS", json!("8000")),
            Category::Basic
        );
        assert_eq!(basic("model", json!("opus")), Category::Basic);
        for name in [
            "TAVILY_API_KEY",
            "OPENAI_APIKEY",
            "GITHUB_TOKEN",
            "R2_SECRET_ACCESS_KEY",
            "DB_PASSWORD",
            "my_credentials",
            "BASIC_AUTH",
            "SERPAPI_KEY",
            "GEMINI_KEY",
            "linear_key",
            "KEY",
            "PGPASSWORD",
            "MYSQL_ROOT_PASSWD",
            "DB_PASS",
            "PASS",
            "MYSQL_PWD",
            "SENTRY_DSN",
            "DSN",
            "SLACK_WEBHOOK_URL",
            "DISCORDWEBHOOK",
            "CLIENTSECRET",
            "GCS_CREDENTIALS_JSON",
            "SSH_PRIVATE_KEY",
            "PRIVATEKEY_PEM",
        ] {
            assert_eq!(
                basic(&format!("env.{name}"), json!("x")),
                Category::SharedSecret,
                "{name}"
            );
        }
        for key in [
            "env.ANTHROPIC_API_KEY",
            "env.ANTHROPIC_BASE_URL",
            "env.CLAUDE_CODE_OAUTH_TOKEN",
            "apiKeyHelper",
            "forceLoginOrgUUID",
        ] {
            assert_eq!(basic(key, json!("x")), Category::AccountBound, "{key}");
        }
        // A cloud provider's credentials and endpoints belong to one
        // account too (they were shared secrets or basic before): never
        // copied, even when the user would agree. A plain region is basic.
        for name in [
            "AWS_BEARER_TOKEN_BEDROCK",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_PROFILE",
            "ANTHROPIC_BEDROCK_BASE_URL",
            "ANTHROPIC_VERTEX_BASE_URL",
            "ANTHROPIC_VERTEX_PROJECT_ID",
            "CLOUD_ML_REGION",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "ANTHROPIC_FOUNDRY_API_KEY",
            "ANTHROPIC_FOUNDRY_BASE_URL",
            "ANTHROPIC_FOUNDRY_RESOURCE",
            "CLAUDE_CODE_USE_FOUNDRY",
        ] {
            let key = format!("env.{name}");
            assert_eq!(basic(&key, json!("x")), Category::AccountBound, "{key}");
            assert!(c.shareable(&key, json!("x")).is_none(), "{key}");
        }
        assert_eq!(basic("env.AWS_REGION", json!("us-east-1")), Category::Basic);
        // A token-shaped value is a secret whatever its key's name.
        let token = format!(
            "bash run.sh {}",
            ["sk", "ant", "api03-abcdefghijkl"].join("-")
        );
        assert_eq!(
            basic("hooks", json!({"Stop": [token]})),
            Category::SharedSecret
        );
        assert_eq!(
            basic(
                "env.X",
                json!(format!("{}{}", "AbCdEfGh12345678", "IjKlMnOp90123456"))
            ),
            Category::SharedSecret
        );
        // Names with KEY not as their last part, or inside a word, are not;
        // neither is PASS or PWD inside a word.
        for name in [
            "KEYBOARD_LAYOUT",
            "MONKEY",
            "KEY_PATH",
            "MAX_THINKING_TOKENS",
            "CLAUDE_CODE_EXPERIMENTAL_CODE_MODE",
            "BYPASS_PROXY",
            "PASSTHROUGH",
            "OLDPWD",
            "COMPASS_URL",
            "DATABASE_URL",
        ] {
            assert_eq!(
                basic(&format!("env.{name}"), json!("x")),
                Category::Basic,
                "{name}"
            );
        }
        // A long hex run in an env value is a secret (a 64-hex SerpAPI key),
        // whatever the variable's name.
        let hex64 = "0123456789abcdef".repeat(4);
        assert_eq!(basic("env.SEARCH", json!(hex64)), Category::SharedSecret);
        assert_eq!(
            basic("env.SEARCH", json!("0123456789ABCDEF".repeat(2))),
            Category::SharedSecret
        );
        // Shorter, or not in env: not.
        assert_eq!(
            basic("env.SEARCH", json!("0123456789abcdef")),
            Category::Basic
        );
        assert_eq!(basic("pinned", json!(hex64)), Category::Basic);
        // Paths, UUIDs and hex hashes are not.
        assert_eq!(
            basic(
                "statusLine",
                json!({"command": "bash '/Users/someone/.mcc/profiles/work-account-2/hooks/statusline-command-runner.sh'"})
            ),
            Category::Basic
        );
        // A URL with a password is a secret, wherever it is; one without is
        // not.
        for v in [
            "postgres://app:hunter2@db.example.invalid:5432/app",
            "redis://default:s3cr3t@cache.example.invalid",
            "bash -c 'psql postgres://u:pass@host/db'",
        ] {
            assert_eq!(
                basic("env.DATABASE_URL", json!(v)),
                Category::SharedSecret,
                "{v}"
            );
            assert_eq!(
                basic("hooks", json!({"Stop": [v]})),
                Category::SharedSecret,
                "{v}"
            );
        }
        for v in [
            "https://api.example.invalid/v1/items?x=1",
            "postgres://db.example.invalid:5432/app",
            "https://user@example.invalid/repo.git",
            "git@github.com:me/repo.git",
        ] {
            assert_eq!(basic("env.DATABASE_URL", json!(v)), Category::Basic, "{v}");
        }
        // A token in a URL's path is one, mid-path or at its end, whatever
        // the key: a webhook's.
        let hook = format!(
            "{}{}{}{}",
            "AbCdEfGh12345678", "IjKlMnOp90123456", "QrStUvWx", "Yz7890AbCdEfGhIjKlMnOpQr"
        );
        assert_eq!(hook.len(), 64);
        for v in [
            format!("curl -X POST https://discord.example.invalid/api/webhooks/123456/{hook}"),
            format!("https://discord.example.invalid/api/webhooks/123456/{hook}"),
            format!("https://example.invalid/hook/{hook}/send"),
            format!("curl -d x 'https://example.invalid/hook/{hook}/send?a=1'"),
            format!("--url=https://example.invalid/{hook}"),
            format!("https://example.invalid/hook?team=a1/{hook}"),
            format!("curl \"https://example.invalid/x?(a=b)/{hook}\""),
        ] {
            assert_eq!(
                basic(
                    "hooks",
                    json!({"Stop": [{"hooks": [{"type": "command", "command": v}]}]})
                ),
                Category::SharedSecret,
                "{v}"
            );
            assert_eq!(
                basic("env.NOTIFY_URL", json!(v)),
                Category::SharedSecret,
                "{v}"
            );
        }
        // A webhook's or another URL that carries a key is one, however
        // short its token (built at run time: no webhook-shaped literal).
        let tok = "AbCdEfGh".repeat(3);
        assert_eq!(tok.len(), 24);
        let slack = format!(
            "https://hooks.{}.com/services/T0PROBE01/B0PROBE01/{tok}",
            "slack"
        );
        for v in [
            format!("curl -s -X POST -d hi {slack}"),
            slack.clone(),
            format!("https://hooks.{}.com/workflows/T0PROBE01/{tok}", "slack"),
            format!("https://{}.com/api/webhooks/123/{tok}", "discord"),
            format!("https://{}.com/api/webhooks/123/{tok}", "discordapp"),
            format!(
                "https://outlook.office.com/{}/abc@def/IncomingWebhook/x/y",
                "webhook"
            ),
            format!("https://acme.{}.office.com/webhookb2/abc", "webhook"),
            format!("https://api.{}.org/bot123:{tok}/sendMessage", "telegram"),
            format!("https://hooks.{}.com/hooks/catch/1/abc/", "zapier"),
            format!("https://maker.{}.com/trigger/ev/with/key/{tok}", "ifttt"),
            format!("https://{tok}@o1.ingest.{}.io/42", "sentry"),
            format!("https://ci.example.invalid/hook/{tok}"),
            format!("https://ci.example.invalid/api/hooks/{tok}"),
            "https://ci.example.invalid/notify-webhook/abc".to_string(),
            format!("https://api.example.invalid/v1/send?chat=1&token={tok}"),
            format!("https://api.example.invalid/v1/send?API_KEY={tok}"),
            format!("https://api.example.invalid/x?sig={tok}"),
        ] {
            assert_eq!(
                basic(
                    "hooks",
                    json!({"Stop": [{"hooks": [{"type": "command", "command": v}]}]})
                ),
                Category::SharedSecret,
                "{v}"
            );
            assert_eq!(
                basic("env.NOTIFY_URL", json!(v)),
                Category::SharedSecret,
                "{v}"
            );
            assert_eq!(
                basic("statusLine", json!({"url": v})),
                Category::SharedSecret,
                "{v}"
            );
        }
        for v in [
            "https://example.com/docs",
            "https://api.anthropic.com",
            "https://docs.example.invalid/hooks",
            "https://api.example.invalid/v1/items?page=2&monkey=1",
            "git@github.com:me/repo.git",
            "bash /Users/x/.claude/projects/-Users-montimage-buildspace-luongnv89-mcc/run.sh",
        ] {
            assert_eq!(basic("env.NOTIFY_URL", json!(v)), Category::Basic, "{v}");
            assert_eq!(
                basic(
                    "hooks",
                    json!({"Stop": [{"hooks": [{"type": "command", "command": v}]}]})
                ),
                Category::Basic,
                "{v}"
            );
        }
        // Credentials in a query parameter, however short: any parameter
        // whose last `_`/`-` part names one.
        let short = "Ab3".repeat(7);
        let hex = "0123456789abcdef".repeat(4);
        for q in [
            format!("api-key={short}"),
            format!("access_key={hex}"),
            format!("private_token={short}"),
            format!("auth_token={short}"),
            format!("client_secret={short}"),
            "password=hunter2hunter2".to_string(),
            format!("page=2&x-api-key={short}"),
        ] {
            let url = format!("https://api.example.invalid/v1/send?{q}");
            assert_eq!(
                basic("env.NOTIFY_URL", json!(url)),
                Category::SharedSecret,
                "{url}"
            );
            let cmd = json!({"Stop": [{"hooks": [{"type": "command", "command": format!("curl '{url}'")}]}]});
            assert_eq!(basic("hooks", cmd), Category::SharedSecret, "{url}");
        }
        for q in [
            "page=2&monkey=1",
            "keyword=rust",
            "passage=3",
            "q=key",
            "sort=asc",
        ] {
            let url = format!("https://api.example.invalid/v1/items?{q}");
            assert_eq!(
                basic("env.NOTIFY_URL", json!(url)),
                Category::Basic,
                "{url}"
            );
        }
        // A password with no user name; a port is not one.
        for v in [
            "redis://:hunter2pass@cache.example.invalid:6379/0",
            "rediss://:pw@cache.example.invalid:6380/0",
        ] {
            assert_eq!(
                basic("env.REDIS_URL", json!(v)),
                Category::SharedSecret,
                "{v}"
            );
            assert_eq!(
                basic("statusLine", json!({"command": format!("x {v}")})),
                Category::SharedSecret,
                "{v}"
            );
        }
        for v in [
            "https://host.example.invalid:8080/path",
            "redis://cache.example.invalid:6379/0",
        ] {
            assert_eq!(basic("env.REDIS_URL", json!(v)), Category::Basic, "{v}");
        }
        // An HTTP credential in a hook command, hex or not; a shell
        // variable's name is not one (the secret isn't in the file).
        let hex32 = "0123456789abcdef".repeat(2);
        for cmd in [
            format!("curl -H 'DD-API-KEY: {hex32}' https://api.example.invalid"),
            format!("curl -H 'Authorization: Bearer {hex32}' https://api.example.invalid"),
            format!("curl -H \"Authorization: Basic {short}\" https://x.example.invalid"),
            format!("curl -H 'X-API-Key: {short}' https://x.example.invalid"),
            format!("curl --header 'X-Auth-Token: {short}' https://x.example.invalid"),
            format!("curl -H 'PRIVATE-TOKEN: {short}' https://x.example.invalid"),
            format!("notify --auth Bearer {hex32}"),
            format!("curl -H 'apikey: {hex32}' https://x.example.invalid"),
            format!("curl -H 'Token: {hex32}' https://x.example.invalid"),
            format!("curl -H 'X_API_KEY: {hex32}' https://x.example.invalid"),
            format!("curl --header=\"x-secret:{hex32}\" https://x.example.invalid"),
        ] {
            let v = json!({"Stop": [{"hooks": [{"type": "command", "command": cmd}]}]});
            assert_eq!(basic("hooks", v), Category::SharedSecret, "{cmd}");
            assert_eq!(
                basic("statusLine", json!({"command": cmd})),
                Category::SharedSecret,
                "{cmd}"
            );
            assert_eq!(
                basic("env.NOTIFY_CMD", json!(cmd)),
                Category::SharedSecret,
                "{cmd}"
            );
        }
        for cmd in [
            "curl -H 'Authorization: Bearer $TOKEN' https://api.example.invalid",
            "curl -H \"Authorization: Bearer ${API_TOKEN}\" https://api.example.invalid",
            "curl -H 'X-API-Key: ${API_KEY}' https://api.example.invalid",
            "curl -H \"DD-API-KEY: $DD_API_KEY\" https://api.example.invalid",
            "curl -H 'Content-Type: application/json' https://api.example.invalid",
            "echo monkey: 1",
            "curl -H 'apikey: $SUPABASE_KEY' https://x.example.invalid",
            "curl -H 'Token: ${TOKEN}' https://x.example.invalid",
        ] {
            let v = json!({"Stop": [{"hooks": [{"type": "command", "command": cmd}]}]});
            assert_eq!(basic("hooks", v), Category::Basic, "{cmd}");
        }
        // Claude Code's own permission rules and format strings name a
        // token without holding one: never held.
        let permissions = json!({
            "allow": [
                "Bash(gcloud auth print-access-token:*)",
                "Bash(aws sts get-session-token:*)",
                "Bash(az account get-access-token:*)",
                "Bash(npm run test:*)",
                "Bash(git status:*)",
                "Bash(gh auth token:*)",
                "WebFetch(domain:docs.anthropic.com)",
                "Read(~/.config/gh/hosts.yml)"
            ],
            "deny": ["Bash(rm -rf:*)"],
            "defaultMode": "acceptEdits"
        });
        assert_eq!(basic("permissions", permissions), Category::Basic);
        for cmd in [
            "printf 'cache-key: %s' x",
            "du -h key: x",
            "du -h token: abcdefghijkl",
            "echo 'x-api-key: %s'",
        ] {
            assert_eq!(
                basic("statusLine", json!({"command": cmd})),
                Category::Basic,
                "{cmd}"
            );
        }
        // A shell assignment to a secret-named (or account-bound) variable
        // in a command; not one to another name, nor of another variable.
        let fake = "abc123".repeat(2);
        for cmd in [
            "PGPASSWORD=hunter2 psql -h db -tc 'select 1'".to_string(),
            format!("API_TOKEN={fake} ./notify.sh"),
            format!("export GITHUB_TOKEN={fake}; gh pr list"),
            format!("cd x && DB_PASS='{fake}' ./run"),
            format!("ANTHROPIC_API_KEY=\"{fake}\" claude -p hi"),
        ] {
            assert_eq!(
                basic("statusLine", json!({"command": cmd})),
                Category::SharedSecret,
                "{cmd}"
            );
            let v = json!({"Stop": [{"hooks": [{"type": "command", "command": cmd}]}]});
            assert_eq!(basic("hooks", v), Category::SharedSecret, "{cmd}");
        }
        for cmd in [
            "FOO=bar cmd",
            "NODE_ENV=production npm start",
            "API_TOKEN=$API_TOKEN ./x",
            "API_TOKEN=${API_TOKEN} ./x",
            "PATH=/usr/bin:$PATH make",
            "API_TOKEN= ./x",
            "TOKEN=$(cat ~/.token) ./notify.sh",
            "API_KEY=\"$(pass show x)\" ./run",
            "token=$1 ./run",
            "SECRET=`cat ~/.s` ./run",
        ] {
            assert_eq!(
                basic("statusLine", json!({"command": cmd})),
                Category::Basic,
                "{cmd}"
            );
        }
        // An env value is classed by its own name, not by assignments in it.
        assert_eq!(
            basic("env.RUNNER", json!("FOO_TOKEN=x ./run")),
            Category::Basic
        );
        // A password given to curl.
        for cmd in [
            "curl -u admin:pw https://api.example.invalid/notify",
            "curl -uadmin:pw https://api.example.invalid/notify",
            "curl --user admin:pw https://api.example.invalid/notify",
            "curl --user=admin:pw https://api.example.invalid/notify",
            "curl -u 'admin:pw' https://api.example.invalid/notify",
            // The last curl before `-u` counts, and a separator after it doesn't.
            "cd /tmp && curl -u admin:pw https://api.example.invalid/notify",
            "curl -s https://x.example.invalid && curl -u admin:pw https://api.example.invalid/notify",
            "curl -u admin:pw https://api.example.invalid/notify | jq .",
        ] {
            let v = json!({"Stop": [{"hooks": [{"type": "command", "command": cmd}]}]});
            assert_eq!(basic("hooks", v), Category::SharedSecret, "{cmd}");
        }
        for cmd in [
            "curl -u $USER:$PASS https://api.example.invalid",
            "curl -u user https://api.example.invalid",
            "sort -u file",
            "ps -u root",
            "docker run -u 1000:1000 img",
            "date -u +%H:%M:%S",
            "echo \"$(date -u +%Y-%m-%dT%H:%M:%SZ) done\" >> log",
            "docker run --user node:node img",
            "rsync -u host:/dir .",
            // A `date -u` after a curl is not curl's `-u` (#134).
            "curl -s -X POST https://ntfy.sh/topic -d \"Claude finished at $(date -u +%H:%M)\"",
            "curl -s wttr.in/?format=3; date -u +%H:%M",
            // A `-u` past a separator belongs to another command.
            "curl -s https://x.example.invalid; docker run --user node:pw img",
            "curl -s https://x.example.invalid | docker run --user node:pw img",
            "curl -s https://x.example.invalid && docker run --user node:pw img",
            "curl -s https://x.example.invalid\ndocker run --user node:pw img",
            "curl -d \"$(id -u node:pw)\" https://x.example.invalid",
            // A `date` format is never a user.
            "curl -u +%H:%M https://x.example.invalid",
            "curl -u %H:%M https://x.example.invalid",
        ] {
            let v = json!({"Stop": [{"hooks": [{"type": "command", "command": cmd}]}]});
            assert_eq!(basic("hooks", v), Category::Basic, "{cmd}");
        }
        // PASSPHRASE, and the header variables Claude Code reads.
        for name in [
            "GPG_PASSPHRASE",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "ANTHROPIC_CUSTOM_HEADERS",
        ] {
            assert_eq!(
                basic(&format!("env.{name}"), json!("x-api-key=AbCdEfGhIjKlMnOp")),
                Category::SharedSecret,
                "{name}"
            );
        }
        for name in ["CLAUDE_CODE_MAX_OUTPUT_TOKENS", "HEADER_STYLE"] {
            assert_eq!(
                basic(&format!("env.{name}"), json!("8000")),
                Category::Basic,
                "{name}"
            );
        }
        // Multi-byte whitespace or quotes before a run never break the
        // reading (NBSP is 2 bytes, U+3000 3, an emoji 4).
        let segment = "-Users-me-src-some-really-long-project-name1";
        for sep in ['\u{a0}', '\u{3000}', '😀'] {
            let path = format!("bash{sep}/home/{segment}/run.sh");
            assert_eq!(
                basic("statusLine", json!({"command": path})),
                Category::Basic,
                "{path:?}"
            );
            let quoted = format!("bash {sep}'/home/{segment}/run.sh'");
            assert_eq!(
                basic("statusLine", json!({"command": quoted})),
                Category::Basic,
                "{quoted:?}"
            );
            let hook = format!(
                "curl{sep}https://example.invalid/x/{}",
                "AbCdEfGh12345678".repeat(3)
            );
            assert_eq!(
                basic("hooks", json!([hook.clone()])),
                Category::SharedSecret,
                "{hook:?}"
            );
        }
        // A path segment with a long mixed-case run (Claude Code names
        // project dirs so) is not a token: the statusline shim's path.
        let segment = "-Users-montimage-buildspace-luongnv89-mcc";
        assert!(segment.len() >= 40);
        for v in [
            json!({"type": "command", "command": format!("/private/tmp/{segment}/repo/.mcc/bin/mcc statusline")}),
            json!(format!("bash '/home/me/{segment}/x.sh' --flag")),
            json!(format!("C:\\Users\\{segment}\\run.cmd")),
            json!(format!("/Users/x/.claude/projects/{segment}/memory")),
            json!(format!(
                "curl https://example.invalid/ping && bash /Users/x/{segment}/run.sh"
            )),
        ] {
            assert_eq!(basic("statusLine", v.clone()), Category::Basic, "{v}");
        }
        // The same run standing alone still is.
        assert_eq!(
            basic("x", json!(format!("--key {}", &segment[1..]))),
            Category::SharedSecret
        );
        assert_eq!(
            basic("x", json!("0d5b1f6e-8a3c-4b7e-9f21-6c3d2e1a0b9f")),
            Category::Basic
        );
        assert_eq!(
            basic("x", json!("3f786850e387550fdab836ed7e6dc881de23001b")),
            Category::Basic
        );
        assert_eq!(
            basic("env.RUN_ID", json!("0d5b1f6e-8a3c-4b7e-9f21-6c3d2e1a0b9f")),
            Category::Basic
        );
        // Config overrides; account-bound wins.
        let c = Classifier::new(&SyncConfig {
            account_keys: vec!["env.MY_ORG".into()],
            secret_keys: vec!["env.MY_ORG".into(), "companyKey".into()],
            ..SyncConfig::default()
        });
        assert_eq!(c.category("env.MY_ORG", None), Category::AccountBound);
        assert_eq!(c.category("companyKey", None), Category::SharedSecret);
    }

    /// The settings `Sandbox::seed_default` writes are all basic.
    #[test]
    fn a_typical_settings_file_is_basic() {
        let home = "/private/var/folders/x1/abcdef_ghij1234/T/.tmpAbC123";
        let s = obj(json!({
            "model": "opus",
            "enabledPlugins": {"p1@m": true, "p2@m": false},
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": format!("bash '{home}/.claude/hooks/notify.sh'")}]}]},
            "statusLine": {"command": "~/.claude/statusline.sh"},
            "permissions": {"allow": ["Bash(git status:*)", "WebFetch(domain:docs.anthropic.com)"]}
        }));
        let c = classifier();
        for (k, v) in flatten(&s) {
            assert_eq!(c.category(&k, Some(&v)), Category::Basic, "{k}");
        }
    }

    #[test]
    fn account_bound_keys_can_never_be_shared() {
        let c = classifier();
        assert!(c.shareable("env.ANTHROPIC_API_KEY", json!("x")).is_none());
        assert!(c.shareable("env.ANTHROPIC_BASE_URL", json!("x")).is_none());
        assert!(c.removable("apiKeyHelper").is_none());
        assert!(c.shareable("env.TAVILY_API_KEY", json!("x")).is_some());
        let shared = c.shared(obj(json!({
            "env.ANTHROPIC_API_KEY": "a", "env.ANTHROPIC_BASE_URL": "b", "apiKeyHelper": "c", "env": null, "model": "opus"
        })));
        assert_eq!(shared.into_map().keys().collect::<Vec<_>>(), ["model"]);
        // A non-object `env` (bare "env") can never be shared or removed,
        // so it can never overwrite an `env` object and its account keys.
        assert!(c.shareable("env", Value::Null).is_none());
        assert!(c.removable("env").is_none());
    }

    #[test]
    fn masked_values_tell_only_whether_they_differ() {
        let c = classifier();
        let a = obj(
            json!({"env.TAVILY_API_KEY": "one", "env.ANTHROPIC_API_KEY": "k", "model": "opus"}),
        );
        let b = obj(
            json!({"env.TAVILY_API_KEY": "two", "env.ANTHROPIC_API_KEY": "k", "model": "haiku"}),
        );
        let left = Value::Object(c.masked(&a, &b, false)).to_string();
        let right = Value::Object(c.masked(&b, &a, true)).to_string();
        assert_eq!(
            left,
            r#"{"env.TAVILY_API_KEY":"<hidden>","env.ANTHROPIC_API_KEY":"<hidden>","model":"opus"}"#
        );
        assert_eq!(
            right,
            r#"{"env.TAVILY_API_KEY":"<hidden, differs>","env.ANTHROPIC_API_KEY":"<hidden>","model":"haiku"}"#
        );
    }

    fn view(want: Value, have: Value, base: KeyBase) -> SettingsView {
        SettingsView {
            want: obj(want),
            have: obj(have),
            have_obj: None,
            base,
            overlay_keys: BTreeSet::new(),
            overlay_env: false,
            linked: false,
        }
    }

    fn keys_of(v: &Value) -> KeyBase {
        KeyBase::Keys(
            obj(v.clone())
                .iter()
                .map(|(k, v)| (k.clone(), manifest::hash_json(v)))
                .collect(),
        )
    }

    fn run(default: Value, views: &[(&str, &SettingsView)]) -> Vec<(String, Verdict, Vec<String>)> {
        let same = |v: &Value| Some(v.clone());
        let sources: Vec<Source> = views
            .iter()
            .map(|(alias, view)| Source {
                alias,
                view,
                to_default: &same,
            })
            .collect();
        decide(&classifier(), &obj(default), &sources)
            .into_iter()
            .map(|d| (d.key, d.verdict, d.sources))
            .collect()
    }

    #[test]
    fn a_change_in_one_profile_is_taken_and_two_are_a_conflict() {
        let base = json!({"model": "opus", "env.A": "1"});
        let d = json!({"model": "opus", "env.A": "1"});
        let one = view(
            d.clone(),
            json!({"model": "opus", "env.A": "1", "env.CODE": "1"}),
            keys_of(&base),
        );
        let two = view(d.clone(), json!({"model": "opus"}), keys_of(&base));
        let three = view(
            d.clone(),
            json!({"model": "sonnet", "env.A": "1"}),
            keys_of(&base),
        );
        let four = view(
            d.clone(),
            json!({"model": "haiku", "env.A": "1"}),
            keys_of(&base),
        );
        let c = classifier();
        let got = run(
            d.clone(),
            &[
                ("one", &one),
                ("two", &two),
                ("three", &three),
                ("four", &four),
            ],
        );
        assert_eq!(
            got,
            vec![
                (
                    "env.A".into(),
                    Verdict::Remove(c.removable("env.A").unwrap()),
                    vec!["two".into()]
                ),
                (
                    "env.CODE".into(),
                    Verdict::Set(c.shareable("env.CODE", json!("1")).unwrap()),
                    vec!["one".into()]
                ),
                (
                    "model".into(),
                    Verdict::Conflict {
                        with_default: false,
                        default_has: true
                    },
                    vec!["three".into(), "four".into()]
                ),
            ]
        );
        // The default changed it too: a conflict.
        let moved = view(
            json!({"model": "fast"}),
            json!({"model": "sonnet"}),
            keys_of(&json!({"model": "opus"})),
        );
        assert_eq!(
            run(json!({"model": "fast"}), &[("p", &moved)]),
            vec![(
                "model".into(),
                Verdict::Conflict {
                    with_default: true,
                    default_has: true
                },
                vec!["p".into()]
            )]
        );
        // Unchanged since the record, or already the default's: nothing.
        let same = view(
            json!({"model": "fast"}),
            json!({"model": "opus"}),
            keys_of(&json!({"model": "opus"})),
        );
        assert!(run(json!({"model": "fast"}), &[("p", &same)]).is_empty());
        // No record at all: never a source.
        let fresh = view(json!({}), json!({"model": "x"}), KeyBase::TwoWay);
        assert!(run(json!({}), &[("p", &fresh)]).is_empty());
    }

    #[test]
    fn account_bound_and_overlaid_keys_are_never_taken() {
        let base = keys_of(&json!({}));
        let p = view(
            json!({}),
            json!({"env.ANTHROPIC_BASE_URL": "https://x", "model": "o"}),
            base.clone(),
        );
        let mut q = view(json!({}), json!({"model": "o"}), base);
        q.overlay_keys.insert("model".into());
        let got = run(json!({}), &[("p", &p), ("q", &q)]);
        assert_eq!(got[0].0, "env.ANTHROPIC_BASE_URL");
        assert_eq!(got[0].1, Verdict::AccountBound);
        // q's model is its overlay's: only p's counts.
        assert_eq!(got[1].2, vec!["p".to_string()]);
    }

    /// The first sync after upgrading: whole-file records only. With the
    /// default changed since, a key only the profile has is a proposal
    /// (new there, or dropped by the default?), one both have otherwise a
    /// conflict; never a removal.
    #[test]
    fn guessing_proposes_what_only_a_profile_has_and_never_removes() {
        let guess = KeyBase::Guess {
            default_unchanged: false,
        };
        let d = json!({"env.TAVILY_API_KEY": "t0", "model": "opus", "statusLine": "new"});
        let a = view(
            d.clone(),
            json!({"env.TAVILY_API_KEY": "t1", "env.CODE": "1", "env.NEW_TOKEN": "x", "model": "opus", "statusLine": "old"}),
            guess.clone(),
        );
        let b = view(
            d.clone(),
            json!({"env.TAVILY_API_KEY": "t0"}),
            guess.clone(),
        );
        let got = run(d.clone(), &[("a", &a), ("b", &b)]);
        let c = classifier();
        let conflict = Verdict::Conflict {
            with_default: true,
            default_has: true,
        };
        assert_eq!(
            got,
            vec![
                (
                    "env.CODE".into(),
                    Verdict::Propose(c.shareable("env.CODE", json!("1")).unwrap()),
                    vec!["a".into()]
                ),
                (
                    "env.NEW_TOKEN".into(),
                    Verdict::Propose(c.shareable("env.NEW_TOKEN", json!("x")).unwrap()),
                    vec!["a".into()]
                ),
                (
                    "env.TAVILY_API_KEY".into(),
                    conflict.clone(),
                    vec!["a".into()]
                ),
                ("statusLine".into(), conflict.clone(), vec!["a".into()]),
            ]
        );
        // Two profiles agreeing on a value the default has otherwise.
        let x = view(d.clone(), json!({"model": "haiku"}), guess.clone());
        let y = view(d.clone(), json!({"model": "haiku"}), guess.clone());
        assert_eq!(run(d.clone(), &[("x", &x), ("y", &y)])[0].1, conflict);
        // One proposal for every profile that has the same value; values
        // that differ are a conflict among the profiles.
        let x = view(json!({}), json!({"env.CODE": "1"}), guess.clone());
        let y = view(json!({}), json!({"env.CODE": "1"}), guess.clone());
        let z = view(json!({}), json!({"env.CODE": "2"}), guess.clone());
        assert_eq!(
            run(json!({}), &[("x", &x), ("y", &y)]),
            vec![(
                "env.CODE".into(),
                Verdict::Propose(c.shareable("env.CODE", json!("1")).unwrap()),
                vec!["x".into(), "y".into()]
            )]
        );
        assert_eq!(
            run(json!({}), &[("x", &x), ("z", &z)])[0].1,
            Verdict::Conflict {
                with_default: false,
                default_has: false
            }
        );

        // The default unchanged since the record: a key the profile changed
        // or added is taken, but one it lacks is never removed.
        let same = KeyBase::Guess {
            default_unchanged: true,
        };
        let p = view(
            d.clone(),
            json!({"env.TAVILY_API_KEY": "t0", "model": "sonnet", "env.CODE": "1"}),
            same,
        );
        assert_eq!(
            run(d.clone(), &[("p", &p)]),
            vec![
                (
                    "env.CODE".into(),
                    Verdict::Set(c.shareable("env.CODE", json!("1")).unwrap()),
                    vec!["p".into()]
                ),
                (
                    "model".into(),
                    Verdict::Set(c.shareable("model", json!("sonnet")).unwrap()),
                    vec!["p".into()]
                ),
            ]
        );
        // A proposal recorded as such stays one next time.
        let next = view(
            json!({}),
            json!({"env.CODE": "1"}),
            KeyBase::Keys([("env.CODE".to_string(), PROPOSED_BASE.to_string())].into()),
        );
        assert_eq!(
            run(json!({}), &[("p", &next)])[0].1,
            Verdict::Propose(c.shareable("env.CODE", json!("1")).unwrap())
        );
    }

    /// The per-key record saved for a profile read by guess reads every
    /// key as the guess did, against the same default.
    #[test]
    fn a_per_key_record_reads_like_the_guess() {
        let c = classifier();
        let want = json!({"same": 1, "theirs": 2, "both": 3, "lost": 4});
        let have = json!({"same": 1, "both": 9, "ours": 5, "env.ANTHROPIC_BASE_URL": "x"});
        for unchanged in [true, false] {
            let g = view(
                want.clone(),
                have.clone(),
                KeyBase::Guess {
                    default_unchanged: unchanged,
                },
            );
            let record = per_key_record(&g, &c).expect("a guess");
            let k = view(want.clone(), have.clone(), KeyBase::Keys(record));
            for key in g.keys() {
                let (w, p) = (g.want.get(&key), g.have.get(&key));
                if w == p || c.is_account_bound(&key) {
                    continue;
                }
                let (a, b) = (
                    key_state(&g.base, &key, w, p),
                    key_state(&k.base, &key, w, p),
                );
                // The guess's own base for Ours is what the record keeps.
                let norm = |s: KeyState| match s {
                    KeyState::Ours { .. } => KeyState::Ours { base: None },
                    s => s,
                };
                assert_eq!(norm(a), norm(b), "{unchanged} {key}");
            }
        }
        // Not for a profile with a per-key record.
        let k = view(want, have, KeyBase::Keys(BTreeMap::new()));
        assert!(per_key_record(&k, &c).is_none());
    }

    /// A conflict says what `--force` does: keep the default's value, or,
    /// when the default has none, remove the key from the profile.
    #[test]
    fn conflicts_say_what_force_does() {
        let c = |with_default, default_has| Outcome::Conflict {
            with_default,
            default_has,
        };
        assert_eq!(
            c(true, true).describe(false, true),
            "conflict: changed differently in the default and the profile; skipped (--force keeps the default's value)"
        );
        assert_eq!(
            c(true, false).describe(false, true),
            "conflict: changed in the profile, removed from the default; skipped (--force removes it from the profile)"
        );
        assert_eq!(
            c(true, false).describe(false, false),
            "conflict: changed in the profile, removed from the default; skipped"
        );
        // --force doesn't settle a conflict among profiles.
        assert_eq!(
            c(false, true).describe(false, true),
            "conflict: changed differently in several profiles; skipped"
        );
    }

    /// Every record against every shape of a differing key: the reading
    /// `decide` and the profile's plan share ([`key_state`]).
    #[test]
    fn key_states_cover_every_record() {
        use KeyState::*;
        let (a, b) = (json!("a"), json!("b"));
        let h = |v: &Value| manifest::hash_json(v);
        let keys = |base: Option<String>| {
            KeyBase::Keys(base.into_iter().map(|b| ("k".to_string(), b)).collect())
        };
        let guess = |default_unchanged| KeyBase::Guess { default_unchanged };
        let cases: Vec<(KeyBase, Option<&Value>, Option<&Value>, KeyState)> = vec![
            (KeyBase::TwoWay, Some(&a), Some(&b), Theirs),
            (KeyBase::TwoWay, Some(&a), None, Theirs),
            (KeyBase::TwoWay, None, Some(&b), Untracked),
            // A real base.
            (keys(Some(h(&b))), Some(&a), Some(&b), Theirs),
            (keys(Some(h(&a))), Some(&a), Some(&b), Ours { base: None }),
            (keys(Some(h(&a))), Some(&a), None, Ours { base: None }),
            (keys(Some(h(&a))), None, Some(&b), Both),
            (keys(Some(h(&json!("c")))), Some(&a), Some(&b), Both),
            // No entry: the default's new key, or the profile's.
            (keys(None), Some(&a), None, Theirs),
            (keys(None), None, Some(&b), Ours { base: None }),
            (keys(None), Some(&a), Some(&b), Both),
            // A pending proposal: never a conflict while the default lacks it.
            (keys(Some(PROPOSED_BASE.into())), None, Some(&b), Proposed),
            (keys(Some(PROPOSED_BASE.into())), Some(&a), None, Theirs),
            (keys(Some(PROPOSED_BASE.into())), Some(&a), Some(&b), Both),
            // Whole-file records.
            (guess(true), Some(&a), Some(&b), Ours { base: Some(h(&a)) }),
            (guess(true), None, Some(&b), Ours { base: None }),
            (
                guess(true),
                Some(&a),
                None,
                Lost {
                    base: Some(format!("{LOST_PREFIX}{}", h(&a))),
                },
            ),
            // Lost: never a removal; settles once the default changes it, or
            // the profile has it again.
            (
                keys(Some(format!("{LOST_PREFIX}{}", h(&a)))),
                Some(&a),
                None,
                Lost {
                    base: Some(format!("{LOST_PREFIX}{}", h(&a))),
                },
            ),
            (
                keys(Some(format!("{LOST_PREFIX}{}", h(&b)))),
                Some(&a),
                None,
                Theirs,
            ),
            (
                keys(Some(format!("{LOST_PREFIX}{}", h(&a)))),
                Some(&a),
                Some(&b),
                Ours { base: None },
            ),
            (guess(false), Some(&a), Some(&b), Both),
            (guess(false), None, Some(&b), Proposed),
            (guess(false), Some(&a), None, Theirs),
        ];
        for (base, w, p, want) in cases {
            assert_eq!(key_state(&base, "k", w, p), want, "{base:?} {w:?} {p:?}");
        }
    }
}
