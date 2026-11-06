//! Launcher management: `~/.local/bin/<alias>` wrappers and managed rc blocks.
//!
//! rc blocks are delimited by `# >>> mcc:<id> >>>` / `# <<< mcc:<id> <<<` so
//! they can be updated or removed without touching anything else in the file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::env_detect::Shell;
use crate::fsutil;
use crate::paths::Paths;

pub const WRAPPER_MARKER: &str = "# managed-by: mcc";

/// Block id of the `~/.local/bin` PATH entry. Profile blocks use the alias as
/// their id, so this name is reserved and can never be an alias.
pub const PATH_BLOCK: &str = "path";

/// Block id of the `claude` function that starts the global default profile
/// (`mcc default <alias>`). `default` can never be an alias, so it can't
/// clash with a profile's block.
pub const DEFAULT_BLOCK: &str = "default";

fn begin(id: &str) -> String {
    format!("# >>> mcc:{id} >>>")
}

fn end(id: &str) -> String {
    format!("# <<< mcc:{id} <<<")
}

/// Marks a launcher written from the current template (one that understands
/// `--mcc-share`). Launchers without it predate session sharing and are only
/// rewritten by `mcc refresh-launchers`.
pub const LAUNCHER_MARKER: &str = "# mcc-launcher: 2";

/// The version [`LAUNCHER_MARKER`] stands for.
const LAUNCHER_VERSION: u32 = 2;

/// Opens the launch-banner block, written only while `[launch] banner` is on.
pub const BANNER_MARKER: &str = "# Show the launch banner ([launch] banner = true).";

/// Opens the catch-up block, written only while `[sync] auto` is on for the
/// profile.
pub const SYNC_MARKER: &str = "# Catch up with the default profile ([sync] auto).";

/// The blocks a launcher runs before claude, each only while its setting is
/// on (see [`wrapper_script`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Blocks {
    /// `[launch] banner`: `mcc launch-banner`.
    pub banner: bool,
    /// `[sync] auto` for this profile: `mcc launch-sync`.
    pub sync: bool,
}

impl Blocks {
    /// The blocks the launcher of `profile` runs under `cfg` (`None`: a
    /// profile being added, with no settings of its own yet).
    pub fn for_profile(
        cfg: &crate::config::Config,
        profile: Option<&crate::config::Profile>,
    ) -> Self {
        Self {
            banner: cfg.launch.banner,
            sync: cfg.auto_sync_of(profile).is_on(),
        }
    }
}

/// The launch flag that turns on session sharing. Only the wrapper knows it:
/// it is stripped before claude starts.
pub const SHARE_FLAG: &str = "--mcc-share";

/// Wrapper script that launches claude with the profile's config dir. Uses
/// `claude` from PATH when available, else the binary found at `add` time
/// (so it also works from cron, IDE tasks, or shells with a trimmed PATH).
///
/// A first argument of `--mcc-share` is removed and runs
/// `mcc share <alias> --from-launch` first (`mcc` from PATH, else `mcc_bin`);
/// if that fails, claude is not started. Without the flag the script does
/// exactly what launchers did before session sharing existed, and the
/// `CLAUDE_CONFIG_DIR` line is unchanged: the keychain login is keyed on it.
///
/// With `blocks.sync` (`[sync] auto` is on for the profile) the script runs
/// `mcc launch-sync <alias>` on every launch, terminal or not, to catch
/// profiles up with the default; with `blocks.banner` (`[launch] banner` is
/// on) it then runs `mcc launch-banner <alias>` when its stderr is a
/// terminal. For both, mcc's stdout goes to the script's stderr, mcc's own
/// errors are discarded and its stdin is empty, so nothing reaches Claude
/// Code's stdout (`<alias> -p … | jq`), an mcc without the command stays
/// quiet, and the launch goes on whatever mcc does. A block whose setting is
/// off is left out, so with both off nothing but claude runs on a plain
/// launch.
///
/// With `plugin_root` (a profile sharing the default's plugins) the script
/// also exports `CLAUDE_CODE_PLUGIN_CACHE_DIR`; without it the script unsets
/// that variable only when it holds the default's plugins root (mcc's own
/// value, as `mcc run` and `mcc exec` do), keeping a value of the user's.
pub fn wrapper_script(
    paths: &Paths,
    alias: &str,
    profile_dir: &Path,
    claude_bin: Option<&Path>,
    mcc_bin: Option<&Path>,
    plugin_root: Option<&Path>,
    blocks: Blocks,
) -> String {
    let fallback = match claude_bin {
        Some(bin) => format!("exec {} \"$@\"", sh_quote(&bin.to_string_lossy())),
        None => "echo \"claude not found on PATH\" >&2\nexit 127".to_string(),
    };
    let share = format!("share {} --from-launch || exit $?", sh_quote(alias));
    let mcc_fallback = match mcc_bin {
        Some(bin) => format!("{} {share}", sh_quote(&bin.to_string_lossy())),
        None => "echo \"mcc not found on PATH\" >&2; exit 127".to_string(),
    };
    // `mcc <call>` from PATH, else `mcc_bin`, indented by `indent`.
    let run_mcc = |call: &str, indent: &str| {
        let fallback = match mcc_bin {
            Some(bin) => {
                let bin = sh_quote(&bin.to_string_lossy());
                format!("\n{indent}elif [ -x {bin} ]; then {bin} {call}")
            }
            None => String::new(),
        };
        format!(
            "{indent}if command -v mcc >/dev/null 2>&1; then mcc {call}{fallback}\n{indent}fi\n"
        )
    };
    let sync = if blocks.sync {
        let call = format!("launch-sync {} </dev/null >&2 2>/dev/null", sh_quote(alias));
        format!("{SYNC_MARKER}\n{}", run_mcc(&call, ""))
    } else {
        String::new()
    };
    let banner = if blocks.banner {
        let call = format!("launch-banner {} >&2 2>/dev/null", sh_quote(alias));
        format!(
            "{BANNER_MARKER}\nif [ -t 2 ]; then\n{}fi\n",
            run_mcc(&call, "  ")
        )
    } else {
        String::new()
    };
    format!(
        "#!/bin/sh\n{WRAPPER_MARKER}\n{LAUNCHER_MARKER}\n\
         # Claude Code profile \"{alias}\". Remove with: mcc delete {alias}\n\
         # Start it with {SHARE_FLAG} to share its sessions (see: mcc help share).\n\
         export CLAUDE_CONFIG_DIR=\"{dir}\"\n\
         {plugins}\
         case \"$1\" in\n  {SHARE_FLAG})\n    shift\n    \
         if command -v mcc >/dev/null 2>&1; then mcc {share}\n    \
         else {mcc_fallback}; fi ;;\nesac\n\
         {sync}\
         {banner}\
         if command -v claude >/dev/null 2>&1; then exec claude \"$@\"; fi\n\
         {fallback}\n",
        dir = paths.shell_expr(profile_dir),
        plugins = match plugin_root {
            Some(r) => format!(
                "export {}=\"{}\"\n",
                crate::plugins::ENV,
                paths.shell_expr(r)
            ),
            // mcc's own value (left by `eval "$(mcc env work)"`, say) is
            // cleared; a value of the user's is passed on.
            None => format!(
                "[ \"${{{var}:-}}\" = \"{root}\" ] && unset {var}\n",
                var = crate::plugins::ENV,
                root = paths.shell_expr(&crate::plugins::shared_root(paths))
            ),
        },
    )
}

/// Single-quote a string for POSIX sh.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// [`wrapper_script`] with this mcc binary as the fallback for `--mcc-share`.
pub fn launcher_script(
    paths: &Paths,
    alias: &str,
    profile_dir: &Path,
    claude_bin: Option<&Path>,
    plugin_root: Option<&Path>,
    blocks: Blocks,
) -> String {
    let mcc_bin = std::env::current_exe().ok();
    wrapper_script(
        paths,
        alias,
        profile_dir,
        claude_bin,
        mcc_bin.as_deref(),
        plugin_root,
        blocks,
    )
}

/// Write an executable launcher script to `path`.
pub fn install_launcher(path: &Path, script: &str) -> Result<()> {
    fsutil::write_atomic(path, script.as_bytes())?;
    fsutil::set_executable(path)
}

pub fn write_wrapper(
    paths: &Paths,
    alias: &str,
    profile_dir: &Path,
    claude_bin: Option<&Path>,
    plugin_root: Option<&Path>,
    blocks: Blocks,
) -> Result<PathBuf> {
    let path = paths.wrapper(alias);
    install_launcher(
        &path,
        &launcher_script(paths, alias, profile_dir, claude_bin, plugin_root, blocks),
    )?;
    Ok(path)
}

/// True when the file at `path` is a wrapper mcc wrote (safe to delete).
pub fn is_mcc_wrapper(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.contains(WRAPPER_MARKER))
        .unwrap_or(false)
}

/// The template version of the launcher at `path`, from its
/// `# mcc-launcher: N` line: 0 for a launcher without one (or no file).
pub fn launcher_version(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("# mcc-launcher: ")?.trim().parse().ok())
        })
        .unwrap_or(0)
}

/// True when the launcher at `path` was written from the current template
/// (it understands `--mcc-share`).
pub fn launcher_is_current(path: &Path) -> bool {
    launcher_version(path) >= LAUNCHER_VERSION
}

/// True when the launcher at `path` runs `mcc launch-banner`.
pub fn launcher_shows_banner(path: &Path) -> bool {
    has_line(path, BANNER_MARKER)
}

/// True when the launcher at `path` runs `mcc launch-sync`.
pub fn launcher_runs_sync(path: &Path) -> bool {
    has_line(path, SYNC_MARKER)
}

fn has_line(path: &Path, line: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.lines().any(|l| l == line))
        .unwrap_or(false)
}

/// True when the mcc launcher at `path` should be rewritten by
/// `mcc refresh-launchers`: it predates session sharing, or lacks a block
/// `blocks` turns on (the banner, the catch-up). A leftover block whose
/// setting is off is harmless: `mcc launch-banner` prints nothing then, and
/// `mcc launch-sync` checks the settings itself and does nothing.
pub fn launcher_needs_refresh(path: &Path, blocks: Blocks) -> bool {
    !launcher_is_current(path)
        || (blocks.banner && !launcher_shows_banner(path))
        || (blocks.sync && !launcher_runs_sync(path))
}

/// The rc block body defining the alias for one profile. With a launcher the
/// alias runs it, so `<alias> --mcc-share` reaches the launcher instead of
/// being handed to claude; without one it sets `CLAUDE_CONFIG_DIR` itself.
pub fn alias_block(
    paths: &Paths,
    shell: Shell,
    alias: &str,
    profile_dir: &Path,
    launcher: Option<&Path>,
) -> String {
    let line = match (launcher, shell) {
        (Some(w), Shell::Fish) => format!("alias {alias} '\"{}\"'", paths.shell_expr(w)),
        (Some(w), Shell::Zsh | Shell::Bash) => {
            format!("alias {alias}='\"{}\"'", paths.shell_expr(w))
        }
        (None, Shell::Fish) => format!(
            "alias {alias} 'env CLAUDE_CONFIG_DIR=\"{}\" claude'",
            paths.shell_expr(profile_dir)
        ),
        (None, Shell::Zsh | Shell::Bash) => format!(
            "alias {alias}='CLAUDE_CONFIG_DIR=\"{}\" claude'",
            paths.shell_expr(profile_dir)
        ),
    };
    format!(
        "# Claude Code profile \"{alias}\" — managed by mcc (remove with: mcc delete {alias})\n{line}"
    )
}

/// The rc block body that makes plain `claude` start profile `alias`, the
/// global default (`mcc default <alias>`): a `claude` function that runs the
/// profile's launcher, or without one sets `CLAUDE_CONFIG_DIR` itself.
///
/// Only where `CLAUDE_CONFIG_DIR` is unset or empty: a shell already pointed
/// at a profile (`mcc env`, direnv, `mcc exec`, a Claude Code session) keeps
/// running claude as that profile. `command claude` skips the function and
/// starts the default profile (~/.claude). The launcher is a `/bin/sh`
/// script that finds `claude` on PATH, where the function doesn't exist, so
/// it never calls itself. zsh and bash use `function claude { … }`: the
/// `claude() { … }` form fails to parse when `claude` is also an alias.
pub fn default_block(
    paths: &Paths,
    shell: Shell,
    alias: &str,
    profile_dir: &Path,
    launcher: Option<&Path>,
) -> String {
    let head = format!(
        "# Plain `claude` starts profile \"{alias}\" — managed by mcc (reset with: mcc default default).\n\
         # `command claude` starts the default profile (~/.claude)."
    );
    let dir = paths.shell_expr(profile_dir);
    let body = match (launcher, shell) {
        (Some(w), Shell::Fish) => format!(
            "function claude\n  if test -z \"$CLAUDE_CONFIG_DIR\"\n    \"{}\" $argv\n  else\n    command claude $argv\n  end\nend",
            paths.shell_expr(w)
        ),
        (None, Shell::Fish) => format!(
            "function claude\n  if test -z \"$CLAUDE_CONFIG_DIR\"\n    env CLAUDE_CONFIG_DIR=\"{dir}\" claude $argv\n  else\n    command claude $argv\n  end\nend"
        ),
        (Some(w), Shell::Zsh | Shell::Bash) => format!(
            "function claude {{\n  if [ -z \"${{CLAUDE_CONFIG_DIR:-}}\" ]; then \"{}\" \"$@\"; else command claude \"$@\"; fi\n}}",
            paths.shell_expr(w)
        ),
        (None, Shell::Zsh | Shell::Bash) => format!(
            "function claude {{\n  if [ -z \"${{CLAUDE_CONFIG_DIR:-}}\" ]; then CLAUDE_CONFIG_DIR=\"{dir}\" command claude \"$@\"; else command claude \"$@\"; fi\n}}"
        ),
    };
    format!("{head}\n{body}")
}

/// rc block that puts `~/.local/bin` on PATH.
pub fn path_block(shell: Shell) -> String {
    match shell {
        Shell::Fish => "fish_add_path -g $HOME/.local/bin".to_string(),
        Shell::Zsh | Shell::Bash => "export PATH=\"$HOME/.local/bin:$PATH\"".to_string(),
    }
}

/// Block `id` as it appears in an rc file: its markers around `body`.
pub fn block_text(id: &str, body: &str) -> String {
    format!("{}\n{}\n{}", begin(id), body.trim_end(), end(id))
}

/// Whether `line` opens an mcc block (any id).
pub fn is_block_begin(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("# >>> mcc:") && l.ends_with(" >>>")
}

/// Whether `line` closes an mcc block (any id).
pub fn is_block_end(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("# <<< mcc:") && l.ends_with(" <<<")
}

pub fn has_block(rc: &Path, id: &str) -> bool {
    std::fs::read_to_string(rc)
        .map(|s| s.contains(&begin(id)))
        .unwrap_or(false)
}

/// The body of block `id` in `rc` (between its markers), if there is one.
pub fn block_body(rc: &Path, id: &str) -> Option<String> {
    let text = std::fs::read_to_string(rc).ok()?;
    let (b, e) = (begin(id), end(id));
    let mut lines = text.lines().skip_while(|l| l.trim() != b).skip(1);
    let mut body = Vec::new();
    for l in lines.by_ref() {
        if l.trim() == e {
            return Some(body.join("\n"));
        }
        body.push(l);
    }
    None
}

/// Insert or replace the block `id` in `rc`. Backs the file up first.
pub fn upsert_block(paths: &Paths, rc: &Path, id: &str, body: &str) -> Result<()> {
    let original = read_or_empty(rc)?;
    let (stripped, _) = strip_block(&original, id);
    let mut out = stripped.trim_end_matches('\n').to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "{}\n{}\n{}\n",
        begin(id),
        body.trim_end(),
        end(id)
    ));
    backup(paths, rc, &original)?;
    fsutil::write_atomic(&write_target(rc), out.as_bytes())
}

/// Remove the block `id` from `rc`. Returns false when there was no block.
pub fn remove_block(paths: &Paths, rc: &Path, id: &str) -> Result<bool> {
    if !rc.exists() {
        return Ok(false);
    }
    let original = read_or_empty(rc)?;
    let (stripped, found) = strip_block(&original, id);
    if found {
        backup(paths, rc, &original)?;
        fsutil::write_atomic(&write_target(rc), stripped.as_bytes())?;
    }
    Ok(found)
}

/// The real file behind `rc`. rc files are often symlinks into a dotfiles repo
/// (GNU stow, yadm, ...); writing through the link (temp file + rename next to
/// the target) keeps the link intact instead of replacing it with a plain file.
/// Dangling links resolve to their target, which is then created. Regular and
/// missing files resolve to themselves.
fn write_target(rc: &Path) -> PathBuf {
    let mut path = rc.to_path_buf();
    // Bounded like the kernel's ELOOP limit so a link cycle cannot spin forever.
    for _ in 0..40 {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            break;
        };
        if !meta.file_type().is_symlink() {
            break;
        }
        let Ok(target) = std::fs::read_link(&path) else {
            break;
        };
        path = match path.parent() {
            Some(dir) if target.is_relative() => dir.join(target),
            _ => target,
        };
    }
    path
}

fn read_or_empty(rc: &Path) -> Result<String> {
    match std::fs::read_to_string(rc) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", rc.display())),
    }
}

/// Copy the rc file to `~/.mcc/backups/` before modifying it. Never overwrites
/// an earlier backup (several edits can land in the same millisecond).
fn backup(paths: &Paths, rc: &Path, content: &str) -> Result<()> {
    if content.is_empty() {
        return Ok(());
    }
    let name = rc
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "rc".into());
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
    let base = format!("{}.{stamp}", name.trim_start_matches('.'));
    let mut dest = paths.backups_dir.join(&base);
    let mut n = 1;
    while dest.exists() {
        dest = paths.backups_dir.join(format!("{base}.{n}"));
        n += 1;
    }
    fsutil::write_atomic(&dest, content.as_bytes())
}

/// Remove the block (and one blank line before it). Returns (text, found).
fn strip_block(text: &str, id: &str) -> (String, bool) {
    let (b, e) = (begin(id), end(id));
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut found = false;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == b
            && let Some(off) = lines[i..].iter().position(|l| l.trim() == e)
        {
            found = true;
            if out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            i += off + 1;
            continue;
        }
        out.push(lines[i]);
        i += 1;
    }
    let mut s = out.join("\n");
    if !s.is_empty() && (text.ends_with('\n') || found) {
        s.push('\n');
    }
    (s, found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No optional block: nothing but claude runs on a plain launch.
    const NONE: Blocks = Blocks {
        banner: false,
        sync: false,
    };
    const BANNER: Blocks = Blocks {
        banner: true,
        sync: false,
    };
    const SYNC: Blocks = Blocks {
        banner: false,
        sync: true,
    };

    #[test]
    fn upsert_and_remove_block() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let rc = d.path().join(".zshrc");
        std::fs::write(&rc, "export A=1\nalias x=y\n").unwrap();

        upsert_block(&paths, &rc, "work", "alias work='true'").unwrap();
        let s = std::fs::read_to_string(&rc).unwrap();
        assert!(s.starts_with(
            "export A=1\nalias x=y\n\n# >>> mcc:work >>>\nalias work='true'\n# <<< mcc:work <<<\n"
        ));

        // Idempotent replace.
        upsert_block(&paths, &rc, "work", "alias work='false'").unwrap();
        let s = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(s.matches(">>> mcc:work >>>").count(), 1);
        assert!(s.contains("alias work='false'"));

        assert!(remove_block(&paths, &rc, "work").unwrap());
        assert_eq!(
            std::fs::read_to_string(&rc).unwrap(),
            "export A=1\nalias x=y\n"
        );
        assert!(!remove_block(&paths, &rc, "work").unwrap());
        // One backup per modification, none overwritten, the original kept.
        let backups: Vec<_> = paths
            .backups_dir
            .read_dir()
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 3, "{backups:?}");
        assert!(
            backups
                .iter()
                .any(|p| std::fs::read_to_string(p).unwrap() == "export A=1\nalias x=y\n")
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_symlink_chain() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let real = d.path().join("dotfiles/zshrc");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "export A=1\n").unwrap();
        let mid = d.path().join("mid");
        let rc = d.path().join(".zshrc");
        std::os::unix::fs::symlink(&real, &mid).unwrap();
        std::os::unix::fs::symlink(&mid, &rc).unwrap();

        upsert_block(&paths, &rc, "work", "alias work='true'").unwrap();
        for link in [&rc, &mid] {
            assert!(
                std::fs::symlink_metadata(link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        assert!(std::fs::read_to_string(&real).unwrap().contains("mcc:work"));

        assert!(remove_block(&paths, &rc, "work").unwrap());
        assert!(
            std::fs::symlink_metadata(&rc)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "export A=1\n");
    }

    #[test]
    fn strip_keeps_other_blocks() {
        let text = "a\n\n# >>> mcc:one >>>\nx\n# <<< mcc:one <<<\n\n# >>> mcc:two >>>\ny\n# <<< mcc:two <<<\n";
        let (s, found) = strip_block(text, "one");
        assert!(found);
        assert_eq!(s, "a\n\n# >>> mcc:two >>>\ny\n# <<< mcc:two <<<\n");
    }

    #[test]
    fn alias_lines() {
        let paths = Paths::new("/h".into());
        let dir = paths.profile_dir("work");
        let wrapper = paths.wrapper("work");
        // With a launcher, the alias runs it (so `work --mcc-share` reaches it).
        assert!(
            alias_block(&paths, Shell::Zsh, "work", &dir, Some(&wrapper))
                .contains("\nalias work='\"$HOME/.local/bin/work\"'")
        );
        assert!(
            alias_block(&paths, Shell::Fish, "work", &dir, Some(&wrapper))
                .contains("\nalias work '\"$HOME/.local/bin/work\"'")
        );
        // Without one, the legacy form that sets CLAUDE_CONFIG_DIR itself.
        assert!(
            alias_block(&paths, Shell::Zsh, "work", &dir, None)
                .contains("alias work='CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\" claude'")
        );
        assert!(
            alias_block(&paths, Shell::Fish, "work", &dir, None)
                .contains("alias work 'env CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\" claude'")
        );
        let w = wrapper_script(
            &paths,
            "work",
            &dir,
            Some(Path::new("/opt/it's/claude")),
            Some(Path::new("/opt/bin/mcc")),
            None,
            NONE,
        );
        assert!(w.starts_with("#!/bin/sh\n# managed-by: mcc\n# mcc-launcher: 2\n"));
        assert!(w.contains("export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\"\n"));
        assert!(w.contains("if command -v claude >/dev/null 2>&1; then exec claude \"$@\"; fi\n"));
        assert!(w.contains("exec '/opt/it'\\''s/claude' \"$@\"\n"), "{w}");
        assert!(wrapper_script(&paths, "work", &dir, None, None, None, NONE).contains("exit 127"));
        // Without shared plugins the launcher only clears mcc's own value.
        assert!(w.contains(
            "[ \"${CLAUDE_CODE_PLUGIN_CACHE_DIR:-}\" = \"$HOME/.claude/plugins\" ] && unset CLAUDE_CODE_PLUGIN_CACHE_DIR\n"
        ), "{w}");
        assert!(!w.contains("export CLAUDE_CODE_PLUGIN_CACHE_DIR"));
    }

    #[test]
    fn default_block_runs_the_launcher_only_without_a_config_dir() {
        let paths = Paths::new("/h".into());
        let dir = paths.profile_dir("work");
        let wrapper = paths.wrapper("work");
        let zsh = default_block(&paths, Shell::Zsh, "work", &dir, Some(&wrapper));
        assert!(zsh.contains("mcc default default"), "{zsh}");
        assert!(
            zsh.ends_with(
                "function claude {\n  if [ -z \"${CLAUDE_CONFIG_DIR:-}\" ]; then \"$HOME/.local/bin/work\" \"$@\"; else command claude \"$@\"; fi\n}"
            ),
            "{zsh}"
        );
        assert_eq!(
            default_block(&paths, Shell::Bash, "work", &dir, Some(&wrapper)),
            zsh
        );
        let fish = default_block(&paths, Shell::Fish, "work", &dir, Some(&wrapper));
        assert!(
            fish.ends_with(
                "function claude\n  if test -z \"$CLAUDE_CONFIG_DIR\"\n    \"$HOME/.local/bin/work\" $argv\n  else\n    command claude $argv\n  end\nend"
            ),
            "{fish}"
        );
        // Without a launcher it sets CLAUDE_CONFIG_DIR and runs the real claude.
        let zsh = default_block(&paths, Shell::Zsh, "work", &dir, None);
        assert!(
            zsh.contains(
                "then CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\" command claude \"$@\"; else"
            ),
            "{zsh}"
        );
        let fish = default_block(&paths, Shell::Fish, "work", &dir, None);
        assert!(
            fish.contains("    env CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\" claude $argv\n"),
            "{fish}"
        );
    }

    #[test]
    fn launcher_of_a_profile_sharing_plugins_exports_the_plugins_root() {
        let paths = Paths::new(PathBuf::from("/h"));
        let dir = paths.profile_dir("work");
        let root = crate::plugins::shared_root(&paths);
        let w = wrapper_script(&paths, "work", &dir, None, None, Some(&root), NONE);
        assert!(
            w.contains(
                "export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\"\n\
                 export CLAUDE_CODE_PLUGIN_CACHE_DIR=\"$HOME/.claude/plugins\"\ncase "
            ),
            "{w}"
        );
    }

    /// The launcher every profile had before session sharing (v0.2.0).
    const LEGACY_WRAPPER: &str = "#!/bin/sh\n# managed-by: mcc\n\
        # Claude Code profile \"work\". Remove with: mcc delete work\n\
        export CLAUDE_CONFIG_DIR=\"$HOME/.mcc/profiles/work\"\n\
        if command -v claude >/dev/null 2>&1; then exec claude \"$@\"; fi\n\
        exec '/opt/claude' \"$@\"\n";

    #[test]
    fn launcher_keeps_the_legacy_launch_lines() {
        let paths = Paths::new("/h".into());
        let dir = paths.profile_dir("work");
        let w = wrapper_script(
            &paths,
            "work",
            &dir,
            Some(Path::new("/opt/claude")),
            Some(Path::new("/opt/bin/mcc")),
            None,
            NONE,
        );
        // The CLAUDE_CONFIG_DIR export (the keychain service is a hash of the
        // value it produces) and the exec lines are byte-identical to v0.2.0.
        let legacy: Vec<&str> = LEGACY_WRAPPER.lines().collect();
        let export = legacy.iter().find(|l| l.starts_with("export ")).unwrap();
        assert_eq!(
            w.lines()
                .filter(|l| l.starts_with("export "))
                .collect::<Vec<_>>(),
            [*export]
        );
        assert!(
            w.ends_with(&format!("{}\n{}\n", legacy[4], legacy[5])),
            "{w}"
        );
        // The only additions on the launch path are clearing mcc's own
        // plugins root (#47) and the `--mcc-share` branch, placed between the
        // export and the exec lines. With the banner off there is no banner
        // block (#59): nothing but claude runs on a plain launch.
        let shared = [
            "# mcc-launcher: 2",
            "# Start it with --mcc-share to share its sessions (see: mcc help share).",
            "[ \"${CLAUDE_CODE_PLUGIN_CACHE_DIR:-}\" = \"$HOME/.claude/plugins\" ] && unset CLAUDE_CODE_PLUGIN_CACHE_DIR",
            "case \"$1\" in",
            "  --mcc-share)",
            "    shift",
            "    if command -v mcc >/dev/null 2>&1; then mcc share 'work' --from-launch || exit $?",
            "    else '/opt/bin/mcc' share 'work' --from-launch || exit $?; fi ;;",
            "esac",
        ];
        let added: Vec<&str> = w.lines().filter(|l| !legacy.contains(l)).collect();
        assert_eq!(added, shared, "{w}");
        assert!(!w.contains("launch-banner"), "{w}");

        // With the banner on, the launch-banner block comes after the
        // `--mcc-share` branch, and the launch lines are unchanged.
        let on = wrapper_script(
            &paths,
            "work",
            &dir,
            Some(Path::new("/opt/claude")),
            Some(Path::new("/opt/bin/mcc")),
            None,
            BANNER,
        );
        assert!(
            on.ends_with(&format!("{}\n{}\n", legacy[4], legacy[5])),
            "{on}"
        );
        let added: Vec<&str> = on.lines().filter(|l| !legacy.contains(l)).collect();
        let mut expected = shared.to_vec();
        expected.extend([
            BANNER_MARKER,
            "if [ -t 2 ]; then",
            "  if command -v mcc >/dev/null 2>&1; then mcc launch-banner 'work' >&2 2>/dev/null",
            "  elif [ -x '/opt/bin/mcc' ]; then '/opt/bin/mcc' launch-banner 'work' >&2 2>/dev/null",
            "  fi",
            "fi",
        ]);
        assert_eq!(added, expected, "{on}");

        let no_mcc = wrapper_script(&paths, "work", &dir, None, None, None, NONE);
        assert!(no_mcc.contains("else echo \"mcc not found on PATH\" >&2; exit 127; fi ;;"));
        assert!(!no_mcc.contains("launch-banner"), "{no_mcc}");
        let no_mcc = wrapper_script(&paths, "work", &dir, None, None, None, BANNER);
        assert!(
            no_mcc.contains(
                "if [ -t 2 ]; then\n  if command -v mcc >/dev/null 2>&1; then mcc launch-banner 'work' >&2 2>/dev/null\n  fi\nfi\n"
            ),
            "{no_mcc}"
        );
    }

    #[test]
    fn the_catch_up_block_runs_before_the_banner_and_never_stops_the_launch() {
        let paths = Paths::new("/h".into());
        let dir = paths.profile_dir("work");
        let script = |blocks| {
            wrapper_script(
                &paths,
                "work",
                &dir,
                Some(Path::new("/opt/claude")),
                Some(Path::new("/opt/bin/mcc")),
                None,
                blocks,
            )
        };
        let plain = script(NONE);
        assert!(!plain.contains("launch-sync"), "{plain}");
        // On every launch, terminal or not: no `[ -t 2 ]` guard, stdin
        // empty, mcc's stdout to the script's stderr, its errors discarded,
        // and no `|| exit`: claude starts whatever mcc does.
        let block = "# Catch up with the default profile ([sync] auto).\n\
            if command -v mcc >/dev/null 2>&1; then mcc launch-sync 'work' </dev/null >&2 2>/dev/null\n\
            elif [ -x '/opt/bin/mcc' ]; then '/opt/bin/mcc' launch-sync 'work' </dev/null >&2 2>/dev/null\n\
            fi\n";
        let on = script(SYNC);
        assert_eq!(on, plain.replacen("esac\n", &format!("esac\n{block}"), 1));
        // With the banner too, the catch-up comes first.
        let both = script(Blocks {
            banner: true,
            sync: true,
        });
        let banner = script(BANNER);
        assert_eq!(
            both,
            banner.replacen("esac\n", &format!("esac\n{block}"), 1)
        );
        // Without a known mcc binary, only the one on PATH.
        let no_mcc = wrapper_script(&paths, "work", &dir, None, None, None, SYNC);
        assert!(
            no_mcc.contains(
                "esac\n# Catch up with the default profile ([sync] auto).\n\
                 if command -v mcc >/dev/null 2>&1; then mcc launch-sync 'work' </dev/null >&2 2>/dev/null\nfi\n"
            ),
            "{no_mcc}"
        );
    }

    #[test]
    fn a_launcher_without_the_catch_up_needs_a_refresh_only_while_it_is_on() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let dir = paths.profile_dir("work");
        let w = d.path().join("work");
        std::fs::write(
            &w,
            wrapper_script(&paths, "work", &dir, None, None, None, NONE),
        )
        .unwrap();
        assert!(!launcher_runs_sync(&w));
        assert!(!launcher_needs_refresh(&w, NONE));
        assert!(launcher_needs_refresh(&w, SYNC));
        std::fs::write(
            &w,
            wrapper_script(&paths, "work", &dir, None, None, None, SYNC),
        )
        .unwrap();
        assert!(launcher_runs_sync(&w) && launcher_is_current(&w));
        assert!(!launcher_needs_refresh(&w, SYNC));
        // A leftover block with it off is harmless (`mcc launch-sync`
        // checks the settings itself).
        assert!(!launcher_needs_refresh(&w, NONE));
    }

    #[test]
    fn launcher_marker_and_block_body() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        let w = d.path().join("work");
        assert!(!launcher_is_current(&w));
        std::fs::write(&w, LEGACY_WRAPPER).unwrap();
        assert!(is_mcc_wrapper(&w) && !launcher_is_current(&w));
        assert_eq!(launcher_version(&w), 0);
        assert!(launcher_needs_refresh(&w, NONE) && launcher_needs_refresh(&w, BANNER));
        let dir = paths.profile_dir("work");
        // With the banner off: current, no banner block, nothing to refresh.
        std::fs::write(
            &w,
            wrapper_script(&paths, "work", &dir, None, None, None, NONE),
        )
        .unwrap();
        assert!(is_mcc_wrapper(&w) && launcher_is_current(&w));
        assert_eq!(launcher_version(&w), 2);
        assert!(!launcher_shows_banner(&w));
        assert!(!launcher_needs_refresh(&w, NONE));
        // ...until the banner is turned on.
        assert!(launcher_needs_refresh(&w, BANNER));
        std::fs::write(
            &w,
            wrapper_script(&paths, "work", &dir, None, None, None, BANNER),
        )
        .unwrap();
        assert!(launcher_is_current(&w) && launcher_shows_banner(&w));
        assert!(!launcher_needs_refresh(&w, BANNER));
        // A leftover block with the banner off is harmless.
        assert!(!launcher_needs_refresh(&w, NONE));

        let rc = d.path().join(".zshrc");
        assert_eq!(block_body(&rc, "work"), None);
        upsert_block(&paths, &rc, "work", "# c\nalias work='true'").unwrap();
        upsert_block(&paths, &rc, "lab", "alias lab='x'").unwrap();
        assert_eq!(
            block_body(&rc, "work").as_deref(),
            Some("# c\nalias work='true'")
        );
        assert_eq!(block_body(&rc, "lab").as_deref(), Some("alias lab='x'"));
        assert_eq!(block_body(&rc, "nope"), None);
    }
}
