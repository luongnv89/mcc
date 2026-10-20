//! `mcc diff` — how a profile differs from the default profile's setup, path
//! by path, classified as `mcc sync` would (see `copy::plan_with`). Read-only.

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use console::style;
use serde_json::Value;

use crate::cli::DiffArgs;
use crate::config::Config;
use crate::copy::{self, Action, Change, Rewriter};
use crate::error::CliError;
use crate::manifest;
use crate::paths::Paths;
use crate::ui;

/// Files larger than this (in lines, per side) are reported without a diff.
const MAX_DIFF_LINES: usize = 5_000;
/// Unchanged lines shown around each change.
const CONTEXT: usize = 3;

pub fn run(paths: &Paths, cfg: &Config, args: &DiffArgs) -> Result<()> {
    let alias = args.alias.as_str();
    if matches!(alias, "default" | "claude") {
        return Err(CliError::usage(
            "the default profile is what profiles are compared with; pick another profile",
        )
        .hint("example: mcc diff work")
        .into());
    }
    let Some(profile) = cfg.profiles.get(alias) else {
        return Err(super::unknown_profile(alias));
    };
    if !paths.default_dir.is_dir() {
        return Err(CliError::runtime(format!(
            "the default profile {} does not exist, so there is nothing to compare with",
            paths.display(&paths.default_dir)
        ))
        .hint("run `mcc add` to create it first")
        .into());
    }
    let dir = profile.dir.clone();
    if !dir.is_dir() {
        return Err(CliError::runtime(format!(
            "profile directory {} is missing",
            paths.display(&dir)
        ))
        .hint(format!(
            "remove the stale entry with `mcc delete {alias} --yes`"
        ))
        .into());
    }
    let spec = copy::spec(args.only.as_deref(), &cfg.sync)?;
    let loaded = manifest::load(&dir);
    if let manifest::Loaded::Invalid(why) = &loaded {
        ui::warn(&format!(
            "ignoring {} ({why}); comparing two-way",
            paths.display(&dir.join(manifest::FILE))
        ));
    }
    let plan = super::sync::plan_for(paths, cfg, &dir, &spec, loaded.manifest(), false, false)?;
    let rw = Rewriter::for_profile(paths, cfg, &dir)?;

    let mut out = std::io::stdout().lock();
    let mut shown = 0;
    for c in &plan.changes {
        // A missing directory's files are listed one by one.
        if c.dir && c.action == Action::Add {
            continue;
        }
        shown += 1;
        file_diff(&mut out, paths, &dir, alias, c, &rw)?;
    }
    if spec.mcp {
        let (src, dst) = copy::mcp_servers(&paths.default_json, &dir.join(".claude.json"), &rw)?;
        let none = Vec::new();
        let m = plan.mcp.as_ref();
        let pick = |f: fn(&copy::McpChange) -> &Vec<String>| m.map_or(&none, f);
        // Servers only the profile has (sync keeps them unless --prune).
        let extra: Vec<String> = dst
            .keys()
            .filter(|k| !src.contains_key(*k))
            .cloned()
            .collect();
        let labeled = [
            (pick(|m| &m.added), "new in default"),
            (pick(|m| &m.updated), "changed in default"),
            (pick(|m| &m.local), "local edit"),
            (pick(|m| &m.conflicts), "conflict"),
            (&extra, "only in profile"),
        ];
        for (names, label) in labeled {
            for name in names {
                shown += 1;
                header(&mut out, label, &format!("MCP server \"{name}\""))?;
                let text = |v: Option<&Value>| {
                    v.map(|v| serde_json::to_string_pretty(v).unwrap_or_default() + "\n")
                        .unwrap_or_default()
                };
                text_diff(
                    &mut out,
                    &format!("default/.claude.json mcpServers.{name}"),
                    &format!("{alias}/.claude.json mcpServers.{name}"),
                    &text(src.get(name)),
                    &text(dst.get(name)),
                )?;
            }
        }
    }
    drop(out);

    let status = plan.status();
    if shown == 0 {
        ui::result(
            true,
            &format!("\"{alias}\" matches the default profile's setup"),
        );
    } else {
        ui::info(&format!(
            "\"{alias}\": {}",
            super::sync::status_text(&status)
        ));
        if status.behind + status.conflicts > 0 {
            ui::hint(&format!(
                "`mcc sync {alias}` takes the default's changes and keeps local edits{}",
                if status.conflicts > 0 {
                    "; --force also overwrites conflicts"
                } else {
                    ""
                }
            ));
        }
    }
    Ok(())
}

/// What `c` means for the profile, in words.
fn label(c: &Change) -> &'static str {
    match c.action {
        Action::Add => "new in default",
        Action::Update => "changed in default",
        Action::Remove => "replaced",
        Action::Extra => "only in profile",
        Action::LocalEdit => "local edit",
        Action::Conflict => "conflict",
    }
}

fn header(out: &mut impl Write, label: &str, what: &str) -> Result<()> {
    writeln!(out, "{}", style(format!("== {label}: {what}")).bold())?;
    Ok(())
}

/// One changed path: a header, then a line diff (default → profile) when
/// both sides are text, or what each side is.
fn file_diff(
    out: &mut impl Write,
    paths: &Paths,
    dir: &Path,
    alias: &str,
    c: &Change,
    rw: &Rewriter,
) -> Result<()> {
    let rel = c.rel.display().to_string();
    let shown = if c.dir {
        format!("{rel}/")
    } else {
        rel.clone()
    };
    header(out, label(c), &shown)?;
    let src = paths.default_dir.join(&c.rel);
    let dst = dir.join(&c.rel);
    let (a, b) = (side(&src, Some((c.rewrite, rw)))?, side(&dst, None)?);
    // The default's settings.json as the profile receives it: with its overlay.
    let from = if rel == copy::SETTINGS_FILE && rw.has_overlay() {
        format!("default/{rel} + {alias} overlay")
    } else {
        format!("default/{rel}")
    };
    let to = format!("{alias}/{rel}");
    match (a, b) {
        (Side::Text(a), Side::Text(b)) => text_diff(out, &from, &to, &a, &b)?,
        (Side::Text(a), Side::Missing) => text_diff(out, &from, &to, &a, "")?,
        (Side::Missing, Side::Text(b)) => text_diff(out, &from, &to, "", &b)?,
        (a, b) if a != b => {
            writeln!(out, "  default: {}", a.describe())?;
            writeln!(out, "  {alias}: {}", b.describe())?;
        }
        _ => {}
    }
    Ok(())
}

#[derive(PartialEq)]
enum Side {
    Missing,
    Dir,
    Link(String),
    Text(String),
    Binary(u64),
}

impl Side {
    fn describe(&self) -> String {
        match self {
            Side::Missing => "missing".into(),
            Side::Dir => "a directory".into(),
            Side::Link(t) => format!("symlink → {t}"),
            Side::Text(t) => format!("text file ({} lines)", t.lines().count()),
            Side::Binary(n) => format!("binary file ({})", ui::human_size(*n)),
        }
    }
}

/// What is at `p`; a default file (`rewrite` given) as the profile receives it.
fn side(p: &Path, rewrite: Option<(bool, &Rewriter)>) -> Result<Side> {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return Ok(Side::Missing);
    };
    let overlaid = rewrite.is_some_and(|(_, rw)| rw.overlays(p));
    if meta.file_type().is_symlink() && !overlaid {
        let target = std::fs::read_link(p).unwrap_or_default();
        return Ok(Side::Link(target.display().to_string()));
    }
    if meta.is_dir() {
        return Ok(Side::Dir);
    }
    let bytes = match rewrite {
        Some((rewrite, rw)) => copy::desired_bytes(p, rewrite, rw)?,
        None => std::fs::read(p)?,
    };
    Ok(match String::from_utf8(bytes) {
        Ok(s) if !s.contains('\0') => Side::Text(s),
        _ => Side::Binary(meta.len()),
    })
}

fn text_diff(out: &mut impl Write, from: &str, to: &str, a: &str, b: &str) -> Result<()> {
    if a == b {
        return Ok(());
    }
    let (la, lb): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    if la.len() > MAX_DIFF_LINES || lb.len() > MAX_DIFF_LINES {
        writeln!(
            out,
            "  (too large to diff: {} and {} lines)",
            la.len(),
            lb.len()
        )?;
        return Ok(());
    }
    writeln!(out, "{}", style(format!("--- {from}")).red())?;
    writeln!(out, "{}", style(format!("+++ {to}")).green())?;
    for line in unified(&la, &lb, CONTEXT) {
        let styled = match line.as_bytes().first() {
            Some(b'-') => style(line).red().to_string(),
            Some(b'+') => style(line).green().to_string(),
            Some(b'@') => style(line).cyan().to_string(),
            _ => line,
        };
        writeln!(out, "{styled}")?;
    }
    if a.lines().eq(b.lines()) {
        writeln!(out, "  (line endings differ)")?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Same,
    Del,
    Ins,
}

/// A unified diff of `a` → `b` (hunk headers and lines), with `ctx` lines of
/// context, from a longest-common-subsequence alignment.
fn unified(a: &[&str], b: &[&str], ctx: usize) -> Vec<String> {
    // Skip the common prefix and suffix; align only the middle.
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (n, m) = (ma.len(), mb.len());
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if ma[i] == mb[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    // (op, line in a, line in b), 0-based.
    let mut ops: Vec<(Op, usize, usize)> = (0..pre).map(|k| (Op::Same, k, k)).collect();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && ma[i] == mb[j] {
            ops.push((Op::Same, pre + i, pre + j));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[at(i + 1, j)] >= lcs[at(i, j + 1)]) {
            // Deletions first, as `diff -u` prints them.
            ops.push((Op::Del, pre + i, pre + j));
            i += 1;
        } else {
            ops.push((Op::Ins, pre + i, pre + j));
            j += 1;
        }
    }
    for k in 0..suf {
        ops.push((Op::Same, a.len() - suf + k, b.len() - suf + k));
    }

    let changed: Vec<usize> = (0..ops.len()).filter(|&k| ops[k].0 != Op::Same).collect();
    let mut out = Vec::new();
    let mut k = 0;
    while k < changed.len() {
        // Extend the hunk while the next change is within 2 * ctx lines.
        let mut last = k;
        while last + 1 < changed.len() && changed[last + 1] - changed[last] <= 2 * ctx + 1 {
            last += 1;
        }
        let start = changed[k].saturating_sub(ctx);
        let end = (changed[last] + ctx + 1).min(ops.len());
        let hunk = &ops[start..end];
        let a_len = hunk.iter().filter(|o| o.0 != Op::Ins).count();
        let b_len = hunk.iter().filter(|o| o.0 != Op::Del).count();
        let (a0, b0) = (hunk[0].1, hunk[0].2);
        let range = |s: usize, len: usize| {
            if len == 0 {
                format!("{s},0")
            } else {
                format!("{},{len}", s + 1)
            }
        };
        out.push(format!("@@ -{} +{} @@", range(a0, a_len), range(b0, b_len)));
        for &(op, ai, bi) in hunk {
            out.push(match op {
                Op::Same => format!(" {}", a[ai]),
                Op::Del => format!("-{}", a[ai]),
                Op::Ins => format!("+{}", b[bi]),
            });
        }
        k = last + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_diff_hunks() {
        let a: Vec<&str> = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj".lines().collect();
        let b: Vec<&str> = "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk".lines().collect();
        let got = unified(&a, &b, 1);
        assert_eq!(
            got,
            vec![
                "@@ -2,3 +2,3 @@",
                " b",
                "-c",
                "+C",
                " d",
                "@@ -10,1 +10,2 @@",
                " j",
                "+k"
            ]
        );
        // Close changes share one hunk.
        let got = unified(&["x", "1", "y"], &["X", "1", "Y"], 1);
        assert_eq!(got, vec!["@@ -1,3 +1,3 @@", "-x", "+X", " 1", "-y", "+Y"]);
        // Against an empty side.
        assert_eq!(unified(&[], &["n"], 3), vec!["@@ -0,0 +1,1 @@", "+n"]);
        assert_eq!(unified(&["o"], &[], 3), vec!["@@ -1,1 +0,0 @@", "-o"]);
        assert!(unified(&["same"], &["same"], 3).is_empty());
    }
}
