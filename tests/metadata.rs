//! Open-source release metadata: license, MSRV, and community files.

use std::path::Path;

fn repo_file(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn package_declares_apache_license() {
    assert_eq!(env!("CARGO_PKG_LICENSE"), "Apache-2.0");
}

#[test]
fn package_declares_rust_version() {
    assert_eq!(env!("CARGO_PKG_RUST_VERSION"), "1.88");
}

#[test]
fn license_file_is_apache_2_0() {
    let license = repo_file("LICENSE");
    for marker in [
        "Apache License",
        "Version 2.0, January 2004",
        "END OF TERMS AND CONDITIONS",
    ] {
        assert!(license.contains(marker), "LICENSE is missing {marker:?}");
    }
}

#[test]
fn community_files_exist() {
    for rel in [
        "CONTRIBUTING.md",
        "CODE_OF_CONDUCT.md",
        "SECURITY.md",
        ".github/PULL_REQUEST_TEMPLATE.md",
        ".github/ISSUE_TEMPLATE/bug_report.md",
        ".github/ISSUE_TEMPLATE/feature_request.md",
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        assert!(path.is_file(), "{rel} is missing");
    }
}

#[test]
fn readme_links_license_and_contributing() {
    let readme = repo_file("README.md");
    for needle in ["## License", "(LICENSE)", "CONTRIBUTING.md"] {
        assert!(readme.contains(needle), "README.md is missing {needle:?}");
    }
}

const FORMULA: &str = "packaging/homebrew/mcc.rb";

#[test]
fn homebrew_formula_builds_the_tagged_release_from_source() {
    let formula = repo_file(FORMULA);
    let url = format!(
        "url \"{}/archive/refs/tags/v{}.tar.gz\"",
        env!("CARGO_PKG_REPOSITORY"),
        env!("CARGO_PKG_VERSION")
    );
    for needle in [
        "class Mcc < Formula".to_string(),
        format!("homepage \"{}\"", env!("CARGO_PKG_REPOSITORY")),
        url,
        format!("license \"{}\"", env!("CARGO_PKG_LICENSE")),
        format!(
            "head \"{}.git\", branch: \"main\"",
            env!("CARGO_PKG_REPOSITORY")
        ),
        "depends_on \"rust\" => :build".to_string(),
        "system \"cargo\", \"install\", *std_cargo_args".to_string(),
    ] {
        assert!(formula.contains(&needle), "{FORMULA} is missing {needle:?}");
    }
    let sha = formula
        .lines()
        .find_map(|l| l.trim().strip_prefix("sha256 \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{FORMULA} has no sha256 line"));
    assert!(
        sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()),
        "sha256 is not 64 hex chars: {sha:?}"
    );
}

#[test]
fn homebrew_formula_installs_completions_and_tests_the_binary() {
    let formula = repo_file(FORMULA);
    for needle in [
        "generate_completions_from_executable(bin/\"mcc\", \"completions\")",
        "test do",
        "shell_output(\"#{bin}/mcc --version\")",
        "assert_match version.to_s",
    ] {
        assert!(formula.contains(needle), "{FORMULA} is missing {needle:?}");
    }
}

#[test]
fn readme_documents_homebrew_install_upgrade_and_uninstall() {
    let readme = repo_file("README.md");
    for needle in [
        "brew install luongnv89/tap/mcc",
        "brew upgrade mcc",
        "brew uninstall mcc",
    ] {
        assert!(readme.contains(needle), "README.md is missing {needle:?}");
    }
}

const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";

#[test]
fn release_workflow_builds_every_target_on_version_tags() {
    let wf = repo_file(RELEASE_WORKFLOW);
    for needle in [
        "tags:\n      - \"v*\"",
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "cargo build --release --locked --target \"$TARGET\"",
        "SHA256SUMS",
        "mcc-$TARGET.tar.gz.sha256",
    ] {
        assert!(
            wf.contains(needle),
            "{RELEASE_WORKFLOW} is missing {needle:?}"
        );
    }
    assert!(
        !wf.contains("pull_request"),
        "{RELEASE_WORKFLOW} must not run for pull requests"
    );
}

#[test]
fn release_workflow_pins_actions_and_limits_permissions() {
    let wf = repo_file(RELEASE_WORKFLOW);
    for line in wf.lines().map(str::trim) {
        let Some(action) = line
            .strip_prefix("- uses: ")
            .or_else(|| line.strip_prefix("uses: "))
        else {
            continue;
        };
        let (_, rev) = action
            .split_once('@')
            .unwrap_or_else(|| panic!("unpinned action: {line}"));
        let sha = rev.split_whitespace().next().unwrap_or("");
        assert!(
            sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
            "action not pinned to a commit SHA: {line}"
        );
        assert!(
            rev.contains("# v"),
            "pinned action lacks a version comment: {line}"
        );
    }
    // Read-only by default; exactly one job (the uploader) may write.
    assert!(
        wf.contains("permissions:\n  contents: read"),
        "{RELEASE_WORKFLOW}"
    );
    assert_eq!(
        wf.matches("contents: write").count(),
        1,
        "{RELEASE_WORKFLOW}"
    );
    // Untrusted event text never reaches a script.
    assert!(!wf.contains("github.event."), "{RELEASE_WORKFLOW}");
}

/// The publish step's branch for a draft release: replacing its assets is safe.
const DRAFT_GUARD: &str = "if [ \"$is_draft\" = true ]; then";
const PUBLISH_DRAFT: &str = "--draft=false";

/// Byte offset of every occurrence of `needle` in `haystack`.
fn positions(haystack: &str, needle: &str) -> Vec<usize> {
    haystack.match_indices(needle).map(|(i, _)| i).collect()
}

/// Byte offset of `needle`, which must occur exactly once in the workflow.
fn position_of_only(wf: &str, needle: &str) -> usize {
    match positions(wf, needle)[..] {
        [at] => at,
        ref all => panic!(
            "{RELEASE_WORKFLOW} has {needle:?} {} times, expected once",
            all.len()
        ),
    }
}

#[test]
fn release_workflow_publishes_a_draft_only_after_its_assets_are_attached() {
    let wf = repo_file(RELEASE_WORKFLOW);
    // Every release with the tag is listed, drafts included; the tag reaches
    // jq through the environment, not the program text.
    for needle in ["repos/$GH_REPO/releases", "select(.tag_name == $ENV.TAG)"] {
        assert!(
            wf.contains(needle),
            "{RELEASE_WORKFLOW} is missing {needle:?}"
        );
    }
    assert!(
        !wf.contains("gh release view \"$TAG\" >/dev/null 2>&1"),
        "{RELEASE_WORKFLOW} still hides release lookup errors"
    );
    // A new release starts as a draft, and only the draft branch publishes it,
    // after its uploads.
    assert_eq!(
        wf.matches("gh release create").count(),
        1,
        "{RELEASE_WORKFLOW} must create the release in one place"
    );
    let create = position_of_only(&wf, "gh release create \"$TAG\" --draft --verify-tag");
    let guard = position_of_only(&wf, DRAFT_GUARD);
    let publish = position_of_only(&wf, PUBLISH_DRAFT);
    assert!(
        create < guard && guard < publish,
        "{RELEASE_WORKFLOW}: create the draft, then attach, then publish"
    );
    let uploads = &wf[guard..publish];
    assert!(
        uploads.contains("gh release upload \"$TAG\" --clobber SHA256SUMS"),
        "{RELEASE_WORKFLOW}: the draft branch must attach SHA256SUMS before publishing"
    );
    assert_eq!(
        wf[publish..].lines().nth(1).map(str::trim),
        Some("exit 0"),
        "{RELEASE_WORKFLOW}: the draft branch must stop once it has published"
    );
}

#[test]
fn release_workflow_never_replaces_the_assets_of_a_published_release() {
    let wf = repo_file(RELEASE_WORKFLOW);
    let guard = position_of_only(&wf, DRAFT_GUARD);
    let publish = position_of_only(&wf, PUBLISH_DRAFT);
    // Assets are replaced only while the release is still a draft.
    let clobbers = positions(&wf, "--clobber");
    assert!(
        !clobbers.is_empty(),
        "{RELEASE_WORKFLOW}: a draft's assets should be replaceable"
    );
    for at in clobbers {
        assert!(
            guard < at && at < publish,
            "{RELEASE_WORKFLOW} replaces assets outside the draft branch (byte {at})"
        );
    }
    // A published release: compare SHA256SUMS first, upload only what is missing.
    let published = &wf[publish..];
    for needle in [
        "gh release download \"$TAG\" --pattern SHA256SUMS",
        "cmp -s",
        "::error::",
        "gh release upload",
    ] {
        assert!(
            published.contains(needle),
            "{RELEASE_WORKFLOW}: the published-release branch is missing {needle:?}"
        );
    }
    let compare = published.find("cmp -s").unwrap();
    let upload = published.find("gh release upload").unwrap();
    assert!(
        compare < upload,
        "{RELEASE_WORKFLOW}: compare SHA256SUMS before uploading anything"
    );
}

/// `x.y` or `x.y.z` as a comparable triple (a missing patch is 0).
fn version_triple(v: &str) -> (u32, u32, u32) {
    let parts: Vec<u32> = v
        .split('.')
        .map(|p| p.parse().unwrap_or_else(|_| panic!("not a version: {v:?}")))
        .collect();
    match parts[..] {
        [major, minor] => (major, minor, 0),
        [major, minor, patch] => (major, minor, patch),
        _ => panic!("not a version: {v:?}"),
    }
}

#[test]
fn release_workflow_builds_with_a_pinned_rust_toolchain() {
    let wf = repo_file(RELEASE_WORKFLOW);
    let pins: Vec<&str> = wf
        .lines()
        .filter_map(|l| l.trim().strip_prefix("RUST_TOOLCHAIN: \""))
        .collect();
    let [pin] = pins[..] else {
        panic!("{RELEASE_WORKFLOW} needs one RUST_TOOLCHAIN: \"x.y.z\" line, found {pins:?}");
    };
    let pinned = pin
        .strip_suffix('"')
        .unwrap_or_else(|| panic!("RUST_TOOLCHAIN is not quoted: {pin:?}"));
    assert_eq!(
        pinned.split('.').count(),
        3,
        "RUST_TOOLCHAIN must name an exact x.y.z release: {pinned:?}"
    );
    let msrv = env!("CARGO_PKG_RUST_VERSION");
    assert!(
        version_triple(pinned) >= version_triple(msrv),
        "RUST_TOOLCHAIN {pinned} is older than rust-version {msrv}"
    );
    for needle in [
        "rustup toolchain install \"$RUST_TOOLCHAIN\"",
        "rustup default \"$RUST_TOOLCHAIN\"",
        "\"rustc $RUST_TOOLCHAIN \"*)",
    ] {
        assert!(
            wf.contains(needle),
            "{RELEASE_WORKFLOW} is missing {needle:?}"
        );
    }
    for needle in ["toolchain install stable", "default stable"] {
        assert!(
            !wf.contains(needle),
            "{RELEASE_WORKFLOW} must not build with the moving stable channel: {needle:?}"
        );
    }
}

#[test]
fn install_script_verifies_the_checksum_of_prebuilt_binaries() {
    let script = repo_file("install.sh");
    for needle in [
        "releases/latest/download",
        "mcc-$target.tar.gz",
        ".sha256",
        "checksum mismatch",
        "MCC_FROM_SOURCE",
    ] {
        assert!(script.contains(needle), "install.sh is missing {needle:?}");
    }
}
