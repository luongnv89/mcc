//! `mcc project` — the profile `mcc run` starts with in each project.
//!
//! A project is a directory, stored absolute with symlinks resolved. It
//! applies to everything below it; the deepest project containing the
//! current directory wins.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{ProjectAction, ProjectArgs};
use crate::config::{self, Config};
use crate::error::CliError;
use crate::paths::Paths;
use crate::ui;

pub fn run(paths: &Paths, cfg: &mut Config, args: &ProjectArgs) -> Result<()> {
    match &args.action {
        ProjectAction::Set {
            profile,
            path,
            allow,
        } => set(paths, cfg, profile, path.as_deref(), allow),
        ProjectAction::Unset { path } => unset(paths, cfg, path.as_deref()),
        ProjectAction::List { json } => list(paths, cfg, *json),
    }
}

/// The current directory, with symlinks resolved like stored projects.
pub fn current_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    Ok(std::fs::canonicalize(&cwd).unwrap_or(cwd))
}

/// The project that applies in `dir`: the deepest one containing it.
pub fn lookup<'a>(cfg: &'a Config, dir: &Path) -> Option<(&'a str, &'a config::Project)> {
    cfg.projects
        .iter()
        .filter(|(p, _)| dir.starts_with(p))
        .max_by_key(|(p, _)| Path::new(p).components().count())
        .map(|(p, c)| (p.as_str(), c))
}

/// The top of the git work tree containing `dir`, else `dir` itself. A
/// repository at `home` (a dotfiles repo) doesn't count: it would make your
/// whole home directory one project.
fn project_root(dir: &Path, home: &Path) -> PathBuf {
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    dir.ancestors()
        .take_while(|d| *d != home)
        .find(|d| d.join(".git").exists())
        .unwrap_or(dir)
        .to_path_buf()
}

/// `--path DIR` as stored: absolute, symlinks resolved.
fn existing_dir(path: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(p) if p.is_dir() => Ok(p),
        _ => Err(
            CliError::usage(format!("{} is not a directory", path.display()))
                .hint("pass an existing project directory with --path")
                .into(),
        ),
    }
}

fn key(dir: &Path) -> String {
    dir.to_string_lossy().into_owned()
}

/// A profile as projects store it (`claude` is `default`), or a usage
/// error when there is no such profile.
fn profile_name<'a>(cfg: &Config, name: &'a str) -> Result<&'a str> {
    match name {
        "default" | "claude" => Ok("default"),
        name if cfg.profiles.contains_key(name) => Ok(name),
        name => Err(super::unknown_profile(name)),
    }
}

fn set(
    paths: &Paths,
    cfg: &mut Config,
    profile: &str,
    path: Option<&Path>,
    allow: &[String],
) -> Result<()> {
    let profile = profile_name(cfg, profile)?;
    let allow = allow
        .iter()
        .map(|name| profile_name(cfg, name).map(str::to_string))
        .collect::<Result<Vec<_>>>()?;
    let dir = match path {
        Some(p) => existing_dir(p)?,
        None => project_root(&current_dir()?, &paths.home),
    };
    // The whole entry is replaced: without --allow, the list is cleared.
    let project = config::Project::new(profile, allow);
    let (also, choices) = match project.allow.as_slice() {
        [] => (String::new(), " or start it anyway"),
        names => (
            format!("; at its limit it can also offer {}", ticked(names)),
            ", start it anyway, or start an allowed profile",
        ),
    };
    let previous = cfg.projects.insert(key(&dir), project);
    cfg.save(&paths.config_file)?;
    let was = match previous {
        Some(p) if p.profile != profile => format!(" (was `{}`)", p.profile),
        _ => String::new(),
    };
    ui::success(&format!(
        "{}: `mcc run` starts `{profile}` here{was}{also}",
        paths.display(&dir)
    ));
    ui::hint(&format!(
        "when it is at its limit, `mcc run` asks whether to wait (the default){choices}; `mcc list` shows every project"
    ));
    Ok(())
}

/// "`lab`, `side`".
pub fn ticked(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn unset(paths: &Paths, cfg: &mut Config, path: Option<&Path>) -> Result<()> {
    let dir = match path {
        // A project that no longer exists can still be removed by its path.
        Some(p) => std::fs::canonicalize(p)
            .or_else(|_| std::path::absolute(p))
            .with_context(|| format!("resolving {}", p.display()))?,
        None => {
            let cwd = current_dir()?;
            match lookup(cfg, &cwd) {
                Some((dir, _)) => PathBuf::from(dir),
                None => {
                    return Err(CliError::usage(format!(
                        "no project default applies in {}",
                        paths.display(&cwd)
                    ))
                    .hint("run `mcc project list` to see them")
                    .into());
                }
            }
        }
    };
    let Some(old) = cfg.projects.remove(&key(&dir)) else {
        return Err(
            CliError::usage(format!("{} has no project default", paths.display(&dir)))
                .hint("run `mcc project list` to see them")
                .into(),
        );
    };
    cfg.save(&paths.config_file)?;
    ui::success(&format!(
        "{}: removed its project default (`{}`)",
        paths.display(&dir),
        old.profile
    ));
    Ok(())
}

#[derive(Serialize)]
struct Entry<'a> {
    path: &'a str,
    profile: &'a str,
    allow: &'a [String],
}

fn list(paths: &Paths, cfg: &Config, json: bool) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if json {
        let entries: Vec<Entry> = cfg
            .projects
            .iter()
            .map(|(path, p)| Entry {
                path,
                profile: &p.profile,
                allow: &p.allow,
            })
            .collect();
        serde_json::to_writer_pretty(&mut out, &entries)?;
        writeln!(out)?;
        return Ok(());
    }
    if cfg.projects.is_empty() {
        drop(out);
        ui::info("no project defaults yet — set one with `mcc project set <profile>` in a project");
        return Ok(());
    }
    let here = current_dir().ok();
    let here = here.as_deref().and_then(|d| lookup(cfg, d)).map(|(p, _)| p);
    let rows: Vec<(String, &str, String, &str)> = cfg
        .projects
        .iter()
        .map(|(path, p)| {
            let mark = if Some(path.as_str()) == here {
                "  ← here"
            } else {
                ""
            };
            let allow = match p.allow.as_slice() {
                [] => "—".to_string(),
                names => names.join(","),
            };
            (
                paths.display(Path::new(path)),
                p.profile.as_str(),
                allow,
                mark,
            )
        })
        .collect();
    let w_path = rows
        .iter()
        .map(|r| r.0.chars().count())
        .max()
        .unwrap_or(0)
        .max(7);
    let w_profile = rows
        .iter()
        .map(|r| r.1.chars().count())
        .max()
        .unwrap_or(0)
        .max(7);
    let w_allow = rows
        .iter()
        .map(|r| r.2.chars().count())
        .max()
        .unwrap_or(0)
        .max(5);
    writeln!(
        out,
        "{:<w_path$}  {:<w_profile$}  ALLOW",
        "PROJECT", "PROFILE"
    )?;
    for (path, profile, allow, mark) in rows {
        let line = format!("{path:<w_path$}  {profile:<w_profile$}  {allow:<w_allow$}{mark}");
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(projects: &[(&str, &str)]) -> Config {
        let mut cfg = Config::default();
        for (dir, profile) in projects {
            cfg.projects
                .insert(dir.to_string(), config::Project::new(profile, Vec::new()));
        }
        cfg
    }

    #[test]
    fn deepest_project_applies() {
        let cfg = cfg_with(&[("/src", "lab"), ("/src/app", "work"), ("/src/ap", "x")]);
        let at = |d: &str| lookup(&cfg, Path::new(d)).map(|(p, c)| (p, c.profile.as_str()));
        assert_eq!(at("/src/app"), Some(("/src/app", "work")));
        assert_eq!(at("/src/app/crates/core"), Some(("/src/app", "work")));
        // Whole path components only: /src/apple is not inside /src/ap.
        assert_eq!(at("/src/apple"), Some(("/src", "lab")));
        assert_eq!(at("/elsewhere"), None);
    }

    #[test]
    fn project_root_is_the_git_work_tree() {
        let d = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(d.path()).unwrap();
        let repo = home.join("code/repo");
        let deep = repo.join("src/bin");
        std::fs::create_dir_all(&deep).unwrap();
        // Outside any repository: the directory itself.
        assert_eq!(project_root(&deep, &home), deep);
        // A worktree's .git is a file; either kind marks the root.
        std::fs::write(repo.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(project_root(&deep, &home), repo);
        assert_eq!(project_root(&repo, &home), repo);
        // A repository at home is ignored.
        std::fs::remove_file(repo.join(".git")).unwrap();
        std::fs::create_dir(home.join(".git")).unwrap();
        assert_eq!(project_root(&deep, &home), deep);
    }
}
