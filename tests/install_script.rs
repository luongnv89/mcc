//! install.sh: the prebuilt-binary download path and its checksum check.
//!
//! Each test serves a fake release from a temporary directory through
//! `MCC_RELEASE_URL=file://…` (curl reads file URLs), so nothing touches the
//! network. The "binary" is a shell script that prints a version.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The release target install.sh picks on this machine.
fn host_target() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => panic!("no prebuilt target for {other}"),
    };
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-musl",
        other => panic!("no prebuilt target for {other}"),
    };
    format!("{arch}-{os}")
}

fn sha256_hex(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).unwrap();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Fixture {
    root: TempDir,
}

impl Fixture {
    /// A release directory holding `mcc-<host target>.tar.gz` and its
    /// `.sha256`, plus a copy of install.sh outside any checkout.
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pkg = root.path().join("pkg");
        let rel = root.path().join("release");
        fs::create_dir_all(&pkg).unwrap();
        fs::create_dir_all(&rel).unwrap();
        fs::create_dir_all(root.path().join("home")).unwrap();

        let bin = pkg.join("mcc");
        fs::write(&bin, "#!/bin/sh\necho \"mcc 9.9.9-test\"\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

        let asset = format!("mcc-{}.tar.gz", host_target());
        let status = Command::new("tar")
            .arg("-czf")
            .arg(rel.join(&asset))
            .arg("-C")
            .arg(&pkg)
            .arg("mcc")
            .status()
            .unwrap();
        assert!(status.success(), "tar failed");
        let sum = sha256_hex(&rel.join(&asset));
        fs::write(
            rel.join(format!("{asset}.sha256")),
            format!("{sum}  {asset}\n"),
        )
        .unwrap();

        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
        fs::copy(&script, root.path().join("install.sh")).unwrap();
        Fixture { root }
    }

    fn release_dir(&self) -> PathBuf {
        self.root.path().join("release")
    }

    fn install_dir(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn checksum_file(&self) -> PathBuf {
        self.release_dir()
            .join(format!("mcc-{}.tar.gz.sha256", host_target()))
    }

    fn run(&self, extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("sh");
        cmd.arg(self.root.path().join("install.sh"))
            .current_dir(self.root.path())
            .env("HOME", self.root.path().join("home"))
            .env("NO_COLOR", "1")
            .env(
                "MCC_RELEASE_URL",
                format!("file://{}", self.release_dir().display()),
            )
            .env("MCC_INSTALL_DIR", self.install_dir())
            .env_remove("MCC_VERSION")
            .env_remove("MCC_FROM_SOURCE");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn installs_the_prebuilt_binary_after_checking_its_checksum() {
    let fx = Fixture::new();
    let out = fx.run(&[]);
    let log = text(&out);
    assert!(out.status.success(), "install.sh failed:\n{log}");
    assert!(log.contains("checksum: sha256 "), "{log}");
    assert!(log.contains("installed mcc 9.9.9-test"), "{log}");
    assert!(!log.contains("cargo"), "should not need cargo:\n{log}");

    let installed = fx.install_dir().join("mcc");
    let mode = fs::metadata(&installed).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    let version = Command::new(&installed).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "mcc 9.9.9-test"
    );
}

#[test]
fn refuses_a_download_whose_checksum_does_not_match() {
    let fx = Fixture::new();
    let asset = format!("mcc-{}.tar.gz", host_target());
    fs::write(fx.checksum_file(), format!("{}  {asset}\n", "0".repeat(64))).unwrap();

    let out = fx.run(&[]);
    let log = text(&out);
    assert!(!out.status.success(), "install.sh should fail:\n{log}");
    assert!(log.contains("checksum mismatch"), "{log}");
    assert!(
        !fx.install_dir().join("mcc").exists(),
        "nothing may be installed on a mismatch"
    );
}

#[test]
fn refuses_a_download_without_a_checksum_file() {
    let fx = Fixture::new();
    fs::remove_file(fx.checksum_file()).unwrap();

    let out = fx.run(&[]);
    let log = text(&out);
    assert!(!out.status.success(), "install.sh should fail:\n{log}");
    assert!(log.contains("not its checksum"), "{log}");
    assert!(!fx.install_dir().join("mcc").exists());
}

#[test]
fn rejects_an_invalid_version() {
    let fx = Fixture::new();
    let out = Command::new("sh")
        .arg(fx.root.path().join("install.sh"))
        .current_dir(fx.root.path())
        .env("HOME", fx.root.path().join("home"))
        .env("NO_COLOR", "1")
        .env("MCC_VERSION", "v1;touch pwned")
        .env_remove("MCC_RELEASE_URL")
        .env_remove("MCC_FROM_SOURCE")
        .output()
        .unwrap();
    let log = text(&out);
    assert!(!out.status.success(), "{log}");
    assert!(log.contains("invalid MCC_VERSION"), "{log}");
    assert!(!fx.root.path().join("pwned").exists());
}
