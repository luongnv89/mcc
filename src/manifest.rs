//! The sync manifest, `<profile>/.mcc-sync.json`: for every path and MCP
//! server mcc last wrote into a profile (by `add --clone` or `sync`), the hash
//! of what it wrote. With it, `sync` tells a change in the default from an
//! edit made in the profile (see `copy::plan_with`).
//!
//! The manifest belongs to its profile: it is never part of the sync set, so
//! it is never copied from the default or between profiles.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::fsutil;

/// File name of the manifest, inside the profile dir.
pub const FILE: &str = ".mcc-sync.json";

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    /// Profile-relative path (`/`-separated) → hash of the file or symlink
    /// mcc wrote there.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// MCP server name → hash of the server entry mcc wrote to `.claude.json`.
    #[serde(default)]
    pub mcp: BTreeMap<String, String>,
    /// The sync items (their paths, and [`MCP_ITEM`]) whose state is
    /// recorded: a sync with `--only` records only some. Paths of other
    /// items are compared two-way.
    #[serde(default)]
    pub items: BTreeSet<String>,
}

/// The [`Manifest::items`] entry for MCP servers.
pub const MCP_ITEM: &str = ":mcp";

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: VERSION,
            files: BTreeMap::new(),
            mcp: BTreeMap::new(),
            items: BTreeSet::new(),
        }
    }
}

/// What reading a profile's manifest found.
pub enum Loaded {
    Found(Manifest),
    /// No manifest yet: the profile predates it, or was created `--fresh`.
    Missing,
    /// Unreadable, invalid or from a newer mcc; the reason.
    Invalid(String),
}

impl Loaded {
    pub fn manifest(&self) -> Option<&Manifest> {
        match self {
            Loaded::Found(m) => Some(m),
            _ => None,
        }
    }
}

pub fn load(profile_dir: &Path) -> Loaded {
    let path = profile_dir.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) => return Loaded::Invalid(e.to_string()),
    };
    match serde_json::from_slice::<Manifest>(&bytes) {
        Ok(m) if m.version == VERSION => Loaded::Found(m),
        Ok(m) => Loaded::Invalid(format!("unsupported version {}", m.version)),
        Err(e) => Loaded::Invalid(e.to_string()),
    }
}

impl Manifest {
    pub fn save(&self, profile_dir: &Path) -> Result<()> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        fsutil::write_atomic(&profile_dir.join(FILE), text.as_bytes())
    }
}

/// The manifest key for a profile-relative path.
pub fn key(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn digest(kind: &[u8], bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(kind);
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Hash of a regular file's content.
pub fn hash_file(bytes: &[u8]) -> String {
    digest(b"file\0", bytes)
}

/// Hash of a symlink, by its target.
pub fn hash_link(target: &Path) -> String {
    digest(b"link\0", target.as_os_str().as_bytes())
}

/// Hash of an MCP server entry. Object keys are sorted first, so a config
/// that was only re-serialized in another key order hashes the same.
pub fn hash_json(v: &Value) -> String {
    digest(b"json\0", canonical(v).to_string().as_bytes())
}

/// `v` with every object's keys sorted; arrays keep their order.
fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&m[k])))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
        _ => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_bad_files() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(load(d.path()), Loaded::Missing));
        let mut m = Manifest::default();
        m.files.insert("skills/a/SKILL.md".into(), hash_file(b"a"));
        m.mcp
            .insert("fs".into(), hash_json(&serde_json::json!({"c": 1})));
        m.save(d.path()).unwrap();
        match load(d.path()) {
            Loaded::Found(got) => assert_eq!(got, m),
            _ => panic!("manifest not read back"),
        }
        std::fs::write(d.path().join(FILE), "{not json").unwrap();
        assert!(matches!(load(d.path()), Loaded::Invalid(_)));
        std::fs::write(d.path().join(FILE), r#"{"version":99}"#).unwrap();
        assert!(matches!(load(d.path()), Loaded::Invalid(r) if r.contains("99")));
    }

    #[test]
    fn hashes_tell_files_from_links() {
        assert_ne!(hash_file(b"x"), hash_link(Path::new("x")));
        assert_eq!(hash_file(b"x"), hash_file(b"x"));
        assert_eq!(key(Path::new("a/b/c.md")), "a/b/c.md");
    }

    #[test]
    fn json_hash_ignores_key_order_only() {
        let a: Value =
            serde_json::from_str(r#"{"command":"x","args":["1","2"],"env":{"A":"1","B":"2"}}"#)
                .unwrap();
        let b: Value =
            serde_json::from_str(r#"{"env":{"B":"2","A":"1"},"args":["1","2"],"command":"x"}"#)
                .unwrap();
        let c: Value =
            serde_json::from_str(r#"{"command":"x","args":["2","1"],"env":{"A":"1","B":"2"}}"#)
                .unwrap();
        assert_eq!(hash_json(&a), hash_json(&b));
        assert_ne!(hash_json(&a), hash_json(&c));
    }
}
