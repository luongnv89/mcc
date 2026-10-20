//! A shared undo log for multi-step commands.
//!
//! A command records one undo step after each change it makes. If a later
//! step fails, [`Transaction::rollback`] undoes the recorded steps in reverse
//! order; once everything succeeded, [`Transaction::commit`] disarms it.
//!
//! The steps are plain data (each command keeps its own `Undo` enum), and the
//! caller passes the code that undoes one step to `rollback`. That way an undo
//! step can use state the command still borrows while it runs, such as the
//! `Config` that `add` saves.

use anyhow::Result;

use crate::ui;

/// How a rollback reports itself on stderr.
pub struct Messages {
    /// Printed once before the first step, if any step was recorded.
    pub header: Option<&'static str>,
    /// Prefix for a step that was undone (`"{done} {what}"`); `None` is quiet.
    pub done: Option<&'static str>,
    /// Prefix for a step that could not be undone (`"{failed} {what}: {err}"`).
    pub failed: &'static str,
}

/// Rollback of things created: `removed …` / `could not remove …`.
pub const REMOVE: Messages = Messages {
    header: Some("Rolling back changes…"),
    done: Some("removed"),
    failed: "could not remove",
};

/// Rollback of things replaced: `restored …` / `could not restore …`.
pub const RESTORE: Messages = Messages {
    header: Some("Rolling back changes…"),
    done: Some("restored"),
    failed: "could not restore",
};

/// A step that could not be undone. Commands rely on the warning `rollback`
/// prints; the list is for callers that need to act on failures.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Failure {
    /// What the step undoes, as shown to the user.
    pub what: String,
    pub error: anyhow::Error,
}

/// Undo steps recorded while a command makes its changes.
pub struct Transaction<U> {
    steps: Vec<U>,
}

impl<U> Default for Transaction<U> {
    fn default() -> Self {
        Self::new()
    }
}

impl<U> Transaction<U> {
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Record how to undo a change that was just made.
    pub fn record(&mut self, step: U) {
        self.steps.push(step);
    }

    /// Keep every change: forget the recorded steps, so a later `rollback`
    /// does nothing.
    pub fn commit(&mut self) {
        self.steps.clear();
    }

    /// Undo every recorded step, the most recent first. `undo` undoes one
    /// step and returns what it was (for the messages) with its result. A
    /// step that fails is reported and the rest still run. Returns the
    /// failures, in the order they happened; the transaction is empty after.
    pub fn rollback(
        &mut self,
        messages: &Messages,
        mut undo: impl FnMut(U) -> (String, Result<()>),
    ) -> Vec<Failure> {
        let mut failures = Vec::new();
        if self.steps.is_empty() {
            return failures;
        }
        if let Some(header) = messages.header {
            ui::warn(header);
        }
        while let Some(step) = self.steps.pop() {
            let (what, res) = undo(step);
            match res {
                Ok(()) => {
                    if let Some(done) = messages.done {
                        ui::info(&format!("{done} {what}"));
                    }
                }
                Err(error) => {
                    ui::warn(&format!("{} {what}: {error:#}", messages.failed));
                    failures.push(Failure { what, error });
                }
            }
        }
        failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: Messages = Messages {
        header: None,
        done: None,
        failed: "could not undo",
    };

    fn ok(n: u32, seen: &mut Vec<u32>) -> (String, Result<()>) {
        seen.push(n);
        (format!("step {n}"), Ok(()))
    }

    #[test]
    fn rolls_back_in_reverse_order() {
        let mut txn = Transaction::new();
        for n in 1..=4 {
            txn.record(n);
        }
        let mut seen = Vec::new();
        let failures = txn.rollback(&QUIET, |n| ok(n, &mut seen));
        assert_eq!(seen, [4, 3, 2, 1]);
        assert!(failures.is_empty());
    }

    #[test]
    fn rollback_runs_each_step_once() {
        let mut txn = Transaction::new();
        txn.record(1);
        let mut seen = Vec::new();
        txn.rollback(&REMOVE, |n| ok(n, &mut seen));
        txn.rollback(&REMOVE, |n| ok(n, &mut seen));
        assert_eq!(seen, [1]);
    }

    #[test]
    fn commit_disarms_the_rollback() {
        let mut txn = Transaction::new();
        txn.record(1);
        txn.record(2);
        txn.commit();
        let mut seen = Vec::new();
        let failures = txn.rollback(&REMOVE, |n| ok(n, &mut seen));
        assert!(seen.is_empty(), "nothing undone after commit: {seen:?}");
        assert!(failures.is_empty());
    }

    #[test]
    fn steps_recorded_after_commit_still_roll_back() {
        let mut txn = Transaction::new();
        txn.record(1);
        txn.commit();
        txn.record(2);
        let mut seen = Vec::new();
        txn.rollback(&QUIET, |n| ok(n, &mut seen));
        assert_eq!(seen, [2]);
    }

    #[test]
    fn a_failing_step_is_reported_and_the_rest_still_run() {
        let mut txn = Transaction::new();
        for n in 1..=4 {
            txn.record(n);
        }
        let mut seen = Vec::new();
        let failures = txn.rollback(&RESTORE, |n| {
            seen.push(n);
            let res = if n == 3 || n == 1 {
                Err(anyhow::anyhow!("boom {n}"))
            } else {
                Ok(())
            };
            (format!("step {n}"), res)
        });
        assert_eq!(seen, [4, 3, 2, 1], "a failure must not stop later steps");
        let reported: Vec<(String, String)> = failures
            .iter()
            .map(|f| (f.what.clone(), f.error.to_string()))
            .collect();
        assert_eq!(
            reported,
            [
                ("step 3".to_string(), "boom 3".to_string()),
                ("step 1".to_string(), "boom 1".to_string()),
            ]
        );
    }

    #[test]
    fn an_empty_rollback_does_nothing() {
        let mut txn: Transaction<u32> = Transaction::default();
        let failures = txn.rollback(&REMOVE, |_| unreachable!("no steps recorded"));
        assert!(failures.is_empty());
    }
}
