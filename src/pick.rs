//! Whether the profile `mcc run` would start is at its limit, and how to say
//! so. Unknown usage (offline, expired login, `MCC_NO_USAGE`) never counts as
//! being at the limit.

use chrono::{DateTime, Utc};

use crate::usage::{self, Reason, Status, Usage, Window};

/// A window at or above this percent is at its limit.
const LIMIT: f64 = 100.0;

/// Known to be at the 5-hour or weekly limit.
pub fn at_limit(u: &Usage) -> bool {
    u.status == Status::Ok
        && [&u.five_hour, &u.weekly]
            .into_iter()
            .flatten()
            .any(|w| w.percent >= LIMIT)
}

/// "5h 42%, week 18% used", or why usage is unknown.
pub fn summary(u: &Usage) -> String {
    match (u.status, u.reason) {
        (Status::Ok, _) => {
            let pct =
                |w: &Option<Window>| w.as_ref().map_or("—".into(), |w| usage::percent(w.percent));
            format!("5h {}, week {} used", pct(&u.five_hour), pct(&u.weekly))
        }
        (_, reason) => format!(
            "usage unknown: {}",
            reason.map_or("unknown", Reason::describe)
        ),
    }
}

/// The windows at their limit, by name.
fn full(u: &Usage) -> Vec<(&'static str, &Window)> {
    [("5-hour", &u.five_hour), ("weekly", &u.weekly)]
        .into_iter()
        .filter_map(|(name, w)| Some((name, w.as_ref().filter(|w| w.percent >= LIMIT)?)))
        .collect()
}

/// How long until a profile at its limit is usable again, once its last full
/// window resets: "35m", or "now" once that time has passed. `None` when no
/// reset time is known.
pub fn reset_time(u: &Usage, now: DateTime<Utc>) -> Option<String> {
    full(u)
        .iter()
        .filter_map(|(_, w)| w.resets_at)
        .max()
        .map(|t| usage::until(t, now))
}

/// "resets in 35m", or "should reset any moment" (never "resets in now").
fn resets(reset: &str) -> String {
    match reset {
        "now" => "should reset any moment".into(),
        left => format!("resets in {left}"),
    }
}

/// When `profile` is usable again, from [`reset_time`]: "`work` resets in
/// 35m", "`work` should reset any moment", "`work` (reset time unknown)".
pub fn reset_note(profile: &str, reset: Option<&str>) -> String {
    match reset {
        Some(t) => format!("`{profile}` {}", resets(t)),
        None => format!("`{profile}` (reset time unknown)"),
    }
}

/// The at-limit prompt's default choice: "Wait — `work` resets in 35m".
pub fn wait_label(profile: &str, reset: Option<&str>) -> String {
    format!("Wait — {}", reset_note(profile, reset))
}

/// Which limit a profile at its limit has reached, and when it lifts:
/// "its 5-hour limit, resets in 35m", "its 5-hour and weekly limits, …".
pub fn limit_note(u: &Usage, now: DateTime<Utc>) -> String {
    let names: Vec<&str> = full(u).iter().map(|(n, _)| *n).collect();
    let what = match names.as_slice() {
        [one] => format!("its {one} limit"),
        _ => format!("its {} limits", names.join(" and ")),
    };
    match reset_time(u, now) {
        Some(t) => format!("{what}, {}", resets(&t)),
        None => what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(five: Option<f64>, week: Option<f64>) -> Usage {
        let w = |p: Option<f64>| {
            p.map(|percent| Window {
                percent,
                resets_at: None,
            })
        };
        Usage {
            status: Status::Ok,
            reason: None,
            five_hour: w(five),
            weekly: w(week),
        }
    }

    #[test]
    fn at_limit_needs_a_full_window() {
        assert!(at_limit(&ok(Some(100.0), Some(18.0))));
        assert!(at_limit(&ok(Some(3.0), Some(100.0))));
        assert!(at_limit(&ok(None, Some(100.5))));
        assert!(!at_limit(&ok(Some(99.9), Some(18.0))));
        assert!(!at_limit(&ok(None, None)));
        // Unknown usage is never at the limit.
        for r in [
            Reason::NotLoggedIn,
            Reason::TokenExpired,
            Reason::Network,
            Reason::Disabled,
        ] {
            assert!(!at_limit(&Usage::unavailable(r)));
        }
    }

    #[test]
    fn summaries_and_limit_notes() {
        assert_eq!(
            summary(&ok(Some(42.4), Some(18.0))),
            "5h 42%, week 18% used"
        );
        assert_eq!(summary(&ok(None, Some(18.0))), "5h —, week 18% used");
        assert_eq!(
            summary(&Usage::unavailable(Reason::NotLoggedIn)),
            "usage unknown: not logged in"
        );

        let now = DateTime::parse_from_rfc3339("2026-10-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let at = |secs| Some(now + chrono::Duration::seconds(secs));
        let mut u = ok(Some(100.0), Some(40.0));
        u.five_hour.as_mut().unwrap().resets_at = at(35 * 60);
        u.weekly.as_mut().unwrap().resets_at = at(3 * 86400);
        assert_eq!(limit_note(&u, now), "its 5-hour limit, resets in 35m");
        u.weekly.as_mut().unwrap().percent = 100.0;
        assert_eq!(
            limit_note(&u, now),
            "its 5-hour and weekly limits, resets in 3d 0h"
        );
        assert_eq!(limit_note(&ok(None, Some(100.0)), now), "its weekly limit");
    }

    #[test]
    fn reset_time_is_the_last_full_window_and_never_says_resets_in_now() {
        let now = DateTime::parse_from_rfc3339("2026-10-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let at = |secs| Some(now + chrono::Duration::seconds(secs));
        let mut u = ok(Some(100.0), Some(40.0));
        u.five_hour.as_mut().unwrap().resets_at = at(35 * 60);
        u.weekly.as_mut().unwrap().resets_at = at(3 * 86400);
        // Only full windows count: the weekly one isn't.
        assert_eq!(reset_time(&u, now).as_deref(), Some("35m"));
        assert_eq!(
            wait_label("work", reset_time(&u, now).as_deref()),
            "Wait — `work` resets in 35m"
        );
        u.weekly.as_mut().unwrap().percent = 100.0;
        assert_eq!(reset_time(&u, now).as_deref(), Some("3d 0h"));

        // No reset time known.
        let unknown = ok(Some(100.0), None);
        assert_eq!(reset_time(&unknown, now), None);
        assert_eq!(
            wait_label("work", None),
            "Wait — `work` (reset time unknown)"
        );

        // Already past: never "resets in now".
        let mut past = ok(Some(100.0), None);
        past.five_hour.as_mut().unwrap().resets_at = at(-60);
        assert_eq!(reset_time(&past, now).as_deref(), Some("now"));
        let label = wait_label("work", reset_time(&past, now).as_deref());
        assert_eq!(label, "Wait — `work` should reset any moment");
        let note = limit_note(&past, now);
        assert_eq!(note, "its 5-hour limit, should reset any moment");
        assert!(!label.contains("in now") && !note.contains("in now"));
    }
}
