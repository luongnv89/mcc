//! `mcc init` — detect the environment so mcc's shell setup takes effect.

use anyhow::Result;

use crate::cli::InitArgs;
use crate::config::Config;
use crate::env_detect::{self, EnvInfo, Shell};
use crate::paths::Paths;
use crate::{shell, ui};

pub fn run(paths: &Paths, cfg: &mut Config, args: &InitArgs) -> Result<()> {
    if let Some(env) = &cfg.env
        && !args.force
        && args.shell.is_none()
    {
        ui::header("mcc environment");
        report(paths, env, cfg);
        ui::result(true, "already initialized — use `mcc init --force` to re-detect");
        return Ok(());
    }
    let env = detect_and_save(paths, cfg, args.shell)?;
    report(paths, &env, cfg);
    ensure_local_bin_on_path(paths, &env)?;
    ui::result(true, &format!("environment saved to {}", paths.display(&paths.config_file)));
    Ok(())
}

/// Detect, print progress, and persist. Used by `init` and on first run.
pub fn detect_and_save(paths: &Paths, cfg: &mut Config, shell_override: Option<Shell>) -> Result<EnvInfo> {
    ui::header("Detecting environment");
    let mut steps = ui::Steps::new(4);

    let step = steps.start("Operating system and terminal");
    let env = env_detect::detect(paths, shell_override);
    if env.os == "macos" || env.os == "linux" {
        step.ok(&format!("{} ({}) · {}", env.os_version, env.arch, env.terminal));
    } else {
        step.warn(&format!("{} is not officially supported yet (macOS and Linux are)", env.os));
    }

    let step = steps.start("Shell and rc file");
    match (&env.shell, &env.rc_file) {
        (Some(sh), Some(rc)) => step.ok(&format!("{} → {}", sh.name(), paths.display(rc))),
        _ => step.warn(&format!(
            "unsupported shell \"{}\" — aliases won't be written; use `mcc init --shell zsh|bash|fish`",
            env.shell_path
        )),
    }

    let step = steps.start("Claude Code binary");
    match (&env.claude_bin, &env.claude_version) {
        (Some(bin), Some(v)) => step.ok(&format!("{} ({v})", paths.display(bin))),
        (Some(bin), None) => step.warn(&format!("{} found but `--version` failed", paths.display(bin))),
        (None, _) => step.warn("`claude` not found on PATH — install Claude Code before adding profiles"),
    }

    let step = steps.start("Launcher directory on PATH");
    if env.local_bin_on_path {
        step.ok(&format!("{} is on PATH", paths.display(&paths.local_bin)));
    } else {
        step.warn(&format!("{} is not on PATH", paths.display(&paths.local_bin)));
    }

    cfg.env = Some(env.clone());
    cfg.save(&paths.config_file)?;
    ui::detail(&format!("saved {}", paths.config_file.display()));
    Ok(env)
}

fn report(paths: &Paths, env: &EnvInfo, cfg: &Config) {
    ui::kv("OS", &format!("{} ({})", env.os_version, env.arch));
    ui::kv("Terminal", &env.terminal);
    ui::kv("Shell", env.shell.map_or("unsupported", |s| s.name()));
    ui::kv("rc file", &env.rc_file.as_ref().map_or("—".into(), |p| paths.display(p)));
    ui::kv(
        "Claude Code",
        &match (&env.claude_bin, &env.claude_version) {
            (Some(b), Some(v)) => format!("{v} at {}", paths.display(b)),
            (Some(b), None) => paths.display(b),
            (None, _) => "not found".into(),
        },
    );
    ui::kv(
        "Default profile",
        &if paths.default_dir.exists() {
            format!("{} (command: claude)", paths.display(&paths.default_dir))
        } else {
            "not created yet — `mcc add` will create it".into()
        },
    );
    ui::kv("mcc profiles", &cfg.profiles.len().to_string());
    ui::kv("Detected at", &env.detected_at);
}

/// Wrappers in ~/.local/bin only work when it is on PATH; offer to fix that.
fn ensure_local_bin_on_path(paths: &Paths, env: &EnvInfo) -> Result<()> {
    if env.local_bin_on_path {
        return Ok(());
    }
    let (Some(sh), Some(rc)) = (env.shell, &env.rc_file) else {
        ui::hint(&format!("add {} to your PATH so profile commands work", paths.display(&paths.local_bin)));
        return Ok(());
    };
    if shell::has_block(rc, "path") {
        ui::info(&format!("{} already adds it to PATH — open a new terminal to pick it up", paths.display(rc)));
        return Ok(());
    }
    let add = ui::is_interactive()
        && ui::confirm(
            &format!("Add {} to PATH in {}?", paths.display(&paths.local_bin), paths.display(rc)),
            true,
            "",
        )?;
    if add {
        shell::upsert_block(paths, rc, "path", &shell::path_block(sh))?;
        ui::success(&format!("added PATH entry to {} (open a new terminal or `source` it)", paths.display(rc)));
    } else {
        ui::hint(&format!(
            "profile commands need {} on PATH; shell aliases still work after `source {}`",
            paths.display(&paths.local_bin),
            paths.display(rc)
        ));
    }
    Ok(())
}
