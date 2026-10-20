//! `mcc share` — opt a profile in to (or out of) session sharing.
//!
//! Sharing replaces the profile's `projects/` (where Claude Code keeps the
//! sessions `--resume` lists) with a symlink to the store of its sharing
//! group: `~/.mcc/shared/groups/<name>/projects` for a named group
//! (`--group`), `~/.mcc/shared/projects` for the default group (the one
//! store every sharing profile used before groups existed, so profiles that
//! already shared keep their sessions as they are). Nothing else is linked:
//! credentials, the account (`.claude.json`), prompt history, live session
//! state and shell snapshots stay per profile.
//!
//! Joining a group whose profiles are logged in to another organization is
//! refused unless `--allow-cross-org` is passed, and organization ↔ personal
//! (or an account mcc can't identify) also needs a confirmation in a
//! terminal: work transcripts resumed under a personal account fall under
//! consumer terms.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;

use crate::cli::ShareArgs;
use crate::config::{Account, AccountKind, Config, OrgMatch, Profile};
use crate::error::CliError;
use crate::paths::Paths;
use crate::txn::{self, Transaction};
use crate::{fsutil, metrics, shell, ui};

/// Printed verbatim on stderr the first time sharing is turned on for a
/// profile (when `share_note_acknowledged_at` is not set yet).
pub const SHARE_TOS_NOTE: &str = crate::cli::share_tos_note!();

/// Printed on stderr instead of the note on every later `mcc share` and
/// `--mcc-share`.
pub const SHARE_TOS_REMINDER: &str = crate::cli::share_tos_reminder!();

/// What a profile's `projects/` currently is.
enum Projects {
    /// A symlink to the shared store: already sharing.
    Shared,
    /// A symlink somewhere else: never touched.
    Foreign(PathBuf),
    Dir,
    /// A file or other non-directory: never touched.
    Other,
    Missing,
}

fn projects_state(projects: &Path, store: &Path) -> Result<Projects> {
    match std::fs::symlink_metadata(projects) {
        Ok(m) if m.file_type().is_symlink() => {
            let target = std::fs::read_link(projects)
                .with_context(|| format!("reading {}", projects.display()))?;
            // The exact target mcc writes; anything else is someone else's link.
            Ok(if target == store {
                Projects::Shared
            } else {
                Projects::Foreign(target)
            })
        }
        Ok(m) if m.is_dir() => Ok(Projects::Dir),
        Ok(_) => Ok(Projects::Other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Projects::Missing),
        Err(e) => Err(e).with_context(|| format!("reading {}", projects.display())),
    }
}

/// A filesystem change made while enabling, undone in reverse order on failure.
enum Undo {
    /// `from` was renamed to `to`; renamed back.
    Moved { from: PathBuf, to: PathBuf },
    /// A symlink mcc created; removed.
    Link(PathBuf),
}

/// Rollback of a share: quiet unless a step fails.
const MESSAGES: txn::Messages = txn::Messages {
    header: None,
    done: None,
    failed: "rollback: could not",
};

fn rollback(paths: &Paths, undo: &mut Transaction<Undo>) {
    undo.rollback(&MESSAGES, |u| match u {
        Undo::Moved { from, to } => (
            format!("move {} back", paths.display(&to)),
            from.parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::rename(&to, &from))
                .map_err(anyhow::Error::from),
        ),
        Undo::Link(p) => (
            format!("remove {}", paths.display(&p)),
            fsutil::remove_any(&p),
        ),
    });
}

/// How merging a profile's existing sessions into the store went.
#[derive(Default)]
struct Merge {
    /// Files and directories moved into the store.
    moved: usize,
    /// Files whose name was taken in the store by different contents: the
    /// store's copy is kept, the profile's goes to `backup`.
    conflicts: usize,
    backup: PathBuf,
    /// Staging directories left by an interrupted `mcc share`, merged into
    /// the store, with the number of items each one gave.
    recovered: Vec<(PathBuf, usize)>,
}

pub fn run(paths: &Paths, cfg: &mut Config, args: &ShareArgs) -> Result<()> {
    let alias = args.alias.as_str();
    let profile = sharing_profile(paths, cfg, alias)?;
    if args.off {
        disable(paths, cfg, alias, &profile)
    } else {
        let group = match &args.group {
            Some(name) => check_group(name)?,
            None => profile.share_group.clone(),
        };
        enable(paths, cfg, alias, &profile, &group, args)
    }
}

/// Longest group name accepted, in characters.
const GROUP_MAX: usize = 64;

/// A group name is 1–64 ASCII letters, digits, `-`, `_` or `.`, starting
/// with a letter or digit, so it is a plain directory name. It is
/// lowercased: on a case-insensitive filesystem (macOS by default) `Acme`
/// and `acme` would be one store, so they must be one group. `default` is
/// the default group (`None`).
pub(crate) fn check_group(name: &str) -> Result<Option<String>> {
    let lower = name.to_ascii_lowercase();
    if lower == "default" {
        return Ok(None);
    }
    let ok = name.len() <= GROUP_MAX
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok {
        Ok(Some(lower))
    } else {
        Err(CliError::usage(format!("invalid group name \"{name}\""))
            .hint(format!(
                "use 1–{GROUP_MAX} letters, digits, `-`, `_` or `.`, starting with a letter or digit"
            ))
            .into())
    }
}

/// The store of `group`; never a path outside `~/.mcc/shared`, even for a
/// group that skipped [`check_group`].
fn store_of(paths: &Paths, group: &Option<String>) -> Result<PathBuf> {
    paths.share_store(group.as_deref()).ok_or_else(|| {
        CliError::input(format!("invalid group name \"{}\"", group_name(group))).into()
    })
}

/// The same group, ignoring case (see [`check_group`]).
///
/// Both sides are normalized like `--group` (`default` and `Default` are
/// the default group), so a hand-edited `share_group` counts in the group
/// whose store it resolves to. A name [`check_group`] rejects never gets a
/// store, but it counts as the same group: the cross-organization guard
/// then weighs that profile too instead of skipping it (fail safe).
fn same_group(a: &Option<String>, b: &Option<String>) -> bool {
    let norm = |g: &Option<String>| g.as_deref().map_or(Ok(None), check_group);
    match (norm(a), norm(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => true,
    }
}

/// A group as people read it.
fn group_name(group: &Option<String>) -> &str {
    group.as_deref().unwrap_or("default")
}

/// A profile's account, for the cross-organization guard.
#[derive(Debug, Clone)]
struct OrgId {
    account: Account,
    kind: AccountKind,
}

impl OrgId {
    fn of(account: &Account) -> Self {
        OrgId {
            kind: account.kind(),
            account: account.clone(),
        }
    }

    fn describe(&self) -> String {
        let org = || {
            self.account
                .organization
                .as_deref()
                .or(self.account.organization_uuid.as_deref())
                .unwrap_or("?")
        };
        match self.kind {
            AccountKind::Organization => format!("organization {}", org()),
            AccountKind::Personal => "a personal account".into(),
            AccountKind::Unclassified => format!("organization {} of unknown type", org()),
            AccountKind::Unknown => "an unknown account".into(),
        }
    }
}

/// How much sharing sessions between two profiles risks, least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Risk {
    None,
    /// Neither profile is in a work organization, but at least one account
    /// is unknown, so they can't be told to be the same kind.
    Unknown,
    /// Two different work organizations.
    CrossOrg,
    /// A work organization and a personal (or unknown) account: work
    /// sessions would become resumable under consumer terms. The strictest
    /// case, also used whenever an organization of unknown type is involved.
    OrgToPersonal,
}

fn pair_risk(a: &OrgId, b: &OrgId) -> Risk {
    use AccountKind::*;
    if a.account.has_org()
        && b.account.has_org()
        && a.account.org_match(&b.account) == OrgMatch::Same
    {
        return Risk::None;
    }
    match (a.kind, b.kind) {
        (Personal, Personal) => Risk::None,
        (Organization, Organization) => Risk::CrossOrg,
        (Unknown, Personal | Unknown) | (Personal, Unknown) => Risk::Unknown,
        // A work organization, or one that may be, with anything else.
        _ => Risk::OrgToPersonal,
    }
}

/// The other sharing profiles of `group` that `alias` would share with at
/// some risk, each with what it is; and `alias`'s own [`OrgId`].
fn group_risks(
    cfg: &Config,
    alias: &str,
    profile: &Profile,
    group: &Option<String>,
) -> (OrgId, Vec<(String, OrgId, Risk)>) {
    let me = OrgId::of(&metrics::profile_account(profile));
    let others = cfg
        .profiles
        .iter()
        .filter(|(name, p)| {
            name.as_str() != alias && p.share_sessions && same_group(&p.share_group, group)
        })
        .map(|(name, p)| {
            let id = OrgId::of(&metrics::profile_account(p));
            let risk = pair_risk(&me, &id);
            (name.clone(), id, risk)
        })
        .filter(|(_, _, r)| *r > Risk::None)
        .collect();
    (me, others)
}

/// What to do about a risk, from `--allow-cross-org` and whether a terminal
/// can confirm.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Proceed,
    /// Allowed by `--allow-cross-org`: proceed and record it.
    Allowed,
    /// Allowed by `--allow-cross-org` once confirmed in the terminal.
    Confirm,
    Refuse,
}

fn verdict(risk: Risk, allow: bool, interactive: bool) -> Verdict {
    match risk {
        Risk::None => Verdict::Proceed,
        _ if !allow => Verdict::Refuse,
        Risk::OrgToPersonal if interactive => Verdict::Confirm,
        Risk::OrgToPersonal => Verdict::Refuse,
        Risk::Unknown | Risk::CrossOrg => Verdict::Allowed,
    }
}

/// The profiles at risk, as `"lab" (organization Acme), …`.
fn risk_list(others: &[(String, OrgId, Risk)]) -> String {
    others
        .iter()
        .map(|(n, id, _)| format!("\"{n}\" ({})", id.describe()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why sharing `alias` (which is `me`) with `others` is refused.
fn refusal(
    alias: &str,
    me: &OrgId,
    group: &Option<String>,
    others: &[(String, OrgId, Risk)],
    risk: Risk,
    allow: bool,
) -> anyhow::Error {
    let list = risk_list(others);
    let who = format!("profile \"{alias}\" ({})", me.describe());
    let why = match risk {
        Risk::OrgToPersonal => {
            "sessions would flow between an organization account and a personal (or unidentified) \
             one, and work transcripts resumed under a personal account fall under consumer terms"
        }
        Risk::CrossOrg => "they belong to different organizations",
        _ => "mcc can't tell which organization every account belongs to",
    };
    let msg = format!(
        "refusing to share the sessions of {who} with {list} in group \"{}\": {why}",
        group_name(group)
    );
    let separate = format!("keep them apart with `mcc share {alias} --group <name>`");
    let hint = match risk {
        Risk::OrgToPersonal if allow => format!(
            "organization ↔ personal sharing needs a confirmation in a terminal: run `mcc share {alias} --allow-cross-org` there; or {separate}"
        ),
        Risk::OrgToPersonal => format!(
            "{separate}; to share anyway, run `mcc share {alias} --allow-cross-org` in a terminal and confirm"
        ),
        Risk::Unknown => format!(
            "log the profiles in (`claude auth login`) so mcc can see their accounts, {separate}, or pass --allow-cross-org"
        ),
        _ => format!("{separate}, or share anyway with --allow-cross-org"),
    };
    CliError::input(msg).hint(hint).into()
}

/// Apply the cross-organization guard to `alias` joining (or, `already`,
/// staying in) `group`. Returns whether the allowance is to be recorded.
/// An already-sharing profile that never confirmed (it shared before the
/// guard existed) is only warned, so nothing it shares is cut off.
fn guard(
    alias: &str,
    profile: &Profile,
    cfg: &Config,
    group: &Option<String>,
    already: bool,
    allow: bool,
) -> Result<bool> {
    let (me, others) = group_risks(cfg, alias, profile, group);
    let Some(risk) = others.iter().map(|(_, _, r)| *r).max() else {
        return Ok(false);
    };
    if already && !allow {
        if profile.share_cross_org_allowed_at.is_none() {
            ui::warn(&format!(
                "profile \"{alias}\" ({}) shares its sessions with {} in group \"{}\", across organizations",
                me.describe(),
                risk_list(&others),
                group_name(group)
            ));
            ui::hint(&format!(
                "stop with `mcc share {alias} --off` and share within a group of its own (`--group <name>`), or keep it with `mcc share {alias} --allow-cross-org`"
            ));
        }
        return Ok(false);
    }
    match verdict(risk, allow, ui::is_interactive()) {
        Verdict::Proceed => Ok(false),
        Verdict::Allowed => {
            ui::warn(&format!(
                "sharing across organizations with {} (--allow-cross-org)",
                risk_list(&others)
            ));
            Ok(true)
        }
        Verdict::Confirm => {
            ui::warn(&format!(
                "profile \"{alias}\" ({}) and {} would share sessions: work transcripts \
                 resumed under a personal account fall under consumer terms, where they may be used for training",
                me.describe(),
                risk_list(&others)
            ));
            let hint = format!("run `mcc share {alias} --allow-cross-org` in a terminal");
            if ui::confirm(
                "Share sessions between an organization and a personal account?",
                false,
                &hint,
            )? {
                Ok(true)
            } else {
                Err(refusal(alias, &me, group, &others, risk, false))
            }
        }
        Verdict::Refuse => Err(refusal(alias, &me, group, &others, risk, allow)),
    }
}

/// The registered profile `alias`, if it can share at all.
fn sharing_profile(paths: &Paths, cfg: &Config, alias: &str) -> Result<Profile> {
    if matches!(alias, "default" | "claude") {
        return Err(
            CliError::input("the default profile (~/.claude) can't share sessions")
                .hint("session sharing is for mcc profiles; see `mcc list`")
                .into(),
        );
    }
    let Some(mut profile) = cfg.profiles.get(alias).cloned() else {
        return Err(super::unknown_profile(alias));
    };
    let fix = format!(
        "fix the [profiles.{alias}] entry in {}",
        paths.display(&paths.config_file)
    );
    if !super::delete::is_profile_dir(paths, &profile.dir) {
        return Err(CliError::input(format!(
            "refusing to share {}: it is not a profile directory inside {}",
            paths.display(&profile.dir),
            paths.display(&paths.profiles_dir)
        ))
        .hint(fix)
        .into());
    }
    if !profile.dir.is_dir() {
        return Err(CliError::input(format!(
            "the directory of profile \"{alias}\" ({}) is missing",
            paths.display(&profile.dir)
        ))
        .hint(fix)
        .into());
    }
    // The stored group becomes a path: hold it to the `--group` rules
    // before anything on disk changes (no fallback to another store, which
    // could merge sessions somewhere unintended).
    if let Some(stored) = profile.share_group.take() {
        profile.share_group = check_group(&stored).map_err(|_| {
            anyhow::Error::from(
                CliError::input(format!(
                    "invalid share_group \"{stored}\" for profile \"{alias}\" in {}",
                    paths.display(&paths.config_file)
                ))
                .hint(format!(
                    "fix `share_group` in the [profiles.{alias}] entry: use 1–{GROUP_MAX} letters, digits, `-`, `_` or `.`, starting with a letter or digit, or remove it for the default group"
                )),
            )
        })?;
    }
    Ok(profile)
}

fn enable(
    paths: &Paths,
    cfg: &mut Config,
    alias: &str,
    profile: &Profile,
    group: &Option<String>,
    args: &ShareArgs,
) -> Result<()> {
    let from_launch = args.from_launch;
    if profile.share_sessions && !same_group(&profile.share_group, group) {
        return Err(CliError::input(format!(
            "profile \"{alias}\" already shares its sessions in group \"{}\", not \"{}\"",
            group_name(&profile.share_group),
            group_name(group)
        ))
        .hint(format!(
            "stop sharing first with `mcc share {alias} --off`, then run `mcc share {alias} --group {}`",
            group_name(group)
        ))
        .into());
    }
    let store = store_of(paths, group)?;
    let projects = profile.dir.join("projects");
    let state = projects_state(&projects, &store)?;
    let retry = format!("move it out of the way, then run `mcc share {alias}` again");
    match &state {
        Projects::Foreign(target) => {
            return Err(CliError::input(format!(
                "{} is a symlink to {}, not to the shared store {}",
                paths.display(&projects),
                target.display(),
                paths.display(&store)
            ))
            .hint(retry)
            .into());
        }
        Projects::Other => {
            return Err(CliError::input(format!(
                "{} is not a directory",
                paths.display(&projects)
            ))
            .hint(retry)
            .into());
        }
        _ => {}
    }
    let already = matches!(state, Projects::Shared);
    // Before anything moves: a refusal changes nothing.
    let allowed = guard(alias, profile, cfg, group, already, args.allow_cross_org)?;
    // The full note until it was shown once for this profile.
    let full_note = profile.share_note_acknowledged_at.is_none();
    if !from_launch {
        ui::header(&format!("Share the sessions of profile \"{alias}\""));
    }

    // The profile's own sessions wait in `staged` until everything else,
    // the registry included, succeeded.
    let staged = profile
        .dir
        .join(format!(".projects.mcc-share-{}", std::process::id()));
    let mut undo: Transaction<Undo> = Transaction::new();
    let result = (|| -> Result<Merge> {
        let mut merge = Merge {
            backup: paths.backups_dir.join(format!(
                "shared-projects-{alias}-{}",
                chrono::Local::now().format("%Y%m%d-%H%M%S%.3f")
            )),
            ..Merge::default()
        };
        // Created even when already shared: the link dangles if the store
        // was deleted by hand.
        create_store(paths, &store)?;
        // Before staging, so this run's own staging directory isn't listed.
        for dir in leftover_stagings(&profile.dir)? {
            let mut rec = Merge::default();
            let name = dir.file_name().expect("listed in the profile directory");
            let backup = merge.backup.join(name);
            merge_into(&dir, &store, &backup, &mut undo, &mut rec)?;
            merge.conflicts += rec.conflicts;
            merge.recovered.push((dir, rec.moved + rec.conflicts));
        }
        if !already {
            if matches!(state, Projects::Dir) {
                rename(&projects, &staged)?;
                undo.record(Undo::Moved {
                    from: projects.clone(),
                    to: staged.clone(),
                });
            }
            std::os::unix::fs::symlink(&store, &projects)
                .with_context(|| format!("linking {}", projects.display()))?;
            undo.record(Undo::Link(projects.clone()));
            if matches!(state, Projects::Dir) {
                let backup = merge.backup.clone();
                merge_into(&staged, &store, &backup, &mut undo, &mut merge)?;
            }
        }
        update_profile(paths, cfg, alias, |p| {
            p.share_sessions = true;
            p.share_group = group.clone();
            if p.share_note_acknowledged_at.is_none() {
                p.share_note_acknowledged_at = Some(now());
            }
            if allowed {
                p.share_cross_org_allowed_at = Some(now());
            }
        })?;
        Ok(merge)
    })();
    let merge = match result {
        Ok(m) => m,
        Err(e) => {
            rollback(paths, &mut undo);
            return Err(e);
        }
    };
    undo.commit();
    if !already
        && matches!(state, Projects::Dir)
        && let Err(e) = fsutil::remove_any(&staged)
    {
        ui::warn(&format!(
            "could not remove {} (left over, safe to delete): {e:#}",
            paths.display(&staged)
        ));
    }
    for (dir, items) in &merge.recovered {
        ui::info(&format!(
            "recovered {items} item(s) from an interrupted `mcc share` in {}",
            paths.display(dir)
        ));
        if let Err(e) = fsutil::remove_any(dir) {
            ui::warn(&format!(
                "could not remove {} (left over, safe to delete): {e:#}",
                paths.display(dir)
            ));
        }
    }
    if merge.conflicts > 0 {
        ui::warn(&format!(
            "{} session file(s) already existed in the shared store with different contents: \
             kept the store's copy, this profile's copy is in {}",
            merge.conflicts,
            paths.display(&merge.backup)
        ));
    }

    if !from_launch {
        if already {
            ui::info(&format!(
                "already shared: {} links to {}",
                paths.display(&projects),
                paths.display(&store)
            ));
        } else {
            ui::success(&format!(
                "linked {} → {}{}",
                paths.display(&projects),
                paths.display(&store),
                if merge.moved > 0 {
                    format!(" ({} existing item(s) moved into the store)", merge.moved)
                } else {
                    String::new()
                }
            ));
        }
    }
    eprintln!(
        "{}",
        if full_note {
            SHARE_TOS_NOTE
        } else {
            SHARE_TOS_REMINDER
        }
    );
    if from_launch {
        return Ok(());
    }
    ui::result(
        true,
        &format!(
            "profile \"{alias}\" shares its sessions with the other sharing profiles of group \"{}\"",
            group_name(group)
        ),
    );
    ui::kv("Group", group_name(group));
    ui::kv("Shared store", &paths.display(&store));
    ui::kv(
        "Not shared",
        "credentials, account (.claude.json), history, live sessions, shell snapshots",
    );
    match &profile.wrapper {
        Some(w) if shell::is_mcc_wrapper(w) && !shell::launcher_is_current(w) => {
            ui::hint(&format!(
                "run `mcc refresh-launchers {alias}` so `{alias} {}` works from your shell",
                shell::SHARE_FLAG
            ));
        }
        _ => ui::hint(&format!(
            "start it with `{alias}`; `{alias} --resume` lists every sharing profile's sessions"
        )),
    }
    Ok(())
}

fn disable(paths: &Paths, cfg: &mut Config, alias: &str, profile: &Profile) -> Result<()> {
    let store = store_of(paths, &profile.share_group)?;
    let projects = profile.dir.join("projects");
    ui::header(&format!("Stop sharing the sessions of profile \"{alias}\""));
    if !matches!(projects_state(&projects, &store)?, Projects::Shared) {
        update_profile(paths, cfg, alias, stop_sharing)?;
        ui::result(
            true,
            &format!("profile \"{alias}\" does not share its sessions — nothing to change"),
        );
        return Ok(());
    }

    // Copy next to the link first, so a failure leaves the link in place.
    let copy = profile
        .dir
        .join(format!(".projects.mcc-unshare-{}", std::process::id()));
    fsutil::remove_any(&copy)?;
    let files = match copy_tree(&store, &copy) {
        Ok(n) => n,
        Err(e) => {
            let _ = fsutil::remove_any(&copy);
            return Err(e);
        }
    };
    // Unlinks the symlink; the store itself is never followed.
    fsutil::remove_any(&projects)?;
    let relink = || {
        let _ = std::os::unix::fs::symlink(&store, &projects);
    };
    if let Err(e) = rename(&copy, &projects) {
        relink();
        let _ = fsutil::remove_any(&copy);
        return Err(e);
    }
    if let Err(e) = update_profile(paths, cfg, alias, stop_sharing) {
        let _ = fsutil::remove_any(&projects);
        relink();
        return Err(e);
    }
    ui::success(&format!(
        "{} is a directory of its own again, with a copy of the {files} shared session file(s)",
        paths.display(&projects)
    ));
    ui::result(
        true,
        &format!("profile \"{alias}\" no longer shares its sessions"),
    );
    ui::hint(&format!(
        "the store of group \"{}\" ({}) is kept for its other sharing profiles; delete it by hand once none is left",
        group_name(&profile.share_group),
        paths.display(&store)
    ));
    Ok(())
}

/// Not sharing: the flag and the cross-organization allowance go, the group
/// stays so the next `mcc share` (or `--mcc-share`) joins it again.
fn stop_sharing(p: &mut Profile) {
    p.share_sessions = false;
    p.share_cross_org_allowed_at = None;
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Change `alias`'s registry entry with `change` and save the config, only
/// when that changes something. On a failed save the entry is put back.
fn update_profile(
    paths: &Paths,
    cfg: &mut Config,
    alias: &str,
    change: impl FnOnce(&mut Profile),
) -> Result<()> {
    let p = cfg
        .profiles
        .get_mut(alias)
        .expect("checked by sharing_profile");
    let was = p.clone();
    change(p);
    if *p == was {
        return Ok(());
    }
    if let Err(e) = cfg.save(&paths.config_file) {
        cfg.profiles.insert(alias.to_string(), was);
        return Err(e);
    }
    Ok(())
}

/// The group's store, private to the user like the profiles, as is every
/// directory between it and `~/.mcc/shared`.
fn create_store(paths: &Paths, store: &Path) -> Result<()> {
    std::fs::create_dir_all(store).with_context(|| format!("creating {}", store.display()))?;
    let mut dir = Some(store);
    while let Some(d) = dir {
        fsutil::set_private_dir(d)?;
        if d == paths.shared_dir {
            break;
        }
        dir = d.parent().filter(|p| p.starts_with(&paths.shared_dir));
    }
    Ok(())
}

/// `.projects.mcc-share-<pid>` directories directly inside `profile_dir`:
/// what an `mcc share` interrupted mid-merge left behind. Symlinks are skipped.
fn leftover_stagings(profile_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(profile_dir)
        .with_context(|| format!("reading {}", profile_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(".projects.mcc-share-"))
        else {
            continue;
        };
        // `file_type` does not follow symlinks.
        if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) && entry.file_type()?.is_dir()
        {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

fn rename(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to)
        .with_context(|| format!("moving {} → {}", from.display(), to.display()))
}

fn is_real_dir(m: &std::fs::Metadata) -> bool {
    m.is_dir() && !m.file_type().is_symlink()
}

/// Move everything in `src` into `dst` without overwriting anything. Session
/// files are named by UUID, so a name taken in `dst` is usually the very same
/// file: identical files are dropped, different ones moved under `backup`.
fn merge_into(
    src: &Path,
    dst: &Path,
    backup: &Path,
    undo: &mut Transaction<Undo>,
    merge: &mut Merge,
) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(src)
        .with_context(|| format!("reading {}", src.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let (from, to) = (src.join(&name), dst.join(&name));
        let src_meta = std::fs::symlink_metadata(&from)?;
        match std::fs::symlink_metadata(&to) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                rename(&from, &to)?;
                undo.record(Undo::Moved { from, to });
                merge.moved += 1;
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", to.display())),
            Ok(dst_meta) if is_real_dir(&src_meta) && is_real_dir(&dst_meta) => {
                merge_into(&from, &to, &backup.join(&name), undo, merge)?;
            }
            Ok(_) if same_file(&from, &to) => {}
            Ok(_) => {
                std::fs::create_dir_all(backup)
                    .with_context(|| format!("creating {}", backup.display()))?;
                let kept = backup.join(&name);
                rename(&from, &kept)?;
                undo.record(Undo::Moved { from, to: kept });
                merge.conflicts += 1;
            }
        }
    }
    Ok(())
}

/// Two regular files with the same bytes. A file that cannot be read is never
/// "the same", so it takes the conflict path and gets backed up.
fn same_file(a: &Path, b: &Path) -> bool {
    let regular = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
    if !(regular(a) && regular(b)) {
        return false;
    }
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Copy the tree at `src` (if any) to a new directory `dst`, keeping symlinks
/// as symlinks. Returns the number of files copied.
fn copy_tree(src: &Path, dst: &Path) -> Result<usize> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    fsutil::set_private_dir(dst)?;
    if !src.is_dir() {
        return Ok(0);
    }
    let mut files = 0;
    for entry in WalkDir::new(src).follow_links(false).min_depth(1) {
        let entry = entry.with_context(|| format!("reading {}", src.display()))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("walk stays under src");
        let to = dst.join(rel);
        let ft = entry.file_type();
        if ft.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            std::os::unix::fs::symlink(target, &to)
                .with_context(|| format!("linking {}", to.display()))?;
        } else if ft.is_dir() {
            std::fs::create_dir_all(&to).with_context(|| format!("creating {}", to.display()))?;
        } else {
            std::fs::copy(entry.path(), &to).with_context(|| {
                format!("copying {} → {}", entry.path().display(), to.display())
            })?;
            files += 1;
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org(name: &str, id: Option<&str>, ty: Option<&str>) -> OrgId {
        OrgId::of(&Account {
            email: Some("me@x.com".into()),
            organization: Some(name.into()),
            organization_uuid: id.map(Into::into),
            organization_type: ty.map(Into::into),
            ..Account::default()
        })
    }

    fn personal() -> OrgId {
        OrgId::of(&Account {
            email: Some("me@x.com".into()),
            ..Account::default()
        })
    }

    #[test]
    fn pair_risk_ranks_org_to_personal_highest() {
        let unknown = OrgId::of(&Account::default());
        let team = Some("claude_team");
        let acme = org("Acme", Some("o1"), team);
        // A personal plan, shaped like Claude Code reports it: with an
        // organization of its own.
        let max = org("me's Organization", Some("o9"), Some("claude_max"));
        let pro = org("you's Organization", Some("o8"), Some("claude_pro"));
        assert_eq!(
            pair_risk(&acme, &org("Acme Inc", Some("o1"), team)),
            Risk::None
        );
        assert_eq!(pair_risk(&personal(), &personal()), Risk::None);
        assert_eq!(pair_risk(&max, &pro), Risk::None);
        assert_eq!(pair_risk(&max, &personal()), Risk::None);
        assert_eq!(
            pair_risk(&acme, &org("Acme", Some("o2"), team)),
            Risk::CrossOrg
        );
        assert_eq!(
            pair_risk(&acme, &org("Beta", None, Some("claude_enterprise"))),
            Risk::CrossOrg
        );
        // Either direction, and an unknown account counts as personal.
        assert_eq!(pair_risk(&acme, &max), Risk::OrgToPersonal);
        assert_eq!(pair_risk(&max, &acme), Risk::OrgToPersonal);
        assert_eq!(pair_risk(&acme, &personal()), Risk::OrgToPersonal);
        assert_eq!(pair_risk(&unknown, &acme), Risk::OrgToPersonal);
        assert_eq!(pair_risk(&unknown, &personal()), Risk::Unknown);
        assert_eq!(pair_risk(&unknown, &max), Risk::Unknown);
        assert_eq!(pair_risk(&unknown, &unknown), Risk::Unknown);
        // An organization of unknown type may be a work one: strictest,
        // unless it is the very same organization.
        let untyped = org("Gamma", Some("o3"), None);
        assert_eq!(pair_risk(&untyped, &max), Risk::OrgToPersonal);
        assert_eq!(pair_risk(&untyped, &acme), Risk::OrgToPersonal);
        assert_eq!(
            pair_risk(&untyped, &org("Delta", Some("o4"), None)),
            Risk::OrgToPersonal
        );
        assert_eq!(
            pair_risk(&untyped, &org("Gamma", Some("o3"), team)),
            Risk::None
        );
        assert!(Risk::OrgToPersonal > Risk::CrossOrg && Risk::CrossOrg > Risk::Unknown);
    }

    #[test]
    fn verdict_is_strictest_for_org_to_personal() {
        use Verdict::*;
        for interactive in [false, true] {
            assert_eq!(verdict(Risk::None, false, interactive), Proceed);
            for risk in [Risk::Unknown, Risk::CrossOrg, Risk::OrgToPersonal] {
                assert_eq!(verdict(risk, false, interactive), Refuse, "{risk:?}");
            }
            assert_eq!(verdict(Risk::CrossOrg, true, interactive), Allowed);
            assert_eq!(verdict(Risk::Unknown, true, interactive), Allowed);
        }
        // --allow-cross-org alone is not enough: a terminal must confirm.
        assert_eq!(verdict(Risk::OrgToPersonal, true, false), Refuse);
        assert_eq!(verdict(Risk::OrgToPersonal, true, true), Confirm);
    }

    #[test]
    fn group_names() {
        assert_eq!(check_group("default").unwrap(), None);
        for ok in ["work", "a.b_c", "9"] {
            assert_eq!(check_group(ok).unwrap().as_deref(), Some(ok));
        }
        // One group whatever the case, as on a case-insensitive filesystem.
        assert_eq!(check_group("Acme-1").unwrap().as_deref(), Some("acme-1"));
        assert_eq!(check_group("DEFAULT").unwrap(), None);
        assert!(same_group(&Some("Acme".into()), &Some("acme".into())));
        assert!(!same_group(&Some("acme".into()), &None));
        // Hand-edited spellings of the default group are the default group.
        assert!(same_group(&Some("Default".into()), &None));
        assert!(same_group(&None, &Some("default".into())));
        assert!(!same_group(&Some("default".into()), &Some("acme".into())));
        // An invalid stored name is never skipped by the guard.
        assert!(same_group(&Some("../..".into()), &None));
        assert!(same_group(&Some("a/b".into()), &Some("acme".into())));
        for bad in ["", ".", "..", "-x", "a/b", "a b", "é", &"x".repeat(65)] {
            assert!(check_group(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn merge_keeps_the_store_and_backs_up_conflicts() {
        let d = tempfile::tempdir().unwrap();
        let (src, dst, backup) = (
            d.path().join("src"),
            d.path().join("dst"),
            d.path().join("bk"),
        );
        for (dir, files) in [
            (
                &src,
                [
                    ("p/a.jsonl", "mine"),
                    ("p/same.jsonl", "x"),
                    ("q/b.jsonl", "b"),
                ],
            ),
            (
                &dst,
                [
                    ("p/a.jsonl", "theirs"),
                    ("p/same.jsonl", "x"),
                    ("r/c.jsonl", "c"),
                ],
            ),
        ] {
            for (rel, body) in files {
                let p = dir.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, body).unwrap();
            }
        }
        let mut undo = Transaction::new();
        let mut merge = Merge::default();
        merge_into(&src, &dst, &backup, &mut undo, &mut merge).unwrap();
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(dst.join("p/a.jsonl")), "theirs");
        assert_eq!(read(backup.join("p/a.jsonl")), "mine");
        assert_eq!(read(dst.join("q/b.jsonl")), "b");
        assert_eq!(read(dst.join("r/c.jsonl")), "c");
        assert!(!backup.join("p/same.jsonl").exists());
        assert_eq!((merge.moved, merge.conflicts), (1, 1));

        // Rolling back puts every moved file back where it was.
        let paths = Paths::new(d.path().to_path_buf());
        rollback(&paths, &mut undo);
        assert_eq!(read(src.join("p/a.jsonl")), "mine");
        assert_eq!(read(src.join("q/b.jsonl")), "b");
        assert!(!dst.join("q").exists());
        assert_eq!(read(dst.join("p/a.jsonl")), "theirs");
    }

    #[test]
    fn unreadable_files_are_never_the_same() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        for p in [&a, &b] {
            std::fs::write(p, "x").unwrap();
        }
        assert!(same_file(&a, &b));
        for p in [&a, &b] {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        // Root can still read 0o000 files, so the check means nothing there.
        if std::fs::read(&a).is_err() {
            assert!(!same_file(&a, &b));
        }
        for p in [&a, &b] {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn copy_tree_keeps_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(src.join("p")).unwrap();
        std::fs::write(src.join("p/s.jsonl"), "s").unwrap();
        std::os::unix::fs::symlink("s.jsonl", src.join("p/link")).unwrap();
        let dst = d.path().join("dst");
        assert_eq!(copy_tree(&src, &dst).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(dst.join("p/s.jsonl")).unwrap(), "s");
        assert_eq!(
            std::fs::read_link(dst.join("p/link")).unwrap(),
            PathBuf::from("s.jsonl")
        );
        // A missing source gives an empty directory.
        let empty = d.path().join("empty");
        assert_eq!(copy_tree(&d.path().join("none"), &empty).unwrap(), 0);
        assert!(empty.is_dir());
    }
}
