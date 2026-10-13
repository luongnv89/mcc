//! `mcc share` — opt a profile in to (or out of) session sharing.
//!
//! Sharing replaces the profile's `projects/` (where Claude Code keeps the
//! sessions `--resume` lists) with a symlink to one store,
//! `~/.mcc/shared/projects`, used by every sharing profile. Nothing else is
//! linked: credentials, the account (`.claude.json`), prompt history, live
//! session state and shell snapshots stay per profile.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;

use crate::cli::ShareArgs;
use crate::config::{Config, Profile};
use crate::error::CliError;
use crate::paths::Paths;
use crate::{fsutil, shell, ui};

/// Printed verbatim on stderr every time sharing is turned on.
pub const SHARE_TOS_NOTE: &str = crate::cli::share_tos_note!();

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

fn rollback(paths: &Paths, undo: Vec<Undo>) {
    for u in undo.into_iter().rev() {
        let (what, res) = match u {
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
        };
        if let Err(e) = res {
            ui::warn(&format!("rollback: could not {what}: {e:#}"));
        }
    }
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
        enable(paths, cfg, alias, &profile, args.from_launch)
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
    let Some(profile) = cfg.profiles.get(alias).cloned() else {
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
    Ok(profile)
}

fn enable(
    paths: &Paths,
    cfg: &mut Config,
    alias: &str,
    profile: &Profile,
    from_launch: bool,
) -> Result<()> {
    let store = paths.shared_projects_dir();
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
    if !from_launch {
        ui::header(&format!("Share the sessions of profile \"{alias}\""));
    }

    // The profile's own sessions wait in `staged` until everything else,
    // the registry included, succeeded.
    let staged = profile
        .dir
        .join(format!(".projects.mcc-share-{}", std::process::id()));
    let mut undo: Vec<Undo> = Vec::new();
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
        create_store(paths)?;
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
                undo.push(Undo::Moved {
                    from: projects.clone(),
                    to: staged.clone(),
                });
            }
            std::os::unix::fs::symlink(&store, &projects)
                .with_context(|| format!("linking {}", projects.display()))?;
            undo.push(Undo::Link(projects.clone()));
            if matches!(state, Projects::Dir) {
                let backup = merge.backup.clone();
                merge_into(&staged, &store, &backup, &mut undo, &mut merge)?;
            }
        }
        set_flag(paths, cfg, alias, true)?;
        Ok(merge)
    })();
    let merge = match result {
        Ok(m) => m,
        Err(e) => {
            rollback(paths, undo);
            return Err(e);
        }
    };
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
    eprintln!("{SHARE_TOS_NOTE}");
    if from_launch {
        return Ok(());
    }
    ui::result(
        true,
        &format!("profile \"{alias}\" shares its sessions with every other sharing profile"),
    );
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
    let store = paths.shared_projects_dir();
    let projects = profile.dir.join("projects");
    ui::header(&format!("Stop sharing the sessions of profile \"{alias}\""));
    if !matches!(projects_state(&projects, &store)?, Projects::Shared) {
        if profile.share_sessions {
            set_flag(paths, cfg, alias, false)?;
        }
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
    if let Err(e) = set_flag(paths, cfg, alias, false) {
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
        "the shared store {} is kept for the other sharing profiles; delete it by hand once none is left",
        paths.display(&store)
    ));
    Ok(())
}

fn set_flag(paths: &Paths, cfg: &mut Config, alias: &str, on: bool) -> Result<()> {
    let p = cfg
        .profiles
        .get_mut(alias)
        .expect("checked by sharing_profile");
    let was = std::mem::replace(&mut p.share_sessions, on);
    if let Err(e) = cfg.save(&paths.config_file) {
        if let Some(p) = cfg.profiles.get_mut(alias) {
            p.share_sessions = was;
        }
        return Err(e);
    }
    Ok(())
}

/// `~/.mcc/shared/projects`, private to the user like the profiles.
fn create_store(paths: &Paths) -> Result<()> {
    let store = paths.shared_projects_dir();
    std::fs::create_dir_all(&store).with_context(|| format!("creating {}", store.display()))?;
    fsutil::set_private_dir(&paths.shared_dir)?;
    fsutil::set_private_dir(&store)
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
    undo: &mut Vec<Undo>,
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
                undo.push(Undo::Moved { from, to });
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
                undo.push(Undo::Moved { from, to: kept });
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
        let mut undo = Vec::new();
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
        rollback(&paths, undo);
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
