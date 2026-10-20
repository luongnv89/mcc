//! `mcc add` — create the default profile, or a new aliased profile.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value};

use crate::claude::{self, Claude};
use crate::cli::AddArgs;
use crate::config::{Account, Config, Profile};
use crate::copy::{self, Rewriter};
use crate::env_detect::{EnvInfo, dir_on_path};
use crate::error::CliError;
use crate::paths::Paths;
use crate::txn::{self, Transaction};
use crate::{fsutil, metrics, plugins, shell, ui, validate};

/// Something created during `add`, undone in reverse order on failure.
enum Undo {
    Dir(PathBuf),
    Wrapper(PathBuf),
    RcBlock(PathBuf, String),
    Registry(String),
}

fn rollback(paths: &Paths, cfg: &mut Config, undo: &mut Transaction<Undo>) {
    undo.rollback(&txn::REMOVE, |u| match u {
        Undo::Dir(p) => (paths.display(&p), fsutil::remove_any(&p)),
        Undo::Wrapper(p) => (paths.display(&p), fsutil::remove_any(&p)),
        Undo::RcBlock(rc, id) => (
            format!("alias block in {}", paths.display(&rc)),
            shell::remove_block(paths, &rc, &id).map(|_| ()),
        ),
        Undo::Registry(alias) => {
            cfg.profiles.remove(&alias);
            (
                format!("registry entry \"{alias}\""),
                cfg.save(&paths.config_file),
            )
        }
    });
}

pub fn run(paths: &Paths, cfg: &mut Config, args: &AddArgs) -> Result<()> {
    let env = cfg.env.clone().expect("init runs before commands");
    if let Some(label) = &args.label {
        check_label(label).map_err(|m| {
            CliError::usage(m).hint("example: mcc add work --label \"Acme (work)\"")
        })?;
    }
    if !paths.default_dir.exists() {
        return add_default(paths, cfg, &env, args);
    }
    add_profile(paths, cfg, &env, args)
}

/// No `~/.claude` yet: this account becomes the default profile (`claude`).
fn add_default(paths: &Paths, cfg: &Config, env: &EnvInfo, args: &AddArgs) -> Result<()> {
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
    if args.label.is_some() {
        ui::warn("--label is not used for the default profile; it applies to profiles mcc adds");
    }
    if ui::is_interactive()
        && !args.yes
        && !ui::confirm("Create the default profile now?", true, "")?
    {
        return Err(CliError::runtime("cancelled")
            .hint("nothing was created; run `mcc add` again when ready")
            .into());
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
    let c = Claude {
        bin: &bin,
        paths,
        config_dir: None,
    };
    if let Some(account) = login_steps(&mut steps, &c, &paths.default_json, None)? {
        warn_if_duplicate(paths, cfg, None, &account);
    }
    ui::result(true, "default profile ready — start it with `claude`");
    Ok(())
}

/// Run login + verification steps. Returns the account logged in to, or
/// `None` when the login did not complete. `state_json` is the profile's
/// `.claude.json`; `theme_src` is a state file to take the UI theme from
/// (the default profile's, when cloning).
fn login_steps(
    steps: &mut ui::Steps,
    c: &Claude,
    state_json: &Path,
    theme_src: Option<&Path>,
) -> Result<Option<Account>> {
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
        let account = logged_in_account(&status, state_json);
        let who = match (&account.email, &account.organization) {
            (Some(e), Some(o)) => format!("{e} ({o})"),
            (Some(e), None) => e.clone(),
            _ => status.method.unwrap_or_else(|| "logged in".into()),
        };
        step.ok(&who);
        if let Err(e) = mark_onboarded(state_json, theme_src) {
            ui::warn(&format!(
                "could not update {}: {e:#} — Claude Code may show its first-run setup once",
                state_json.display()
            ));
        }
        Ok(Some(account))
    } else {
        step.warn("not logged in");
        Ok(None)
    }
}

/// The account `claude auth status` reports, completed from the profile's
/// state file (`oauthAccount`: account and organization UUIDs) when that
/// describes the same login.
fn logged_in_account(status: &claude::AuthStatus, state_json: &Path) -> Account {
    let reported = Account {
        email: status.email.clone(),
        uuid: None,
        organization: status.org_name.clone(),
        organization_uuid: status.org_id.clone(),
        organization_type: None,
        recorded_at: None,
    };
    let state = metrics::account_info(state_json)
        .filter(|s| reported.email.is_none() || s.email.is_none() || reported.same_login(s))
        .unwrap_or_default();
    Account {
        recorded_at: Some(now()),
        ..reported.or(state)
    }
}

/// The other profiles' accounts: what each state file says now, else what
/// `mcc add` recorded. `alias` is the profile being added (`None` for the
/// default profile), which is left out.
fn other_accounts(paths: &Paths, cfg: &Config, alias: Option<&str>) -> Vec<(String, Account)> {
    let mut out = Vec::new();
    if alias.is_some()
        && let Some(a) = metrics::account_info(&paths.default_json)
    {
        out.push(("default".to_string(), a));
    }
    for (name, p) in &cfg.profiles {
        if Some(name.as_str()) == alias {
            continue;
        }
        let a = metrics::profile_account(p);
        if !a.is_anonymous() {
            out.push((name.clone(), a));
        }
    }
    out
}

/// Warn when another profile is already logged in to `account` in the same
/// organization: two such profiles share its usage limits, so it is usually
/// a mistake. The same login in another organization (a personal plan and a
/// work seat) has limits of its own: that only gets a note. Never fails.
fn warn_if_duplicate(paths: &Paths, cfg: &Config, alias: Option<&str>, account: &Account) {
    let others = other_accounts(paths, cfg, alias);
    let who = account.email.as_deref().unwrap_or("this account");
    let quote = |names: &[String]| -> String {
        names
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let plural = |names: &[String]| if names.len() == 1 { "" } else { "s" };
    let elsewhere = other_org_profiles(&others, account);
    if !elsewhere.is_empty() {
        ui::info(&format!(
            "{who} is also logged in to profile{} {} under another organization — each organization has its own usage limits",
            plural(&elsewhere),
            quote(&elsewhere)
        ));
    }
    let names = duplicate_profiles(&others, account);
    if names.is_empty() {
        return;
    }
    ui::warn(&format!(
        "{who} is already logged in to profile{} {} — profiles on the same account share its usage limits",
        plural(&names),
        quote(&names)
    ));
    match alias {
        Some(alias) => ui::hint(&format!(
            "if this was a mistake, remove it with `mcc delete {alias}` and add it again with another account"
        )),
        None => {
            ui::hint("run `claude auth login` to switch the default profile to another account")
        }
    }
}

/// The names of the profiles in `others` logged in to `account` in the same
/// organization (or where that can't be told apart).
fn duplicate_profiles(others: &[(String, Account)], account: &Account) -> Vec<String> {
    others
        .iter()
        .filter(|(_, a)| a.same_seat(account))
        .map(|(n, _)| n.clone())
        .collect()
}

/// The names of the profiles in `others` logged in to `account` in another
/// organization.
fn other_org_profiles(others: &[(String, Account)], account: &Account) -> Vec<String> {
    others
        .iter()
        .filter(|(_, a)| a.same_login(account) && !a.same_seat(account))
        .map(|(n, _)| n.clone())
        .collect()
}

/// Longest label accepted, in characters.
const LABEL_MAX: usize = 64;

/// A label is 1–64 characters of printable text.
pub(crate) fn check_label(label: &str) -> std::result::Result<(), String> {
    let t = label.trim();
    if t.is_empty() {
        return Err("the label is empty".into());
    }
    if t.chars().count() > LABEL_MAX {
        return Err(format!("the label is longer than {LABEL_MAX} characters"));
    }
    if t.chars().any(char::is_control) {
        return Err("the label contains control characters".into());
    }
    Ok(())
}

/// The profile's label: `--label`, else asked (interactive, without `-y`):
/// none, "personal", the account's organization, or one typed in.
fn resolve_label(args: &AddArgs, account: Option<&Account>) -> Result<Option<String>> {
    if let Some(l) = &args.label {
        return Ok(Some(l.trim().to_string()));
    }
    if args.yes || !ui::is_interactive() {
        return Ok(None);
    }
    let org = account.and_then(|a| a.organization.as_deref());
    let mut items = vec!["No label".to_string(), "personal".to_string()];
    if let Some(o) = org {
        items.push(format!("{o} (the account's organization)"));
    }
    items.push("Something else…".to_string());
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let choice = ui::select(
        "Label this account (shown in `mcc list`)?",
        &refs,
        0,
        "pass --label",
    )?;
    Ok(match (choice, org) {
        (0, _) => None,
        (1, _) => Some("personal".into()),
        (2, Some(o)) => Some(o.to_string()),
        _ => loop {
            let l = ui::input("Label", "pass --label")?;
            match check_label(&l) {
                Ok(()) => break Some(l),
                Err(m) => ui::warn(&m),
            }
        },
    })
}

/// `claude auth login` stores credentials but not `hasCompletedOnboarding`, so
/// without this the first interactive start shows the theme picker and the
/// login screen again even though the account is already logged in.
fn mark_onboarded(state_json: &Path, theme_src: Option<&Path>) -> Result<()> {
    let mut doc = copy::read_json(state_json)?.unwrap_or_else(|| Value::Object(Map::new()));
    let Some(obj) = doc.as_object_mut() else {
        return Ok(());
    };
    obj.insert("hasCompletedOnboarding".into(), Value::Bool(true));
    if !obj.contains_key("theme")
        && let Some(theme) = theme_src
            .and_then(|p| copy::read_json(p).ok().flatten())
            .and_then(|v| v.get("theme").cloned())
    {
        obj.insert("theme".into(), theme);
    }
    fsutil::write_atomic(state_json, serde_json::to_string_pretty(&doc)?.as_bytes())
}

fn add_profile(paths: &Paths, cfg: &mut Config, env: &EnvInfo, args: &AddArgs) -> Result<()> {
    let shell_path = (!env.shell_path.is_empty()).then_some(env.shell_path.as_str());
    let alias = resolve_alias(paths, cfg, args, shell_path)?;
    let clone = resolve_mode(args)?;
    // Checked before anything is created: each item can be linked and the
    // default has it.
    let mut link = match (clone, args.link.is_empty()) {
        (_, true) => Vec::new(),
        (true, false) => super::link::check_new_links(paths, &args.link)?,
        (false, false) => {
            return Err(CliError::usage(
                "--link needs --clone: a fresh profile copies nothing to link",
            )
            .hint(format!("example: mcc add {alias} --clone --link skills"))
            .into());
        }
    };
    // A clone shares the default's plugins unless asked for a copy (or
    // plugins are excluded from sync altogether).
    if args.copy_plugins && plugins::in_link(&link) {
        return Err(
            CliError::usage("--copy-plugins and --link plugins contradict each other")
                .hint(format!("example: mcc add {alias} --clone --copy-plugins"))
                .into(),
        );
    }
    if clone
        && !args.copy_plugins
        && !plugins::in_link(&link)
        && !cfg.sync.exclude.iter().any(|g| g == plugins::ITEM)
    {
        link.push(plugins::ITEM.into());
    }
    let bin = if args.no_login {
        claude::find_binary(env.claude_bin.as_deref())
    } else {
        Some(claude::require_binary(env.claude_bin.as_deref())?)
    };

    ui::header(&format!("Add Claude Code profile \"{alias}\""));
    let profile_dir = paths.profile_dir(&alias);
    let mut steps = ui::Steps::new(8);
    let mut undo: Transaction<Undo> = Transaction::new();

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
        undo.record(Undo::Dir(profile_dir.clone()));
        fsutil::set_private_dir(&profile_dir)?;
        step.ok("");

        let step = steps.start(if clone {
            "Copy setup from the default profile"
        } else {
            "Set up an empty profile"
        });
        if clone {
            step.ok(&clone_setup(paths, cfg, &profile_dir, &link)?);
            let symlinked: Vec<String> = link
                .iter()
                .filter(|l| *l != plugins::ITEM)
                .cloned()
                .collect();
            if !symlinked.is_empty() {
                ui::warn(&super::link::write_through_note(&alias, &symlinked));
            }
            if plugins::in_link(&link) {
                ui::warn(&plugins::shared_note(&alias));
            }
        } else {
            step.skip("fresh profile");
        }

        let step = steps.start(&format!(
            "Install launcher {}",
            paths.display(&paths.wrapper(&alias))
        ));
        let wrapper = shell::write_wrapper(
            paths,
            &alias,
            &profile_dir,
            claude::find_binary(env.claude_bin.as_deref()).as_deref(),
            plugins::in_link(&link)
                .then(|| plugins::shared_root(paths))
                .as_deref(),
        )?;
        undo.record(Undo::Wrapper(wrapper.clone()));
        if dir_on_path(&paths.local_bin) {
            step.ok("");
        } else {
            step.warn(&format!(
                "{} is not on PATH — run `mcc init` to fix",
                paths.display(&paths.local_bin)
            ));
        }

        let step = steps.start("Add shell alias");
        let rc_file = match (env.shell, &env.rc_file) {
            (Some(sh), Some(rc)) => {
                shell::upsert_block(
                    paths,
                    rc,
                    &alias,
                    &shell::alias_block(paths, sh, &alias, &profile_dir, Some(&wrapper)),
                )?;
                undo.record(Undo::RcBlock(rc.clone(), alias.clone()));
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
                mode: if clone {
                    "clone".into()
                } else {
                    "fresh".into()
                },
                created_at: now(),
                wrapper: Some(wrapper),
                rc_file,
                last_sync: clone.then(now),
                share_sessions: false,
                share_note_acknowledged_at: None,
                share_group: None,
                share_cross_org_allowed_at: None,
                label: None,
                account: None,
                settings: None,
                link: link.clone(),
            },
        );
        undo.record(Undo::Registry(alias.clone()));
        cfg.save(&paths.config_file)?;
        step.ok(&paths.display(&paths.config_file));
        Ok(())
    })();

    if let Err(e) = result {
        rollback(paths, cfg, &mut undo);
        return Err(e);
    }

    let account = match (&bin, args.no_login) {
        (Some(bin), false) => {
            let c = Claude {
                bin,
                paths,
                config_dir: Some(&profile_dir),
            };
            let account = match login_steps(
                &mut steps,
                &c,
                &profile_dir.join(".claude.json"),
                clone.then_some(paths.default_json.as_path()),
            ) {
                Ok(ok) => ok,
                Err(e) => {
                    rollback(paths, cfg, &mut undo);
                    return Err(CliError::runtime(format!(
                        "{e:#} — profile \"{alias}\" was removed"
                    ))
                    .hint(format!(
                        "check that {} is a working Claude Code binary (or set MCC_CLAUDE_BIN), then retry `mcc add {alias}`",
                        bin.display()
                    ))
                    .into());
                }
            };
            if account.is_none() {
                let keep = !ui::is_interactive()
                    || ui::confirm(
                        &format!(
                            "Login did not complete. Keep \"{alias}\" so you can log in later by running it?"
                        ),
                        true,
                        "",
                    )?;
                if !keep {
                    rollback(paths, cfg, &mut undo);
                    return Err(CliError::runtime(format!(
                        "login failed; profile \"{alias}\" was removed"
                    ))
                    .hint(format!("retry with `mcc add {alias}`"))
                    .into());
                }
            }
            account
        }
        _ => {
            steps.start("Log in").skip("--no-login");
            steps.start("Verify login").skip("--no-login");
            None
        }
    };
    // The profile is kept from here on: nothing below rolls it back.
    undo.commit();
    let logged_in = account.is_some();
    if let Some(a) = &account {
        warn_if_duplicate(paths, cfg, Some(&alias), a);
    }

    // The profile exists by now: a failed prompt or save only loses the label.
    let label = resolve_label(args, account.as_ref()).unwrap_or_else(|e| {
        ui::warn(&format!("no label set: {e:#}"));
        None
    });
    if (account.is_some() || label.is_some())
        && let Some(p) = cfg.profiles.get_mut(&alias)
    {
        p.account = account.or(p.account.take());
        p.label = label.or(p.label.take());
        if let Err(e) = cfg.save(&paths.config_file) {
            ui::warn(&format!(
                "could not record the account and label in {}: {e:#}",
                paths.display(&paths.config_file)
            ));
        }
    }

    summary(paths, cfg, &alias, clone, logged_in);
    Ok(())
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Copy the default's setup into the new profile, linking the items in
/// `link` to the default's copy instead (see `mcc link`).
fn clone_setup(paths: &Paths, cfg: &Config, profile_dir: &Path, link: &[String]) -> Result<String> {
    let spec = copy::spec(None, &cfg.sync)?.without(link);
    // The same rewriter sync uses, so a clone and a sync write the same.
    let mut rw = Rewriter::for_profile(paths, cfg, profile_dir)?;
    if plugins::in_link(link) {
        rw = rw.sharing_plugins();
    }
    let plan = copy::plan(
        &paths.default_dir,
        &paths.default_json,
        profile_dir,
        &spec,
        &rw,
        false,
    )?;
    copy::apply(
        &plan,
        &paths.default_dir,
        profile_dir,
        &rw,
        ui::is_interactive() && !ui::settings().quiet,
    )?;
    // What was copied, so `mcc sync` can later tell the default's changes
    // from edits made in the profile.
    plan.manifest(None).save(profile_dir)?;
    super::link::link_new_profile(paths, profile_dir, link)?;
    let groups: Vec<String> = plan
        .by_group()
        .iter()
        .map(|(g, a, ..)| format!("{g} {a}"))
        .collect();
    let mcp = plan.mcp.as_ref().map_or(0, |m| m.added.len());
    let symlinked: Vec<&str> = link
        .iter()
        .map(String::as_str)
        .filter(|l| *l != plugins::ITEM)
        .collect();
    let mut linked = String::new();
    if !symlinked.is_empty() {
        linked.push_str(&format!("; linked: {}", symlinked.join(", ")));
    }
    if plugins::in_link(link) {
        linked.push_str("; plugins shared with the default");
    }
    Ok(format!(
        "{} items ({}), {mcp} MCP servers{linked}; no credentials copied",
        plan.count(copy::Action::Add),
        if groups.is_empty() {
            "nothing to copy".into()
        } else {
            groups.join(", ")
        }
    ))
}

fn resolve_alias(
    paths: &Paths,
    cfg: &Config,
    args: &AddArgs,
    shell_path: Option<&str>,
) -> Result<String> {
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
    CliError::usage(format!(
        "alias \"{alias}\" can't be used: {}",
        reasons.join("; ")
    ))
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
    if let Some(a) = &p.account {
        match (&a.email, &a.organization) {
            (Some(e), Some(o)) => ui::kv("Account", &format!("{e} ({o})")),
            (Some(e), None) => ui::kv("Account", e),
            _ => {}
        }
    }
    if let Some(l) = &p.label {
        ui::kv("Label", l);
    }
    ui::kv("Directory", &paths.display(&p.dir));
    ui::kv(
        "Setup",
        if clone {
            "cloned from default (sync later with `mcc sync`)"
        } else {
            "fresh"
        },
    );
    if let Some(w) = &p.wrapper {
        ui::kv("Launcher", &paths.display(w));
    }
    if let Some(rc) = &p.rc_file {
        ui::kv("Shell alias", &paths.display(rc));
        if !dir_on_path(&paths.local_bin) {
            ui::hint(&format!(
                "open a new terminal or run `source {}` to use `{alias}`",
                paths.display(rc)
            ));
        }
    }
}
