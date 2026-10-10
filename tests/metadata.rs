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
