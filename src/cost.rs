//! An API-key profile's estimated spend (#132), from the cost Claude Code
//! reports to its status line.
//!
//! Claude Code gives the status line each session's running cost
//! (`cost.total_cost_usd`, an estimate at list prices) with its
//! `session_id`. For an API-key profile the `mcc statusline` shim keeps one
//! ledger per session, `~/.mcc/state/<profile>/cost/<session_id>.json`:
//! the last total it saw and what the session added in each UTC month
//! (`{"last_total": …, "months": {"YYYY-MM": usd}, "updated_at": …}`).
//! `mcc list` and `mcc usage` add up the current month across a profile's
//! ledgers ([`month_to_date`]). It is an estimate for this machine only:
//! sessions on other machines, and API use outside Claude Code, are not in
//! it; mcc can't read the account's real spend or credit balance (see
//! docs/api-key-profiles.md).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::Paths;

/// The ledgers' directory inside a profile's state dir.
pub const DIR: &str = "cost";

/// Ledgers left alone this long are removed when a new session starts.
const KEEP_DAYS: i64 = 62;

/// Longest session id kept as a file name.
const SESSION_ID_MAX: usize = 128;

/// One session's ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    /// The session's total cost when last seen, in US dollars.
    pub last_total: f64,
    /// What the session added in each UTC month (`YYYY-MM`), in US dollars.
    #[serde(default)]
    pub months: BTreeMap<String, f64>,
    pub updated_at: String,
}

fn month(now: DateTime<Utc>) -> String {
    now.format("%Y-%m").to_string()
}

/// The ledger after the session's total cost is seen at `total`. The first
/// sight of a session is its baseline and adds nothing, so a total a
/// forked or resumed session carries over is never counted twice (a new
/// session reports 0 first). After that, a higher total adds the
/// difference; a lower one means Claude Code started counting again
/// (`/clear`), so all of it is new. The amount goes to the current UTC month.
pub fn advance(prev: Option<&Ledger>, total: f64, now: DateTime<Utc>) -> Ledger {
    let updated_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let Some(prev) = prev else {
        return Ledger {
            last_total: total,
            months: BTreeMap::new(),
            updated_at,
        };
    };
    let delta = if total >= prev.last_total {
        total - prev.last_total
    } else {
        total
    };
    let mut months = prev.months.clone();
    if delta > 0.0 {
        *months.entry(month(now)).or_insert(0.0) += delta;
    }
    Ledger {
        last_total: total,
        months,
        updated_at,
    }
}

/// A session id that is safe as a file name: 1–128 of `[A-Za-z0-9._-]`,
/// not starting with a dot.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= SESSION_ID_MAX
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn dir(paths: &Paths, profile: &str) -> PathBuf {
    paths.profile_state_dir(profile).join(DIR)
}

fn read_ledger(path: &Path) -> Option<Ledger> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Record the status line's `status` JSON for `profile`'s ledger: its
/// `session_id` and `cost.total_cost_usd`. Nothing happens without both, for
/// a session id that isn't a plain file name, or a total that isn't a
/// number of dollars; nothing is written when the total hasn't changed.
/// Best effort: the shim must never fail on it, so errors are dropped.
pub fn record(paths: &Paths, profile: &str, status: &Value, now: DateTime<Utc>) {
    let Some(id) = status.get("session_id").and_then(Value::as_str) else {
        return;
    };
    let Some(total) = status
        .pointer("/cost/total_cost_usd")
        .and_then(Value::as_f64)
        .filter(|t| t.is_finite() && *t >= 0.0)
    else {
        return;
    };
    if !valid_session_id(id) {
        return;
    }
    let dir = dir(paths, profile);
    let path = dir.join(format!("{id}.json"));
    let prev = read_ledger(&path);
    if prev.as_ref().is_some_and(|p| p.last_total == total) {
        return;
    }
    if prev.is_none() {
        prune(&dir, now);
    }
    let next = advance(prev.as_ref(), total, now);
    if let Ok(v) = serde_json::to_value(&next) {
        let _ = crate::commands::statusline::write_json(&path, &v);
    }
}

/// Remove the ledgers in `dir` left alone for [`KEEP_DAYS`] or more: only
/// the current month counts, and a session that old adds nothing to it.
fn prune(dir: &Path, now: DateTime<Utc>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::from(now - chrono::Duration::days(KEEP_DAYS));
    for e in entries.flatten() {
        let path = e.path();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t < cutoff);
        if old && path.extension().is_some_and(|x| x == "json") {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// `profile`'s estimated spend in the current UTC month, in US dollars: the
/// sum over its ledgers. `None` when no ledger is there (no session seen
/// yet, or the shim isn't installed). Read-only.
pub fn month_to_date(paths: &Paths, profile: &str, now: DateTime<Utc>) -> Option<f64> {
    let entries = std::fs::read_dir(dir(paths, profile)).ok()?;
    let current = month(now);
    let mut seen = false;
    let mut sum = 0.0;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        if let Some(l) = read_ledger(&path) {
            seen = true;
            sum += l
                .months
                .get(&current)
                .copied()
                .filter(|v| v.is_finite())
                .unwrap_or(0.0);
        }
    }
    seen.then_some(sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn spent(l: &Ledger, m: &str) -> f64 {
        l.months.get(m).copied().unwrap_or(0.0)
    }

    /// T6: growth adds the difference; the same total again (a resumed
    /// session's first render) adds nothing; a lower total is a reset whose
    /// whole value is new; the first sight is a baseline (a fork carrying a
    /// total over adds nothing); each month keeps its own amount.
    #[test]
    fn the_ledger_counts_each_dollar_once() {
        let t = at("2026-10-31T23:00:00Z");
        let first = advance(None, 2.5, t);
        assert_eq!(first.last_total, 2.5);
        assert!(first.months.is_empty(), "the baseline adds nothing");
        let grown = advance(Some(&first), 3.0, t);
        assert_eq!(spent(&grown, "2026-10"), 0.5);
        let same = advance(Some(&grown), 3.0, t);
        assert_eq!(spent(&same, "2026-10"), 0.5);
        let reset = advance(Some(&same), 0.25, t);
        assert_eq!(spent(&reset, "2026-10"), 0.75);
        assert_eq!(reset.last_total, 0.25);
        // Over the month boundary (UTC): the new month starts from zero.
        let next = advance(Some(&reset), 1.25, at("2026-11-01T00:30:00Z"));
        assert_eq!(spent(&next, "2026-10"), 0.75);
        assert_eq!(spent(&next, "2026-11"), 1.0);
    }

    #[test]
    fn session_ids_are_plain_file_names() {
        for ok in ["abc", "0b9c-1F_2.x", &"a".repeat(128)] {
            assert!(valid_session_id(ok), "{ok}");
        }
        for bad in ["", ".hidden", "../x", "a/b", "a b", "é", &"a".repeat(129)] {
            assert!(!valid_session_id(bad), "{bad}");
        }
    }

    /// T6: concurrent sessions keep separate ledgers that add up; bad
    /// input writes nothing; month to date sums the current month only.
    #[test]
    fn sessions_add_up_by_month() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let now = at("2026-11-10T12:00:00Z");
        assert_eq!(month_to_date(&paths, "api", now), None);
        let status =
            |id: &str, total: f64| json!({"session_id": id, "cost": {"total_cost_usd": total}});
        record(&paths, "api", &status("s1", 0.0), now);
        record(&paths, "api", &status("s2", 0.0), now);
        assert_eq!(month_to_date(&paths, "api", now), Some(0.0));
        record(&paths, "api", &status("s1", 1.5), now);
        record(&paths, "api", &status("s2", 0.5), now);
        record(&paths, "api", &status("s1", 2.0), now);
        assert_eq!(month_to_date(&paths, "api", now), Some(2.5));
        // A later month doesn't count this one's.
        assert_eq!(
            month_to_date(&paths, "api", at("2026-12-01T00:00:00Z")),
            Some(0.0)
        );
        for bad in [
            json!({"session_id": "../evil", "cost": {"total_cost_usd": 9.0}}),
            json!({"session_id": "s3", "cost": {"total_cost_usd": -1.0}}),
            json!({"session_id": "s3", "cost": {"total_cost_usd": "9"}}),
            json!({"session_id": "s3"}),
            json!({"cost": {"total_cost_usd": 9.0}}),
        ] {
            record(&paths, "api", &bad, now);
        }
        let names: Vec<String> = std::fs::read_dir(dir(&paths, "api"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // Nothing beside the ledgers either (`../evil` would land there).
        let state: Vec<String> = std::fs::read_dir(paths.profile_state_dir("api"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(state, [DIR], "{state:?}");
        assert_eq!(month_to_date(&paths, "api", now), Some(2.5));
    }

    #[test]
    fn a_new_session_prunes_ledgers_left_for_two_months() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let now = Utc::now();
        let status = |id: &str| json!({"session_id": id, "cost": {"total_cost_usd": 0.0}});
        record(&paths, "api", &status("old"), now);
        let old = dir(&paths, "api").join("old.json");
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(std::time::SystemTime::from(
            now - chrono::Duration::days(63),
        ))
        .unwrap();
        record(&paths, "api", &status("new"), now);
        assert!(!old.exists());
        assert!(dir(&paths, "api").join("new.json").exists());
    }
}
