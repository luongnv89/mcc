//! `settings.json` synced key by key (#129). Each key of a profile's
//! `settings.json` (top-level, with `env` split into one key per variable,
//! `env.NAME`) falls in one [`Category`]:
//!
//! - **basic**: no secret; a change in any profile reaches every profile;
//! - **shared secret**: an API key or token not tied to an account; `mcc
//!   sync` asks before copying it and never shows its value;
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
/// [`crate::refresh::STRIPPED_ENV`] (as `env.NAME`).
const ACCOUNT_KEYS: &[&str] = &[
    "apiKeyHelper",
    "awsAuthRefresh",
    "awsCredentialExport",
    "forceLoginMethod",
    "forceLoginOrgUUID",
];

/// An `env` variable named like a secret: one of these words, as a whole
/// `_`-separated part of the name (so `MAX_THINKING_TOKENS` is not one).
static SECRET_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|_)(?:API_?KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIALS?|PRIVATE_KEY|ACCESS_KEY|AUTH)(?:_|$)",
    )
    .expect("static regex")
});

/// Text shaped like a well-known token, wherever it is.
static TOKEN_SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"sk-ant-[A-Za-z0-9_-]{8,}|\bsk-[A-Za-z0-9_-]{20,}|\bghp_[A-Za-z0-9]{20,}|\bgithub_pat_[A-Za-z0-9_]{20,}|\bxox[bp]-[A-Za-z0-9-]{10,}|\bAKIA[0-9A-Z]{16}\b",
    )
    .expect("static regex")
});

/// A long run that may be a random token: checked for mixed case and
/// digits by [`holds_token`]. `/` and `.` are left out, so paths are not.
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
/// token prefix, or a long run of mixed-case letters and digits.
fn holds_token(v: &Value) -> bool {
    match v {
        Value::String(s) => {
            TOKEN_SHAPE.is_match(s)
                || LONG_RUN.find_iter(s).any(|m| {
                    let run = m.as_str();
                    run.bytes().any(|b| b.is_ascii_uppercase())
                        && run.bytes().any(|b| b.is_ascii_lowercase())
                        && run.bytes().any(|b| b.is_ascii_digit())
                })
        }
        Value::Array(a) => a.iter().any(holds_token),
        Value::Object(m) => m.values().any(holds_token),
        _ => false,
    }
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
    /// shared secret.
    pub fn category(&self, key: &str, value: Option<&Value>) -> Category {
        if self.is_account_bound(key) {
            Category::AccountBound
        } else if self.secret.iter().any(|k| k == key)
            || key
                .strip_prefix(ENV_PREFIX)
                .is_some_and(|name| SECRET_NAME.is_match(name))
            || value.is_some_and(holds_token)
        {
            Category::SharedSecret
        } else {
            Category::Basic
        }
    }

    /// The key `key` holding `value`, to be copied into other profiles;
    /// `None` when it is account-bound, which is never copied.
    pub fn shareable(&self, key: &str, value: Value) -> Option<Shareable> {
        (!self.is_account_bound(key)).then(|| Shareable {
            key: key.to_string(),
            value,
        })
    }

    /// The key `key`, to be removed from other profiles; `None` when it is
    /// account-bound, which sync never removes.
    pub fn removable(&self, key: &str) -> Option<SharedKey> {
        (!self.is_account_bound(key)).then(|| SharedKey(key.to_string()))
    }

    /// The flat settings `flat` without their account-bound keys: what
    /// may reach another profile.
    pub fn shared(&self, flat: Map<String, Value>) -> Shared {
        Shared(
            flat.into_iter()
                .filter(|(k, _)| !self.is_account_bound(k))
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
    /// guessed. `default_unchanged`: the default still gives what mcc
    /// wrote, so every key that differs was changed in the profile.
    Guess { default_unchanged: bool },
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
}

impl SettingsView {
    /// Whether the profile's overlay decides `key`.
    pub fn overlaid(&self, key: &str) -> bool {
        self.overlay_keys.contains(key) || (self.overlay_env && key.starts_with(ENV_PREFIX))
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

    /// Whether the profile changed `key` since its record, and if so
    /// whether the default did too (`Some(true)`, a conflict). Profiles
    /// without a record are never sources; neither is a key the overlay
    /// sets, or one that already matches the default.
    fn changed(&self, key: &str) -> Option<Change> {
        if self.overlaid(key) {
            return None;
        }
        let (w, p) = (self.want.get(key), self.have.get(key));
        if w == p {
            return None;
        }
        match &self.base {
            KeyBase::TwoWay => None,
            KeyBase::Keys(b) => {
                let last = b.get(key).map(String::as_str);
                (hash(p).as_deref() != last).then(|| Change::Exact {
                    default_changed: hash(w).as_deref() != last,
                })
            }
            KeyBase::Guess {
                default_unchanged: true,
            } => Some(Change::Exact {
                default_changed: false,
            }),
            // Never a removal by guess: the default may have added it.
            KeyBase::Guess {
                default_unchanged: false,
            } => p.is_some().then_some(Change::Guessed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Change {
    /// Known from the record.
    Exact { default_changed: bool },
    /// Guessed: the record holds only the whole file's hash.
    Guessed,
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
    /// Changed differently in several places (`with_default`: the default
    /// among them): nothing is taken.
    Conflict { with_default: bool },
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
/// per-key record ([`KeyBase::Guess`]), a key is taken only when one
/// profile has it differently, or the default lacks it and the profiles
/// that have it agree; a shared secret is then never taken (a conflict).
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
        let with_default = cands.iter().any(|(_, _, ch)| {
            matches!(
                ch,
                Change::Exact {
                    default_changed: true
                }
            )
        });
        let guessed = cands.iter().any(|(_, _, ch)| *ch == Change::Guessed);
        let conflict = proposals.len() > 1
            || with_default
            || (guessed
                && (category == Category::SharedSecret
                    || (cands.len() > 1 && default.contains_key(&key))));
        let verdict = if conflict {
            Verdict::Conflict {
                with_default: with_default || guessed,
            }
        } else {
            match cands[0].1.clone() {
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
    /// A shared secret, or a removal without `--yes`, a real sync would
    /// ask about (dry run).
    Asked,
    /// A shared secret the user agreed to sync.
    Approved,
    /// A shared secret the user declined (`earlier`: in a previous sync).
    Declined { earlier: bool },
    /// Left as it is, for this reason.
    Skipped(&'static str),
    /// Changed differently in several places; left as it is.
    Conflict { with_default: bool },
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
            Outcome::Conflict { with_default } => format!(
                "conflict: changed differently in {}; skipped{}",
                if with_default {
                    "the default and the profile"
                } else {
                    "several profiles"
                },
                if force {
                    " (--force keeps the default's value)"
                } else {
                    ""
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
            "AWS_SECRET_ACCESS_KEY",
            "DB_PASSWORD",
            "my_credentials",
            "BASIC_AUTH",
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
        // Paths, UUIDs and hex hashes are not.
        assert_eq!(
            basic(
                "statusLine",
                json!({"command": "bash '/Users/someone/.mcc/profiles/work-account-2/hooks/statusline-command-runner.sh'"})
            ),
            Category::Basic
        );
        assert_eq!(
            basic("x", json!("0d5b1f6e-8a3c-4b7e-9f21-6c3d2e1a0b9f")),
            Category::Basic
        );
        assert_eq!(
            basic("x", json!("3f786850e387550fdab836ed7e6dc881de23001b")),
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
            "env.ANTHROPIC_API_KEY": "a", "env.ANTHROPIC_BASE_URL": "b", "apiKeyHelper": "c", "model": "opus"
        })));
        assert_eq!(shared.into_map().keys().collect::<Vec<_>>(), ["model"]);
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
                        with_default: false
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
                Verdict::Conflict { with_default: true },
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

    /// The first sync after upgrading: whole-file records only.
    #[test]
    fn guessing_never_spreads_a_secret_nor_removes() {
        let guess = KeyBase::Guess {
            default_unchanged: false,
        };
        let d = json!({"env.TAVILY_API_KEY": "t0", "model": "opus"});
        let a = view(
            d.clone(),
            json!({"env.TAVILY_API_KEY": "t1", "env.CODE": "1", "model": "opus"}),
            guess.clone(),
        );
        let b = view(
            d.clone(),
            json!({"env.TAVILY_API_KEY": "t0"}),
            guess.clone(),
        );
        let got = run(d.clone(), &[("a", &a), ("b", &b)]);
        let c = classifier();
        assert_eq!(
            got,
            vec![
                (
                    "env.CODE".into(),
                    Verdict::Set(c.shareable("env.CODE", json!("1")).unwrap()),
                    vec!["a".into()]
                ),
                (
                    "env.TAVILY_API_KEY".into(),
                    Verdict::Conflict { with_default: true },
                    vec!["a".into()]
                ),
            ]
        );
        // Two profiles agreeing on a value the default has otherwise.
        let x = view(d.clone(), json!({"model": "haiku"}), guess.clone());
        let y = view(d.clone(), json!({"model": "haiku"}), guess);
        assert_eq!(
            run(d.clone(), &[("x", &x), ("y", &y)])[0].1,
            Verdict::Conflict { with_default: true }
        );
    }
}
