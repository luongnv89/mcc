//! `mcc usage` with other machines (#130): `[machines.NAME]` read over
//! SSH, one table per machine, one live lookup per account, the newest
//! numbers on every row of an account. Every machine here is a sandbox of
//! its own behind the fake ssh (`tests/common`).

mod common;

use std::collections::HashMap;

use common::{
    HOUR_MS, Sandbox, UsageStub, by_name, json, now_ms, rfc3339_in, seed_usage_cache, stdout_of,
    usage_sandbox, usage_stub, with_profile,
};
use predicates::prelude::*;

/// A machine: the default profile signed in to `default` (email, token)
/// or signed out, and `profiles` (name, email, token) signed in. Tokens
/// are valid for an hour, so nothing is refreshed.
fn machine(default: Option<(&str, &str)>, profiles: &[(&str, &str, &str)]) -> Sandbox {
    let sb = Sandbox::new();
    sb.seed_default();
    match default {
        Some((email, token)) => {
            sb.sign_in(".claude", ".claude.json", email);
            sb.seed_oauth(".claude", token, now_ms() + HOUR_MS);
        }
        None => sb.write(".claude.json", "{}"),
    }
    for (name, email, token) in profiles {
        with_profile(&sb, name, "--fresh");
        let dir = format!(".mcc/profiles/{name}");
        sb.sign_in(&dir, &format!("{dir}/.claude.json"), email);
        sb.seed_oauth(&dir, token, now_ms() + HOUR_MS);
    }
    sb
}

/// The usage endpoint, answering every token with a weekly window of the
/// percent `weekly` gives it (50 when not listed).
fn stub(weekly: &[(&str, f64)]) -> UsageStub {
    let weekly: HashMap<String, f64> = weekly.iter().map(|(t, p)| (t.to_string(), *p)).collect();
    let (five, week) = (rfc3339_in(2 * 3600), rfc3339_in(3 * 86400));
    UsageStub::start(move |req| {
        let token = req
            .authorization
            .as_deref()
            .and_then(|a| a.strip_prefix("Bearer "))
            .unwrap_or_default();
        let pct = weekly.get(token).copied().unwrap_or(50.0);
        (
            200,
            format!(
                r#"{{"five_hour":{{"utilization":5,"resets_at":"{five}"}},"seven_day":{{"utilization":{pct},"resets_at":"{week}"}}}}"#
            ),
        )
    })
}

/// `mcc ARGS` against `stub`, its stdout; it must succeed.
fn run(sb: &Sandbox, stub: &UsageStub, args: &[&str]) -> String {
    stdout_of(sb.mcc().env("MCC_USAGE_URL", &stub.url).args(args))
}

/// `mcc usage`'s table output with each machine heading's rule taken off
/// (`── lab (lab-host) · updated 5m ago ───` → `lab (lab-host) · updated
/// 5m ago`), for tests about what a heading says.
fn table(sb: &Sandbox, stub: &UsageStub, args: &[&str]) -> String {
    unruled(&run(sb, stub, args))
}

fn unruled(out: &str) -> String {
    out.lines()
        .map(|l| match l.strip_prefix("── ") {
            Some(h) => h.trim_end_matches('─').trim_end(),
            None => l,
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

fn v2(sb: &Sandbox, stub: &UsageStub, args: &[&str]) -> serde_json::Value {
    json(run(sb, stub, &[&["usage", "--json-v2"], args].concat()).as_bytes())
}

/// Machine `name` of a `--json-v2` answer.
fn machine_of<'a>(v: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    v["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .unwrap_or_else(|| panic!("no machine {name}: {v}"))
}

fn rows(v: &serde_json::Value) -> &[serde_json::Value] {
    v["profiles"].as_array().unwrap()
}

fn weekly(row: &serde_json::Value) -> f64 {
    row["usage"]["weekly"]["percent"]
        .as_f64()
        .unwrap_or_else(|| panic!("no weekly usage: {row}"))
}

/// The ssh runs that went to `host`, their remote command only.
fn commands_to(sb: &Sandbox, host: &str) -> Vec<String> {
    sb.ssh_log()
        .iter()
        .filter_map(|l| {
            let a: Vec<&str> = l.split('\t').collect();
            (a.get(6) == Some(&host)).then(|| a[7..].join("\t"))
        })
        .collect()
}

#[test]
fn every_machine_gets_its_own_table_next_to_this_ones() {
    // AC1 + AC2: this machine's table, then one per machine, each headed
    // by its own last update; ALSO ON names where a shared account is.
    let local = machine(Some(("ann@example.com", "tok-local")), &[]);
    let lab = machine(
        Some(("bob@example.com", "tok-lab")),
        &[("work", "Ann@Example.com ", "tok-lab-work")],
    );
    seed_usage_cache(&local, "default", None, Some((20.0, 86400)), 30 * 60);
    seed_usage_cache(&lab, "default", None, Some((30.0, 86400)), 5 * 60);
    seed_usage_cache(&lab, "work", None, Some((40.0, 86400)), 2 * 3600);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);

    let out = table(&local, &stub, &["usage"]);
    // T4: options first, then `--` right before the host.
    assert_eq!(
        local.ssh_log(),
        [
            "ssh\t-o\tBatchMode=yes\t-o\tConnectTimeout=10\t--\tlab-host\t'mcc' 'usage' '--json-v2' '--machine-protocol'"
        ]
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "local · updated 30m ago", "{out}");
    let lab_head = lines
        .iter()
        .position(|l| *l == "lab (lab-host) · updated 5m ago")
        .unwrap_or_else(|| panic!("no lab heading\n{out}"));
    // Both tables have the ALSO ON column: the local default and lab's
    // `work` are the same account (emails match whatever their case).
    assert!(
        lines[1].starts_with("# PROFILE    ALSO ON  LAST ACTIVE"),
        "{out}"
    );
    assert!(lines[2].starts_with("1 default ★  lab      "), "{out}");
    assert!(
        lines[lab_head + 1].starts_with("# PROFILE    ALSO ON  "),
        "{out}"
    );
    // lab's `work` shows ann's newer numbers from here (20%, 30 minutes
    // old) in place of its own (40%, 2 hours old), and sorts by them.
    let lab_rows = &lines[lab_head + 2..];
    assert!(lab_rows[0].starts_with("1 work       local    "), "{out}");
    assert!(
        lab_rows[0].contains(" 20% ") && lab_rows[0].ends_with("30m ago"),
        "{out}"
    );
    assert!(lab_rows[1].starts_with("2 default ★  —        "), "{out}");
    // Cached reads only: nothing was looked up, on any machine.
    assert!(stub.requests().is_empty());
    assert_eq!(
        commands_to(&local, "lab-host"),
        ["'mcc' 'usage' '--json-v2' '--machine-protocol'"]
    );
    // No email in sight, here or there.
    assert!(!out.to_lowercase().contains("example.com"), "{out}");
}

#[test]
fn json_v2_adds_each_machine_with_its_own_update_time() {
    let local = machine(Some(("ann@example.com", "tok-local")), &[]);
    let lab = machine(
        Some(("bob@example.com", "tok-lab")),
        &[("work", "ann@example.com", "tok-lab-work")],
    );
    seed_usage_cache(&lab, "default", None, Some((30.0, 86400)), 5 * 60);
    seed_usage_cache(&lab, "work", None, Some((40.0, 86400)), 2 * 3600);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);

    let v = v2(&local, &stub, &[]);
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["schema", "profiles", "updated_at", "machines"]);
    // Nothing cached here: this machine has no update time of its own.
    assert_eq!(v["updated_at"], serde_json::Value::Null);
    let m = machine_of(&v, "lab");
    let keys: Vec<&str> = m.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["name", "host", "updated_at", "error", "profiles"]);
    assert_eq!(m["host"], "lab-host");
    assert_eq!(m["error"], serde_json::Value::Null);
    // Its own newest numbers: lab's default, 5 minutes ago.
    assert_eq!(
        m["updated_at"],
        by_name(rows(m), "default")["usage"]["updated_at"]
    );
    let ann_here = by_name(rows(&v), "default");
    let ann_there = by_name(rows(m), "work");
    let id = ann_here["account_id"].as_str().unwrap();
    assert!(
        id.len() == 64
            && id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "{id}"
    );
    assert_eq!(ann_there["account_id"], id);
    assert_ne!(by_name(rows(m), "default")["account_id"], id);
    assert_eq!(ann_here["also_on"], serde_json::json!(["lab"]));
    assert_eq!(ann_there["also_on"], serde_json::json!(["local"]));
    assert_eq!(
        by_name(rows(m), "default")["also_on"],
        serde_json::json!([])
    );
    assert_eq!(ann_here["account"], serde_json::Value::Null);
}

#[test]
fn all_refreshes_every_machine() {
    // AC3: --all reaches every machine, each for its own accounts.
    let local = machine(Some(("ann@example.com", "tok-local")), &[]);
    let lab = machine(Some(("bob@example.com", "tok-lab")), &[]);
    let box_ = machine(None, &[("work", "cat@example.com", "tok-box-work")]);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    local.add_machine("box", "box-host", Some(&box_), None);
    let stub = stub(&[
        ("tok-local", 11.0),
        ("tok-lab", 22.0),
        ("tok-box-work", 33.0),
    ]);

    let o = local
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(["usage", "--all", "--json-v2"])
        .assert()
        .success()
        .stderr(predicate::str::contains("--all also refreshes them"));
    let v = json(&o.get_output().stdout);
    assert_eq!(weekly(by_name(rows(&v), "default")), 11.0);
    assert_eq!(
        weekly(by_name(rows(machine_of(&v, "lab")), "default")),
        22.0
    );
    assert_eq!(weekly(by_name(rows(machine_of(&v, "box")), "work")), 33.0);
    assert_eq!(stub.tokens(), ["tok-box-work", "tok-lab", "tok-local"]);
    // Each machine read from its cache first, then refreshed. box's
    // signed-out default is looked up there too (it has no account to share).
    assert_eq!(
        commands_to(&local, "lab-host"),
        [
            "'mcc' 'usage' '--json-v2' '--machine-protocol'",
            "'mcc' 'usage' '--all' '--json-v2' '--machine-protocol' '--only' 'default'"
        ]
    );
    assert_eq!(
        commands_to(&local, "box-host")[1],
        "'mcc' 'usage' '--all' '--json-v2' '--machine-protocol' '--only' 'default,work'"
    );
    // What the other machines looked up is in their own caches now.
    assert!(lab.exists(".mcc/state/default/usage.json"));
    assert!(box_.exists(".mcc/state/work/usage.json"));
}

#[test]
fn an_account_on_several_machines_is_looked_up_once() {
    // AC4: ann is here and on both machines; dan is on both machines.
    // One --all asks once for each: ann here, dan on the first machine in
    // the config (box, before lab).
    let local = machine(Some(("ann@example.com", "tok-ann-local")), &[]);
    let lab = machine(
        Some(("ann@example.com", "tok-ann-lab")),
        &[("side", "dan@example.com", "tok-dan-lab")],
    );
    let box_ = machine(
        Some(("dan@example.com", "tok-dan-box")),
        &[("work", "ANN@example.com", "tok-ann-box")],
    );
    local.add_machine("lab", "lab-host", Some(&lab), None);
    local.add_machine("box", "box-host", Some(&box_), None);
    let stub = stub(&[("tok-ann-local", 61.0), ("tok-dan-box", 72.0)]);

    let v = v2(&local, &stub, &["--all"]);
    assert_eq!(stub.tokens(), ["tok-ann-local", "tok-dan-box"]);
    assert_eq!(
        commands_to(&local, "box-host")[1],
        "'mcc' 'usage' '--all' '--json-v2' '--machine-protocol' '--only' 'default'"
    );
    // lab has nothing of its own to look up: no second run there.
    assert_eq!(commands_to(&local, "lab-host").len(), 1);
    // Every row of each account shows the one lookup (AC5).
    let (lab_v, box_v) = (machine_of(&v, "lab"), machine_of(&v, "box"));
    for ann in [
        by_name(rows(&v), "default"),
        by_name(rows(lab_v), "default"),
        by_name(rows(box_v), "work"),
    ] {
        assert_eq!(weekly(ann), 61.0, "{ann}");
    }
    for dan in [
        by_name(rows(lab_v), "side"),
        by_name(rows(box_v), "default"),
    ] {
        assert_eq!(weekly(dan), 72.0, "{dan}");
    }
}

#[test]
fn an_account_signed_out_here_is_looked_up_where_its_login_still_works() {
    // AC4: of dan's rows, box's login ran out, so lab looks dan up.
    let local = machine(Some(("ann@example.com", "tok-ann")), &[]);
    let box_ = machine(Some(("dan@example.com", "tok-dan-box")), &[]);
    box_.seed_login(
        ".claude",
        "tok-dan-box",
        now_ms() + HOUR_MS,
        now_ms() - HOUR_MS,
    );
    let lab = machine(Some(("dan@example.com", "tok-dan-lab")), &[]);
    local.add_machine("box", "box-host", Some(&box_), None);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);
    run(&local, &stub, &["usage", "--all", "--json-v2"]);
    assert_eq!(stub.tokens(), ["tok-ann", "tok-dan-lab"]);
}

#[test]
fn every_row_of_an_account_shows_its_newest_numbers() {
    // AC5: ann's newest numbers are lab's, a minute old; they replace the
    // older ones here and on box, and land in this machine's cache, so
    // `mcc list` shows them too.
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(Some(("ann@example.com", "tok-2")), &[]);
    let box_ = machine(None, &[("work", "ann@example.com", "tok-3")]);
    seed_usage_cache(&local, "default", None, Some((20.0, 86400)), 30 * 60);
    seed_usage_cache(&lab, "default", None, Some((70.0, 86400)), 60);
    seed_usage_cache(&box_, "work", None, Some((40.0, 86400)), 10 * 60);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    local.add_machine("box", "box-host", Some(&box_), None);
    let stub = stub(&[]);

    let v = v2(&local, &stub, &[]);
    let (lab_v, box_v) = (machine_of(&v, "lab"), machine_of(&v, "box"));
    let newest = &by_name(rows(lab_v), "default")["usage"];
    for ann in [by_name(rows(&v), "default"), by_name(rows(box_v), "work")] {
        assert_eq!(&ann["usage"], newest, "{ann}");
    }
    assert_eq!(weekly(by_name(rows(&v), "default")), 70.0);
    // Each machine's own update time is its own, not the newer one.
    assert_ne!(box_v["updated_at"], lab_v["updated_at"]);
    assert_eq!(box_v["updated_at"].as_str().map(str::len), Some(20));
    // Saved here with lab's time, not a new one.
    let cached = json(local.read(".mcc/state/default/usage.json").as_bytes());
    assert_eq!(cached["seven_day"]["used_percentage"], 70.0);
    assert_eq!(cached["updated_at"], newest["updated_at"]);
    let list = json(stdout_of(local.mcc().args(["list", "--json"])).as_bytes());
    assert_eq!(
        by_name(list.as_array().unwrap(), "default")["usage"]["weekly"]["percent"],
        70.0
    );
    // The other way round, an older point elsewhere changes nothing here.
    seed_usage_cache(&local, "default", None, Some((10.0, 86400)), 10);
    let before = local.read(".mcc/state/default/usage.json");
    let v = v2(&local, &stub, &[]);
    assert_eq!(weekly(by_name(rows(&v), "default")), 10.0);
    assert_eq!(
        weekly(by_name(rows(machine_of(&v, "lab")), "default")),
        10.0
    );
    assert_eq!(local.read(".mcc/state/default/usage.json"), before);
    // Nothing was looked up live for any of it.
    assert!(stub.requests().is_empty());
}

#[test]
fn a_machine_that_fails_shows_why_while_the_others_show() {
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(Some(("bob@example.com", "tok-2")), &[]);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    local.add_machine("down", "down", None, None);
    local.add_machine("old", "old", None, None);
    local.add_machine("garbage", "garbage", None, None);
    local.add_machine("nowhere", "no-such-host", None, None);
    let stub = stub(&[]);

    let out = table(&local, &stub, &["usage"]);
    for line in [
        "down (down) · updated — · ssh could not connect to down without a prompt: check that `ssh down true` works with your key",
        "old (old) · updated — · the mcc on old is too old to share its usage: upgrade it there",
        "garbage (garbage) · updated — · unexpected answer from the mcc on garbage (not JSON): use the same mcc version on both machines",
        "nowhere (no-such-host) · updated — · ssh could not connect to no-such-host without a prompt: check that `ssh no-such-host true` works with your key",
        "lab (lab-host) · updated —",
    ] {
        assert!(out.lines().any(|l| l == line), "no {line:?}\n{out}");
    }
    let v = v2(&local, &stub, &[]);
    for (name, code) in [
        ("down", "unreachable"),
        ("old", "outdated"),
        ("garbage", "incompatible"),
        ("lab", ""),
    ] {
        let m = machine_of(&v, name);
        assert_eq!(m["error"].as_str().unwrap_or_default(), code, "{m}");
    }
    assert_eq!(rows(machine_of(&v, "lab")).len(), 1);
    assert!(rows(machine_of(&v, "down")).is_empty());
    // --all too: what can be refreshed is, and the command succeeds.
    let v = v2(&local, &stub, &["--all"]);
    assert_eq!(machine_of(&v, "down")["error"], "unreachable");
    assert_eq!(stub.tokens(), ["tok-1", "tok-2"]);
}

#[test]
fn an_unusable_machine_entry_is_reported_and_never_reaches_ssh() {
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    local.add_machine("bad", "-oProxyCommand=touch pwned", None, None);
    let stub = stub(&[]);
    let v = v2(&local, &stub, &[]);
    assert_eq!(machine_of(&v, "bad")["error"], "invalid_config");
    assert!(local.ssh_log().is_empty());
    assert!(!local.exists("pwned"));
}

#[test]
fn an_mcc_path_with_spaces_and_quotes_reaches_the_other_machine_whole() {
    // Each word of the remote command is quoted for the shell ssh runs it
    // with: a path with a space, quotes, `$`, `;` and backticks is one word.
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(Some(("bob@example.com", "tok-2")), &[]);
    let dir = lab.path("my tools/it's $HOME;`touch pwned`");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(common::mcc_bin(), dir.join("mcc")).unwrap();
    let path = dir.join("mcc").display().to_string();
    local.add_machine("lab", "lab-host", Some(&lab), Some(&path));
    let stub = stub(&[]);
    let v = v2(&local, &stub, &[]);
    let m = machine_of(&v, "lab");
    assert_eq!(m["error"], serde_json::Value::Null, "{v}");
    assert_eq!(rows(m).len(), 1);
    assert!(!lab.exists("pwned") && !local.exists("pwned"));

    // An mcc missing there says how to fix it.
    let other = machine(Some(("ann@example.com", "tok-1")), &[]);
    other.add_machine("lab", "lab-host", Some(&lab), Some("/nowhere/mcc"));
    let out = run(&other, &stub, &["usage"]);
    assert!(
        out.contains("no mcc on lab-host's PATH over SSH: set mcc = \"/full/path/to/mcc\" under [machines.lab]"),
        "{out}"
    );
}

#[test]
fn a_profile_name_another_machine_reports_must_be_a_valid_alias() {
    // T3: a hand-written profile name on lab that isn't an alias never
    // reaches a command line here: its row is dropped.
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(Some(("bob@example.com", "tok-2")), &[]);
    let cfg = if lab.exists(".mcc/config.toml") {
        lab.read(".mcc/config.toml")
    } else {
        String::new()
    };
    let mut evil = String::new();
    for (i, name) in ["x;touch pwned", "$(touch pwned)", "`touch pwned`"]
        .iter()
        .enumerate()
    {
        let dir = format!(".mcc/profiles/evil{i}");
        lab.sign_in(
            &dir,
            &format!("{dir}/.claude.json"),
            &format!("evil{i}@example.com"),
        );
        lab.seed_oauth(&dir, &format!("tok-evil{i}"), now_ms() + HOUR_MS);
        evil.push_str(&format!(
            "\n[profiles.{}]\ndir = {}\nmode = \"fresh\"\ncreated_at = \"t\"\n",
            toml::Value::String(name.to_string()),
            toml::Value::String(lab.path(&dir).display().to_string()),
        ));
    }
    lab.write(".mcc/config.toml", &format!("{cfg}{evil}"));
    // lab itself lists them.
    let there =
        json(stdout_of(lab.mcc().args(["usage", "--json-v2", "--machine-protocol"])).as_bytes());
    assert_eq!(rows(&there).len(), 4, "{there}");
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);

    let v = v2(&local, &stub, &["--all"]);
    let names: Vec<&str> = rows(machine_of(&v, "lab"))
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["default"]);
    let log = local.ssh_log().join("\n");
    assert!(!log.contains("pwned") && !log.contains("evil"), "{log}");
    assert_eq!(
        commands_to(&local, "lab-host")[1],
        "'mcc' 'usage' '--all' '--json-v2' '--machine-protocol' '--only' 'default'"
    );
    assert_eq!(stub.tokens(), ["tok-1", "tok-2"]);
    assert!(!lab.exists("pwned") && !local.exists("pwned"));
}

#[test]
fn another_machine_reads_its_cache_and_never_its_own_machines() {
    // lab looks usage up live by default and has machines of its own: read
    // from here, it neither looks anything up nor reads its machines.
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(
        Some(("bob@example.com", "tok-2")),
        &[("show", "eve@example.com", "tok-3")],
    );
    common::enable_live_usage(&lab);
    lab.write(
        ".mcc/config.toml",
        &lab.read(".mcc/config.toml")
            .replace("[usage]\n", "[usage]\nshow_email = true\n"),
    );
    lab.add_machine("back", "local-host", Some(&local), None);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);

    let v = v2(&local, &stub, &[]);
    assert!(stub.requests().is_empty());
    assert!(lab.ssh_log().is_empty());
    let m = machine_of(&v, "lab");
    for row in rows(m) {
        assert_eq!(row["account"], serde_json::Value::Null, "{row}");
    }
    // --all asks lab for its accounts once, and lab still reads no machine.
    run(&local, &stub, &["usage", "--all"]);
    assert_eq!(stub.tokens(), ["tok-1", "tok-2", "tok-3"]);
    assert!(lab.ssh_log().is_empty());
}

#[test]
fn reading_other_machines_is_explained_once() {
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(Some(("bob@example.com", "tok-2")), &[]);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);
    let note = "mcc usage reads the other machines set up under [machines]";
    let o = local
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .args(["usage", "--json-v2"])
        .assert()
        .success()
        .stderr(predicate::str::contains(note));
    json(&o.get_output().stdout);
    assert!(
        local
            .read(".mcc/config.toml")
            .contains("machines_disclosed_at = ")
    );
    local
        .mcc()
        .env("MCC_USAGE_URL", &stub.url)
        .arg("usage")
        .assert()
        .success()
        .stderr(predicate::str::contains(note).not());
}

#[test]
fn only_these_profiles_with_all() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let v = json(run(&sb, &stub, &["usage", "--all", "--only", "work", "--json"]).as_bytes());
    let names: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["work"]);
    assert_eq!(stub.tokens(), ["test-token-work"]);
    // `claude` stands for the default; a profile that isn't here is skipped.
    let v = json(
        run(
            &sb,
            &stub,
            &["usage", "--all", "--only", "claude,gone", "--json"],
        )
        .as_bytes(),
    );
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["name"], "default");
    for args in [
        &["usage", "--only", "work"][..],
        &["usage", "--all", "--only", "x;y"][..],
        &["usage", "--all", "--only", "Work"][..],
        &["usage", "work", "--all", "--only", "work"][..],
        &["usage", "--machine-protocol"][..],
    ] {
        sb.mcc().args(args).assert().code(2);
    }
}

#[test]
fn the_machine_protocol_marks_its_answer_and_hashes_accounts() {
    let sb = usage_sandbox();
    let stub = usage_stub();
    let v = json(
        run(
            &sb,
            &stub,
            &["usage", "--json-v2", "--machine-protocol", "--show-email"],
        )
        .as_bytes(),
    );
    assert_eq!(v["protocol"], "mcc-machines/1");
    let default = by_name(rows(&v), "default");
    assert_eq!(default["account"], serde_json::Value::Null);
    assert_eq!(default["account_id"].as_str().map(str::len), Some(64));
    assert_eq!(
        by_name(rows(&v), "lab")["account_id"],
        serde_json::Value::Null
    );
    // The cache, though `[usage] live = true` here.
    assert!(stub.requests().is_empty());
    // Hidden from the help.
    let help = stdout_of(sb.mcc().args(["usage", "--help"]));
    assert!(!help.contains("machine-protocol"), "{help}");
    assert!(help.contains("--only <PROFILES>"), "{help}");
}

#[test]
fn without_machines_ssh_never_runs_and_nothing_changes() {
    // T5: the shape and the requests of today, with no [machines].
    let sb = usage_sandbox();
    let stub = usage_stub();
    let v = json(run(&sb, &stub, &["usage", "--json-v2"]).as_bytes());
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["schema", "profiles"]);
    for row in rows(&v) {
        assert!(
            row.get("account_id").is_none() && row.get("also_on").is_none(),
            "{row}"
        );
        assert_eq!(row["usage"]["updated_at"], serde_json::Value::Null, "{row}");
    }
    let bare = json(run(&sb, &stub, &["usage", "--json"]).as_bytes());
    assert_eq!(&bare, &v["profiles"]);
    let out = run(&sb, &stub, &["usage"]);
    assert!(out.starts_with("# PROFILE    LAST ACTIVE"), "{out}");
    assert!(
        !out.contains("local ·") && !out.contains("ALSO ON"),
        "{out}"
    );
    // --all asks once per signed-in profile, as before.
    let before = stub.requests().len();
    run(&sb, &stub, &["usage", "--all", "--json-v2"]);
    assert_eq!(stub.requests().len() - before, 2);
    assert!(sb.ssh_log().is_empty());
    assert!(!sb.read(".mcc/config.toml").contains("machines"));

    // With machines, --json (schema 1) and one profile still read only here.
    let lab = machine(Some(("bob@example.com", "tok-2")), &[]);
    sb.add_machine("lab", "lab-host", Some(&lab), None);
    let again = json(run(&sb, &stub, &["usage", "--json"]).as_bytes());
    assert_eq!(
        again.as_array().unwrap().len(),
        bare.as_array().unwrap().len()
    );
    assert!(again[0].get("account_id").is_none());
    run(&sb, &stub, &["usage", "work", "--json-v2"]);
    sb.mcc()
        .env("MCC_NO_USAGE", "1")
        .args(["usage", "--json-v2"])
        .assert()
        .success();
    assert!(sb.ssh_log().is_empty());
}

#[test]
fn another_machines_devin_row_is_shown_as_it_was_read() {
    // Devin isn't an account machines share: its row on lab is shown in a
    // Devin block under lab's table, and --all never asks lab for it.
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(Some(("bob@example.com", "tok-2")), &[]);
    lab.seed_devin_login(common::DEVIN_TOKEN);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);
    let v = v2(&local, &stub, &["--all"]);
    let devin = by_name(rows(machine_of(&v, "lab")), "devin");
    assert_eq!(devin["provider"], "devin");
    assert_eq!(devin["usage"]["reason"], "not_looked_up");
    assert_eq!(
        commands_to(&local, "lab-host")[1],
        "'mcc' 'usage' '--all' '--json-v2' '--machine-protocol' '--only' 'default'"
    );
    let out = run(&local, &stub, &["usage"]);
    let lab_part = &out[out.find("lab (lab-host)").unwrap()..];
    assert!(lab_part.contains("\n# PROVIDER  "), "{out}");
    assert!(lab_part.contains("\n2 devin     — (not looked up"), "{out}");
}

#[test]
fn the_same_email_in_another_organization_is_another_account() {
    // A personal plan here and a work seat on lab, on one email: limits of
    // their own, so each is looked up, and neither shows the other's.
    let local = machine(Some(("ann@example.com", "tok-personal")), &[]);
    let lab = machine(Some(("ann@example.com", "tok-work")), &[]);
    for (sb, org) in [(&local, "org-personal"), (&lab, "org-acme")] {
        sb.write(
            ".claude.json",
            &format!(r#"{{"oauthAccount":{{"emailAddress":"ann@example.com","organizationUuid":"{org}"}}}}"#),
        );
    }
    seed_usage_cache(&local, "default", None, Some((20.0, 86400)), 30 * 60);
    seed_usage_cache(&lab, "default", None, Some((70.0, 86400)), 60);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);
    let before = local.read(".mcc/state/default/usage.json");

    let v = v2(&local, &stub, &[]);
    let (here, there) = (
        by_name(rows(&v), "default"),
        by_name(rows(machine_of(&v, "lab")), "default"),
    );
    assert_ne!(here["account_id"], there["account_id"]);
    assert_eq!(here["also_on"], serde_json::json!([]));
    assert_eq!(weekly(here), 20.0);
    assert_eq!(weekly(there), 70.0);
    assert_eq!(local.read(".mcc/state/default/usage.json"), before);
    run(&local, &stub, &["usage", "--all"]);
    assert_eq!(stub.tokens(), ["tok-personal", "tok-work"]);
}

#[test]
fn what_another_machine_sends_is_never_shown_raw() {
    // A changed mcc there could send an account (an email, or terminal
    // escapes) and its own also_on: neither is kept.
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let reply = serde_json::json!({
        "schema": 2,
        "protocol": "mcc-machines/1",
        "profiles": [{
            "name": "work", "command": "work", "is_default": false,
            "account": "evil@example.com\u{1b}]0;pwned\u{7}\u{1b}[2J",
            "usage": {"status": "unavailable", "reason": "no_data", "five_hour": null,
                      "weekly": null, "source": "cache", "updated_at": null},
            "last_activity": null, "login_expires_at": null, "login_expires_source": null,
            "login_warning": false, "is_global_default": false, "provider": "claude",
            "account_id": null, "also_on": ["\u{1b}[31mred"]
        }, {
            "name": "devin", "command": "devin", "is_default": false, "account": null,
            "usage": {"status": "unavailable", "reason": "not_looked_up", "five_hour": null,
                      "weekly": null, "source": null, "updated_at": null},
            "last_activity": null, "login_expires_at": null, "login_expires_source": null,
            "login_warning": false, "is_global_default": false, "provider": "devin",
            "account_id": null, "also_on": ["local"]
        }]
    });
    local.write("ssh-canned", &reply.to_string());
    local.add_machine("box", "canned", None, None);
    let stub = stub(&[]);
    let out = table(&local, &stub, &["usage", "--show-email"]);
    assert!(out.contains("box (canned) · updated —"), "{out}");
    assert!(!out.contains('\u{1b}') && !out.contains("evil"), "{out:?}");
    let v = v2(&local, &stub, &["--show-email"]);
    let row = by_name(rows(machine_of(&v, "box")), "work");
    assert_eq!(row["account"], serde_json::Value::Null);
    assert_eq!(row["also_on"], serde_json::json!([]));
    let devin = by_name(rows(machine_of(&v, "box")), "devin");
    assert!(devin.get("also_on").is_none(), "{devin}");
}

#[test]
fn this_machines_heading_shows_its_own_update_time() {
    // AC2: ann's newest numbers are lab's, a minute old, and every row of
    // hers shows them; this machine's heading (and `updated_at`) still
    // says when its own numbers are from, 30 minutes ago, run after run,
    // though its cache now holds lab's numbers (AC5).
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(Some(("ann@example.com", "tok-2")), &[]);
    seed_usage_cache(&lab, "default", None, Some((70.0, 86400)), 60);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);
    seed_usage_cache(&local, "default", None, Some((20.0, 86400)), 30 * 60);
    let own = json(local.read(".mcc/state/default/usage.json").as_bytes())["updated_at"].clone();

    for _ in 0..2 {
        let out = table(&local, &stub, &["usage"]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "local · updated 30m ago", "{out}");
        assert!(
            lines[2].contains(" 70% ") && lines[2].ends_with("1m ago"),
            "{out}"
        );
        assert!(
            out.lines().any(|l| l == "lab (lab-host) · updated 1m ago"),
            "{out}"
        );
        let v = v2(&local, &stub, &[]);
        assert_eq!(weekly(by_name(rows(&v), "default")), 70.0);
        assert_eq!(v["updated_at"], own, "{v}");
        assert_ne!(v["updated_at"], machine_of(&v, "lab")["updated_at"]);
        let cached = json(local.read(".mcc/state/default/usage.json").as_bytes());
        assert_eq!(cached["seven_day"]["used_percentage"], 70.0, "{cached}");
        assert_eq!(cached["own_updated_at"], own, "{cached}");
    }
}

#[test]
fn another_machine_tells_its_own_update_time_from_numbers_it_got() {
    // lab's cache holds numbers a third machine had, a minute old, beside
    // lab's own last update, two hours ago: they are shown, under a heading
    // that says when lab itself last updated.
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(Some(("dan@example.com", "tok-2")), &[]);
    seed_usage_cache(&lab, "default", None, Some((70.0, 86400)), 60);
    let path = ".mcc/state/default/usage.json";
    let mut record = json(lab.read(path).as_bytes());
    let own = rfc3339_in(-2 * 3600);
    record["own_updated_at"] = own.clone().into();
    lab.write(path, &record.to_string());
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);

    // lab's own answers: only the machine reading it gets its own time.
    let plain = json(stdout_of(lab.mcc().args(["usage", "--json-v2"])).as_bytes());
    assert!(rows(&plain)[0].get("own_updated_at").is_none(), "{plain}");
    let reply =
        json(stdout_of(lab.mcc().args(["usage", "--json-v2", "--machine-protocol"])).as_bytes());
    assert_eq!(rows(&reply)[0]["own_updated_at"], own.as_str(), "{reply}");

    let v = v2(&local, &stub, &[]);
    let lab_v = machine_of(&v, "lab");
    assert_eq!(lab_v["updated_at"], own.as_str(), "{v}");
    assert_eq!(weekly(by_name(rows(lab_v), "default")), 70.0, "{v}");
    assert!(
        by_name(rows(lab_v), "default")
            .get("own_updated_at")
            .is_none()
    );
    let out = table(&local, &stub, &["usage"]);
    assert!(
        out.lines().any(|l| l == "lab (lab-host) · updated 2h ago"),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.contains(" 70% ") && l.ends_with("1m ago")),
        "{out}"
    );
}

#[test]
fn a_time_ahead_of_this_clock_never_wins() {
    // Another machine whose clock is ahead (or that sends a year-9999
    // time) can't pass its numbers off as the newest: this machine's
    // fresher point is shown everywhere and its cache is left alone.
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    seed_usage_cache(&local, "default", None, Some((20.0, 86400)), 5 * 60);
    // What this machine answers, as another one would send it, with
    // ann's numbers changed and dated in the future.
    let mut reply = json(
        stdout_of(
            local
                .mcc()
                .args(["usage", "--json-v2", "--machine-protocol"]),
        )
        .as_bytes(),
    );
    let mine = reply["profiles"][0].clone();
    assert_eq!(
        mine["account_id"].as_str().map(str::len),
        Some(64),
        "{mine}"
    );
    let mut rows_there = Vec::new();
    for (name, at) in [
        ("work", rfc3339_in(86400)),
        ("side", "9999-12-31T23:59:59Z".to_string()),
    ] {
        let mut row = mine.clone();
        row["name"] = name.into();
        row["command"] = name.into();
        row["is_default"] = false.into();
        row["is_global_default"] = false.into();
        row["usage"]["weekly"]["percent"] = 99.0.into();
        row["usage"]["updated_at"] = at.into();
        rows_there.push(row);
    }
    reply["profiles"] = rows_there.into();
    local.write("ssh-canned", &reply.to_string());
    local.add_machine("box", "canned", None, None);
    let stub = stub(&[]);
    let before = local.read(".mcc/state/default/usage.json");

    let v = v2(&local, &stub, &[]);
    assert_eq!(weekly(by_name(rows(&v), "default")), 20.0, "{v}");
    let box_v = machine_of(&v, "box");
    for name in ["work", "side"] {
        assert_eq!(weekly(by_name(rows(box_v), name)), 20.0, "{v}");
    }
    // Its own times weren't believed, so it has none.
    assert_eq!(box_v["updated_at"], serde_json::Value::Null, "{v}");
    assert_eq!(local.read(".mcc/state/default/usage.json"), before);
    let out = table(&local, &stub, &["usage"]);
    assert!(out.starts_with("local · updated 5m ago\n"), "{out}");
    assert!(out.contains("box (canned) · updated —"), "{out}");
    assert_eq!(local.read(".mcc/state/default/usage.json"), before);
}

#[test]
fn a_machine_that_fails_to_refresh_keeps_what_it_had() {
    // dan is on aa (first in the config, a minute-old 70%) and on box (a
    // two-hour-old 40%). --all asks aa to look dan up, and that run
    // can't connect: aa says why, keeps its cached rows, and dan's row on
    // box still shows aa's newer numbers.
    let local = machine(Some(("ann@example.com", "tok-ann")), &[]);
    let aa = machine(Some(("dan@example.com", "tok-dan-aa")), &[]);
    let box_ = machine(Some(("dan@example.com", "tok-dan-box")), &[]);
    seed_usage_cache(&aa, "default", None, Some((70.0, 86400)), 60);
    seed_usage_cache(&box_, "default", None, Some((40.0, 86400)), 2 * 3600);
    local.add_machine("aa", "aa-flaky", Some(&aa), None);
    local.add_machine("box", "box-host", Some(&box_), None);
    let stub = stub(&[]);

    let v = v2(&local, &stub, &["--all"]);
    assert_eq!(
        commands_to(&local, "aa-flaky"),
        [
            "'mcc' 'usage' '--json-v2' '--machine-protocol'",
            "'mcc' 'usage' '--all' '--json-v2' '--machine-protocol' '--only' 'default'"
        ]
    );
    assert_eq!(commands_to(&local, "box-host").len(), 1);
    let aa_v = machine_of(&v, "aa");
    assert_eq!(aa_v["error"], "unreachable", "{v}");
    assert_eq!(weekly(by_name(rows(aa_v), "default")), 70.0, "{v}");
    assert_eq!(aa_v["updated_at"].as_str().map(str::len), Some(20), "{v}");
    assert_eq!(
        weekly(by_name(rows(machine_of(&v, "box")), "default")),
        70.0,
        "{v}"
    );
    assert_eq!(machine_of(&v, "box")["error"], serde_json::Value::Null);
    // Only ann was looked up, here; dan's lookup never ran.
    assert_eq!(stub.tokens(), ["tok-ann"]);
}

/// Each machine's heading is a rule as wide as the tables, so machines
/// stand apart; a narrower terminal caps it.
#[test]
fn each_machines_heading_is_a_rule_as_wide_as_the_tables() {
    let local = machine(Some(("ann@example.com", "tok-1")), &[]);
    let lab = machine(Some(("bob@example.com", "tok-2")), &[]);
    local.add_machine("lab", "lab-host", Some(&lab), None);
    let stub = stub(&[]);
    let is_rule = |l: &&str| l.starts_with("── ");

    let out = run(&local, &stub, &["usage"]);
    let width = out
        .lines()
        .filter(|l| !is_rule(l))
        .map(|l| l.chars().count())
        .max()
        .unwrap();
    let rules: Vec<&str> = out.lines().filter(is_rule).collect();
    assert_eq!(rules.len(), 2, "{out}");
    for r in rules {
        assert!(r.ends_with("──"), "{out}");
        assert_eq!(r.chars().count(), width, "{out}");
    }

    let narrow = stdout_of(
        local
            .mcc()
            .env("MCC_USAGE_URL", &stub.url)
            .env("COLUMNS", "40")
            .args(["usage"]),
    );
    let rules: Vec<&str> = narrow.lines().filter(is_rule).collect();
    assert_eq!(rules.len(), 2, "{narrow}");
    for r in rules {
        assert_eq!(r.chars().count(), 40, "{narrow}");
    }
}
