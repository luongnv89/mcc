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

    fn tarball(&self) -> PathBuf {
        self.release_dir()
            .join(format!("mcc-{}.tar.gz", host_target()))
    }

    /// Where the stub `cargo` installs, as `CARGO_INSTALL_ROOT`.
    fn cargo_root(&self) -> PathBuf {
        self.root.path().join("cargo-root")
    }

    /// The file the stub `cargo` appends each invocation's arguments to.
    fn cargo_log(&self) -> PathBuf {
        self.root.path().join("cargo-args.log")
    }

    /// Writes a stub `cargo` into a fresh directory and returns that
    /// directory. `cargo install …` records its arguments and drops an `mcc`
    /// into `$CARGO_INSTALL_ROOT/bin`; `cargo --version` prints a version.
    fn stub_cargo(&self) -> PathBuf {
        let dir = self.root.path().join("stub-bin");
        fs::create_dir_all(&dir).unwrap();
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{log}'\n\
             if [ \"$1\" = --version ]; then echo 'cargo 1.99.0-stub'; exit 0; fi\n\
             mkdir -p \"$CARGO_INSTALL_ROOT/bin\"\n\
             printf '#!/bin/sh\\necho \"mcc 0.3.0-stub\"\\n' > \"$CARGO_INSTALL_ROOT/bin/mcc\"\n\
             chmod 755 \"$CARGO_INSTALL_ROOT/bin/mcc\"\n",
            log = self.cargo_log().display()
        );
        let cargo = dir.join("cargo");
        fs::write(&cargo, script).unwrap();
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    /// The `install cargo …` lines the stub recorded.
    fn cargo_installs(&self) -> Vec<String> {
        fs::read_to_string(self.cargo_log())
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("install "))
            .map(str::to_owned)
            .collect()
    }

    /// install.sh with a hermetic environment. `path` replaces `PATH`; by
    /// default the install directory goes first so no other `mcc` shadows it.
    fn command(&self, path: Option<String>) -> Command {
        let path = path.unwrap_or_else(|| {
            join_path(
                &[self.install_dir()],
                &std::env::var("PATH").unwrap_or_default(),
            )
        });
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(self.root.path().join("install.sh"))
            .current_dir(self.root.path())
            .env("PATH", path)
            .env("HOME", self.root.path().join("home"))
            .env("NO_COLOR", "1")
            .env(
                "MCC_RELEASE_URL",
                format!("file://{}", self.release_dir().display()),
            )
            .env("MCC_INSTALL_DIR", self.install_dir())
            .env("CARGO_INSTALL_ROOT", self.cargo_root())
            .env_remove("MCC_VERSION")
            .env_remove("MCC_FROM_SOURCE");
        cmd
    }

    fn run(&self, extra_env: &[(&str, &str)]) -> Output {
        self.run_with_path(None, extra_env)
    }

    fn run_with_path(&self, path: Option<String>, extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = self.command(path);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    /// `PATH` with the stub `cargo` first, then the directory it installs to.
    fn path_with_stub_cargo(&self) -> String {
        join_path(
            &[self.stub_cargo(), self.cargo_root().join("bin")],
            &std::env::var("PATH").unwrap_or_default(),
        )
    }
}

fn join_path(front: &[PathBuf], rest: &str) -> String {
    let mut parts: Vec<String> = front.iter().map(|p| p.display().to_string()).collect();
    if !rest.is_empty() {
        parts.push(rest.to_owned());
    }
    parts.join(":")
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
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
    let out = Command::new("/bin/sh")
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

#[test]
fn falls_back_to_cargo_at_the_requested_tag_when_no_binary_exists() {
    let fx = Fixture::new();
    fs::remove_file(fx.tarball()).unwrap();

    let out = fx.run_with_path(Some(fx.path_with_stub_cargo()), &[("MCC_VERSION", "0.3.0")]);
    let log = text(&out);
    assert!(out.status.success(), "install.sh failed:\n{log}");
    assert!(log.contains("building from source instead"), "{log}");
    assert!(log.contains("installed mcc 0.3.0-stub"), "{log}");

    let installs = fx.cargo_installs();
    assert_eq!(installs.len(), 1, "{installs:?}");
    let args: Vec<&str> = installs[0].split_whitespace().collect();
    for want in ["install", "--git", "--tag", "v0.3.0"] {
        assert!(args.contains(&want), "missing {want}: {args:?}");
    }
    let tag = args.iter().position(|a| *a == "--tag").unwrap();
    assert_eq!(args[tag + 1], "v0.3.0", "{args:?}");
}

#[test]
fn from_source_uses_cargo_even_when_a_binary_exists() {
    let fx = Fixture::new();
    assert!(fx.tarball().exists());

    let out = fx.run_with_path(Some(fx.path_with_stub_cargo()), &[("MCC_FROM_SOURCE", "1")]);
    let log = text(&out);
    assert!(out.status.success(), "install.sh failed:\n{log}");
    assert!(!log.contains("checksum:"), "should not download:\n{log}");
    assert!(log.contains("installed mcc 0.3.0-stub"), "{log}");
    assert!(!fx.install_dir().join("mcc").exists());

    let installs = fx.cargo_installs();
    assert_eq!(installs.len(), 1, "{installs:?}");
    let args: Vec<&str> = installs[0].split_whitespace().collect();
    assert!(args.contains(&"--git"), "{args:?}");
    assert!(!args.contains(&"--tag"), "no version was set: {args:?}");
}

#[test]
fn fails_clearly_without_cargo_or_a_binary() {
    let fx = Fixture::new();
    fs::remove_file(fx.tarball()).unwrap();

    // Drop every PATH directory holding a `cargo` (rustup, mise shims, …).
    let orig = std::env::var("PATH").unwrap_or_default();
    let dirs: Vec<&str> = orig
        .split(':')
        .filter(|d| !d.is_empty() && !is_executable(&Path::new(d).join("cargo")))
        .collect();
    let removed: Vec<&str> = orig
        .split(':')
        .filter(|d| !d.is_empty() && is_executable(&Path::new(d).join("cargo")))
        .collect();
    // Some systems keep cargo next to the basic tools (e.g. /usr/bin); link
    // the tools the script needs from those directories so it still runs.
    let tools = fx.root.path().join("tools");
    fs::create_dir_all(&tools).unwrap();
    for name in [
        "uname", "dirname", "grep", "curl", "tar", "mktemp", "rm", "cut", "cat", "mkdir",
    ] {
        if let Some(src) = removed
            .iter()
            .map(|d| Path::new(d).join(name))
            .find(|p| is_executable(p))
        {
            std::os::unix::fs::symlink(src, tools.join(name)).unwrap();
        }
    }
    let path = join_path(&[fx.install_dir(), tools], &dirs.join(":"));

    let out = fx.run_with_path(Some(path), &[]);
    let log = text(&out);
    assert!(!out.status.success(), "install.sh should fail:\n{log}");
    assert!(
        log.contains("cargo (the Rust toolchain) is required"),
        "{log}"
    );
    assert!(!fx.install_dir().join("mcc").exists());
    assert!(!fx.cargo_root().join("bin/mcc").exists());
}
