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
