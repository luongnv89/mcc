//! `mcc add` — create the default profile, or a new aliased profile.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::claude::{self, Claude};
use crate::cli::AddArgs;
use crate::config::{Config, Profile, SyncConfig};
use crate::copy::{self, Rewriter};
use crate::env_detect::{EnvInfo, dir_on_path};
use crate::error::CliError;
use crate::paths::Paths;
use crate::{fsutil, shell, ui, validate};

/// Something created during `add`, undone in reverse order on failure.
enum Undo {
    Dir(PathBuf),
    Wrapper(PathBuf),
    RcBlock(PathBuf, String),
    Registry(String),
}

fn rollback(paths: &Paths, cfg: &mut Config, undo: Vec<Undo>) {
    if undo.is_empty() {
        return;
    }
    ui::warn("Rolling back changes…");
    for u in undo.into_iter().rev() {
        let (what, res) = match u {
            Undo::Dir(p) => (paths.display(&p), fsutil::remove_any(&p)),
            Undo::Wrapper(p) => (paths.display(&p), fsutil::remove_any(&p)),
            Undo::RcBlock(rc, id) => {
                (format!("alias block in {}", paths.display(&rc)), shell::remove_block(paths, &rc, &id).map(|_| ()))
            }
            Undo::Registry(alias) => {
                cfg.profiles.remove(&alias);
                (format!("registry entry \"{alias}\""), cfg.save(&paths.config_file))
            }
        };
        match res {
            Ok(()) => ui::info(&format!("removed {what}")),
            Err(e) => ui::warn(&format!("could not remove {what}: {e:#}")),
        }
    }
}

pub fn run(paths: &Paths, cfg: &mut Config, args: &AddArgs) -> Result<()> {
    let env = cfg.env.clone().expect("init runs before commands");
    if !paths.default_dir.exists() {
        return add_default(paths, &env, args);
    }
    add_profile(paths, cfg, &env, args)
}

/// No `~/.claude` yet: this account becomes the default profile (`claude`).
fn add_default(paths: &Paths, env: &EnvInfo, args: &AddArgs) -> Result<()> {
    ui::header("Add the default Claude Code profile");
    ui::info(&format!(
        "{} does not exist yet, so this account becomes the default profile, started with `claude`.",
        paths.display(&paths.default_dir)
    ));
    if let Some(alias) = &args.alias {
        ui::warn(&format!(
            "alias \"{alias}\" is not used for the default profile — run `mcc add {alias}` afterwards to add a second account"
        ));
    }
    if ui::is_interactive() && !args.yes && !ui::confirm("Create the default profile now?", true, "")? {
        return Err(CliError::runtime("cancelled").into());
    }

    let mut steps = ui::Steps::new(3);
    let step = steps.start(&format!("Create {}", paths.display(&paths.default_dir)));
    std::fs::create_dir_all(&paths.default_dir)?;
    fsutil::set_private_dir(&paths.default_dir)?;
    step.ok("");

    if args.no_login {
        steps.start("Log in").skip("--no-login");
        steps.start("Verify login").skip("--no-login");
        ui::result(true, "default profile created — run `claude` to log in");
        return Ok(());
    }

    let bin = claude::require_binary(env.claude_bin.as_deref())?;
    let c = Claude { bin: &bin, paths, config_dir: None };
    login_steps(&mut steps, &c)?;
    ui::result(true, "default profile ready — start it with `claude`");
    Ok(())
}

/// Run login + verification steps. Returns Ok(true) when logged in.
fn login_steps(steps: &mut ui::Steps, c: &Claude) -> Result<bool> {
    let step = steps.start("Log in (complete the sign-in in your browser)");
    let ok = c.login()?;
    if ok {
        step.ok("");
    } else {
        step.fail("`claude auth login` did not finish");
    }
    let step = steps.start("Verify login");
    let status = c.status().unwrap_or_default();
    if status.logged_in {
        let who = status.email.or(status.method).unwrap_or_else(|| "logged in".into());
        step.ok(&who);
        Ok(true)
    } else {
        step.warn("not logged in");
        Ok(false)
    }
}

fn add_profile(paths: &Paths, cfg: &mut Config, env: &EnvInfo, args: &AddArgs) -> Result<()> {
    let shell_path = (!env.shell_path.is_empty()).then_some(env.shell_path.as_str());
    let alias = resolve_alias(paths, cfg, args, shell_path)?;
    let clone = resolve_mode(args)?;
    let bin = if args.no_login { claude::find_binary(env.claude_bin.as_deref()) } else {
        Some(claude::require_binary(env.claude_bin.as_deref())?)
    };

    ui::header(&format!("Add Claude Code profile \"{alias}\""));
    let profile_dir = paths.profile_dir(&alias);
    let mut steps = ui::Steps::new(8);
    let mut undo: Vec<Undo> = Vec::new();

    let result = (|| -> Result<()> {
        let step = steps.start("Validate alias");
        let conflicts = validate::conflicts(&alias, paths, cfg, shell_path);
        if !conflicts.is_empty() {
            step.fail("name is taken");
            return Err(conflict_error(&alias, &conflicts));
        }
        step.ok(&format!("\"{alias}\" is available"));

        let step = steps.start(&format!("Create {}", paths.display(&profile_dir)));
        std::fs::create_dir_all(&profile_dir)?;
        undo.push(Undo::Dir(profile_dir.clone()));
        fsutil::set_private_dir(&profile_dir)?;
        step.ok("");

        let step = steps.start(if clone { "Copy setup from the default profile" } else { "Set up an empty profile" });
        if clone {
            step.ok(&clone_setup(paths, &cfg.sync, &profile_dir)?);
        } else {
            step.skip("fresh profile");
        }

        let step = steps.start(&format!("Install launcher {}", paths.display(&paths.wrapper(&alias))));
        let wrapper = shell::write_wrapper(paths, &alias, &profile_dir)?;
        undo.push(Undo::Wrapper(wrapper.clone()));
        if dir_on_path(&paths.local_bin) {
            step.ok("");
        } else {
            step.warn(&format!("{} is not on PATH — run `mcc init` to fix", paths.display(&paths.local_bin)));
        }

        let step = steps.start("Add shell alias");
        let rc_file = match (env.shell, &env.rc_file) {
            (Some(sh), Some(rc)) => {
                shell::upsert_block(paths, rc, &alias, &shell::alias_block(paths, sh, &alias, &profile_dir))?;
                undo.push(Undo::RcBlock(rc.clone(), alias.clone()));
                step.ok(&paths.display(rc));
                Some(rc.clone())
            }
            _ => {
                step.skip("unsupported shell — the launcher command still works");
                None
            }
        };

        let step = steps.start("Register profile");
        cfg.profiles.insert(
            alias.clone(),
            Profile {
                dir: profile_dir.clone(),
                mode: if clone { "clone".into() } else { "fresh".into() },
                created_at: now(),
                wrapper: Some(wrapper),
                rc_file,
                last_sync: clone.then(now),
            },
        );
        undo.push(Undo::Registry(alias.clone()));
        cfg.save(&paths.config_file)?;
        step.ok(&paths.display(&paths.config_file));
        Ok(())
    })();

    if let Err(e) = result {
        rollback(paths, cfg, undo);
        return Err(e);
    }

    let logged_in = match (&bin, args.no_login) {
        (Some(bin), false) => {
            let c = Claude { bin, paths, config_dir: Some(&profile_dir) };
            let ok = login_steps(&mut steps, &c)?;
            if !ok {
                let keep = !ui::is_interactive()
                    || ui::confirm(
                        &format!("Login did not complete. Keep \"{alias}\" so you can log in later by running it?"),
                        true,
                        "",
                    )?;
                if !keep {
                    rollback(paths, cfg, undo);
                    return Err(CliError::runtime(format!("login failed; profile \"{alias}\" was removed"))
                        .hint(format!("retry with `mcc add {alias}`"))
                        .into());
                }
            }
            ok
        }
        _ => {
            steps.start("Log in").skip("--no-login");
            steps.start("Verify login").skip("--no-login");
            false
        }
    };

    summary(paths, cfg, &alias, clone, logged_in);
    Ok(())
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn clone_setup(paths: &Paths, sync_cfg: &SyncConfig, profile_dir: &Path) -> Result<String> {
    let spec = copy::spec(None, sync_cfg)?;
    let rw = Rewriter::new(paths, profile_dir, sync_cfg);
    let plan = copy::plan(&paths.default_dir, &paths.default_json, profile_dir, &spec, &rw, false)?;
    copy::apply(&plan, &paths.default_dir, profile_dir, &rw, ui::is_interactive() && !ui::settings().quiet)?;
    let groups: Vec<String> = plan.by_group().iter().map(|(g, a, ..)| format!("{g} {a}")).collect();
    let mcp = plan.mcp.as_ref().map_or(0, |m| m.added.len());
    Ok(format!(
        "{} items ({}), {mcp} MCP servers; no credentials copied",
        plan.count(copy::Action::Add),
        if groups.is_empty() { "nothing to copy".into() } else { groups.join(", ") }
    ))
}

fn resolve_alias(paths: &Paths, cfg: &Config, args: &AddArgs, shell_path: Option<&str>) -> Result<String> {
    if let Some(a) = &args.alias {
        validate::check_syntax(a).map_err(|m| CliError::usage(m).hint("example: mcc add work"))?;
        return Ok(a.clone());
    }
    ui::require_interactive("an alias", "pass it as an argument: mcc add <alias>")?;
    ui::info(&format!(
        "{} exists, so this will be an additional profile with its own command.",
        paths.display(&paths.default_dir)
    ));
    loop {
        let a = ui::input("Alias for the new profile (this becomes its command)", "")?;
        if let Err(m) = validate::check_syntax(&a) {
            ui::warn(&m);
            continue;
        }
        let conflicts = validate::conflicts(&a, paths, cfg, shell_path);
        if conflicts.is_empty() {
            return Ok(a);
        }
        for c in conflicts {
            ui::warn(&c.reason);
        }
    }
}

fn resolve_mode(args: &AddArgs) -> Result<bool> {
    if args.clone || args.fresh {
        return Ok(args.clone);
    }
    if args.yes {
        return Ok(true);
    }
    let choice = ui::select(
        "How should the new profile start?",
        &[
            "Clone the default's setup — settings, CLAUDE.md, skills, agents, commands, hooks, plugins, MCP servers (never credentials)",
            "Fresh — an empty profile",
        ],
        0,
        "pass --clone or --fresh",
    )?;
    Ok(choice == 0)
}

fn conflict_error(alias: &str, conflicts: &[validate::Conflict]) -> anyhow::Error {
    let reasons: Vec<&str> = conflicts.iter().map(|c| c.reason.as_str()).collect();
    CliError::usage(format!("alias \"{alias}\" can't be used: {}", reasons.join("; ")))
        .hint("choose another alias, e.g. `mcc add work2`")
        .into()
}

fn summary(paths: &Paths, cfg: &Config, alias: &str, clone: bool, logged_in: bool) {
    let p = &cfg.profiles[alias];
    ui::result(
        true,
        &if logged_in {
            format!("profile \"{alias}\" is ready — start it with `{alias}`")
        } else {
            format!("profile \"{alias}\" created — run `{alias}` to log in and start")
        },
    );
    ui::kv("Command", alias);
    ui::kv("Directory", &paths.display(&p.dir));
    ui::kv("Setup", if clone { "cloned from default (sync later with `mcc sync`)" } else { "fresh" });
    if let Some(w) = &p.wrapper {
        ui::kv("Launcher", &paths.display(w));
    }
    if let Some(rc) = &p.rc_file {
        ui::kv("Shell alias", &paths.display(rc));
        if !dir_on_path(&paths.local_bin) {
            ui::hint(&format!("open a new terminal or run `source {}` to use `{alias}`", paths.display(rc)));
        }
    }
}
