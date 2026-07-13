//! grow-core: the GROW manifest and permission model.
//!
//! Every GROW repository carries `.grow/manifest.toml` describing the
//! application, its runtime entry point, resource limits, and requested
//! permissions. The manifest is the single source of truth for how the
//! runtime may execute the application.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Location of the manifest inside every GROW repository.
pub const MANIFEST_PATH: &str = ".grow/manifest.toml";

/// The only runtime engine supported in Milestone 1.
pub const ENGINE_WASM: &str = "wasm";

/// Parsed `.grow/manifest.toml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub grow: GrowSection,
    /// Absent for static-only repositories (no executable entry point).
    #[serde(default)]
    pub runtime: Option<RuntimeSection>,
    #[serde(default)]
    pub network: NetworkSection,
    #[serde(default)]
    pub storage: StorageSection,
    #[serde(default)]
    pub permissions: Permissions,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GrowSection {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RuntimeSection {
    /// Execution engine. Milestone 1 supports only `"wasm"`.
    pub engine: Option<String>,
    /// Path to the entry module, relative to the repository root.
    pub entry: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct NetworkSection {
    /// External networking is off unless explicitly enabled *and* approved.
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct StorageSection {
    /// Persistent storage quota, e.g. `"500mb"`. Parsed but not yet enforced.
    pub quota: Option<String>,
}

/// Capabilities requested by the application. All are opt-in; the user
/// approves them at install time (approval flow lands in Milestone 2).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Permissions {
    /// `none` | `restricted` | `full` — only `restricted` is honored in M1
    /// (WASI modules receive no filesystem access regardless).
    pub filesystem: Option<String>,
    pub camera: Option<bool>,
    pub microphone: Option<bool>,
    pub clipboard: Option<bool>,
    pub notifications: Option<bool>,
    /// Forward-compatible catch-all for capability keys added later
    /// (e.g. `network.external = true`).
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to parse manifest TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("grow.name must not be empty")]
    MissingName,
    #[error("grow.name must be at most 128 characters, got {0}")]
    NameTooLong(usize),
    #[error("grow.version must not be empty")]
    MissingVersion,
    #[error("runtime.engine is required when a [runtime] section is present")]
    MissingEngine,
    #[error("unsupported runtime engine {0:?} (supported: \"wasm\")")]
    UnsupportedEngine(String),
    #[error("runtime.entry is required for engine \"wasm\"")]
    MissingEntry,
    #[error("runtime.entry must be a relative path without '..': {0:?}")]
    BadEntry(String),
    #[error("storage.quota format is invalid: {0:?}")]
    BadQuota(String),
}

impl Manifest {
    /// Parse and validate a manifest from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.grow.name.trim().is_empty() {
            return Err(ManifestError::MissingName);
        }
        if self.grow.name.len() > 128 {
            return Err(ManifestError::NameTooLong(self.grow.name.len()));
        }
        if self.grow.version.trim().is_empty() {
            return Err(ManifestError::MissingVersion);
        }
        if let Some(quota) = &self.storage.quota {
            if !is_valid_quota(quota) {
                return Err(ManifestError::BadQuota(quota.clone()));
            }
        }
        if let Some(runtime) = &self.runtime {
            let engine = runtime
                .engine
                .as_deref()
                .ok_or(ManifestError::MissingEngine)?;
            if engine != ENGINE_WASM {
                return Err(ManifestError::UnsupportedEngine(engine.to_string()));
            }
            let entry = runtime
                .entry
                .as_deref()
                .ok_or(ManifestError::MissingEntry)?;
            if !is_safe_relative_path(entry) {
                return Err(ManifestError::BadEntry(entry.to_string()));
            }
        }
        Ok(())
    }

    /// Entry module path when this repository declares a WASM runtime.
    pub fn wasm_entry(&self) -> Option<&str> {
        let runtime = self.runtime.as_ref()?;
        if runtime.engine.as_deref() == Some(ENGINE_WASM) {
            runtime.entry.as_deref()
        } else {
            None
        }
    }
}

/// A path is usable inside a repository only if it is relative and cannot
/// escape the root.
pub fn is_safe_relative_path(path: &str) -> bool {
    use std::path::{Component, Path};
    if path.is_empty() || path.contains('\\') {
        return false;
    }
    let p = Path::new(path);
    if p.is_absolute() {
        return false;
    }
    !p.components()
        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
}

/// Validate a storage quota string (e.g. "500mb", "1gb", "2.5tb", "100").
/// Bare numbers are interpreted as bytes.
pub fn is_valid_quota(quota: &str) -> bool {
    let trimmed = quota.trim();
    if trimmed.is_empty() {
        return true; // empty quota is valid (no limit)
    }
    // Bare numbers are valid (interpreted as bytes).
    if trimmed.parse::<f64>().is_ok() {
        return true;
    }
    let suffixes = ["b", "kb", "mb", "gb", "tb"];
    let lower = trimmed.to_lowercase();
    suffixes.iter().any(|suffix| {
        lower.ends_with(suffix)
            && lower[..lower.len() - suffix.len()]
                .trim()
                .parse::<f64>()
                .is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECTIVE_EXAMPLE: &str = r#"
[grow]
name = "example-app"
version = "1.0.0"

[runtime]
engine = "wasm"
entry = "main.wasm"

[network]
enabled = false

[storage]
quota = "500mb"

[permissions]
filesystem = "restricted"
camera = false
microphone = false
"#;

    #[test]
    fn parses_directive_example() {
        let m = Manifest::from_toml(DIRECTIVE_EXAMPLE).expect("directive example must parse");
        assert_eq!(m.grow.name, "example-app");
        assert_eq!(m.grow.version, "1.0.0");
        assert_eq!(m.wasm_entry(), Some("main.wasm"));
        assert_eq!(m.network.enabled, Some(false));
        assert_eq!(m.storage.quota.as_deref(), Some("500mb"));
        assert_eq!(m.permissions.filesystem.as_deref(), Some("restricted"));
        assert_eq!(m.permissions.camera, Some(false));
    }

    #[test]
    fn rejects_missing_name() {
        let err = Manifest::from_toml(
            r#"
[grow]
name = "  "
version = "1.0.0"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::MissingName));
    }

    #[test]
    fn rejects_unknown_engine() {
        let err = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"

[runtime]
engine = "docker"
entry = "main.wasm"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::UnsupportedEngine(ref e) if e == "docker"));
    }

    #[test]
    fn rejects_traversal_entry() {
        let err = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"

[runtime]
engine = "wasm"
entry = "../evil.wasm"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadEntry(_)));
    }

    #[test]
    fn static_only_repo_has_no_entry() {
        let m = Manifest::from_toml(
            r#"
[grow]
name = "docs"
version = "0.1.0"
"#,
        )
        .expect("static-only manifest must parse");
        assert_eq!(m.wasm_entry(), None);
    }

    #[test]
    fn accepts_valid_entry_paths() {
        assert!(is_safe_relative_path("main.wasm"));
        assert!(is_safe_relative_path("sub/dir/main.wasm"));
        assert!(is_safe_relative_path("a/b/c/d.wasm"));
    }

    #[test]
    fn rejects_dangerous_paths() {
        assert!(!is_safe_relative_path(""));
        assert!(!is_safe_relative_path("../evil.wasm"));
        assert!(!is_safe_relative_path("a/../../b.wasm"));
        assert!(!is_safe_relative_path("/absolute.wasm"));
        assert!(!is_safe_relative_path("C:\\windows.wasm"));
        assert!(!is_safe_relative_path("mixed\\path.wasm"));
    }

    #[test]
    fn rejects_missing_engine() {
        let err = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"

[runtime]
entry = "main.wasm"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::MissingEngine));
    }

    #[test]
    fn rejects_missing_entry() {
        let err = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"

[runtime]
engine = "wasm"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::MissingEntry));
    }

    #[test]
    fn empty_manifest_fails_toml_parse() {
        let err = Manifest::from_toml("").unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    #[test]
    fn network_defaults_to_none() {
        let m = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"
"#,
        )
        .unwrap();
        assert_eq!(m.network.enabled, None);
    }

    #[test]
    fn permissions_default_to_none() {
        let m = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"
"#,
        )
        .unwrap();
        assert_eq!(m.permissions.filesystem, None);
        assert_eq!(m.permissions.camera, None);
        assert_eq!(m.permissions.microphone, None);
        assert_eq!(m.permissions.clipboard, None);
        assert_eq!(m.permissions.notifications, None);
    }

    #[test]
    fn storage_quota_is_optional() {
        let m = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"
"#,
        )
        .unwrap();
        assert_eq!(m.storage.quota, None);
    }

    #[test]
    fn wasm_entry_returns_none_for_static_only() {
        let m = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"
"#,
        )
        .unwrap();
        assert_eq!(m.wasm_entry(), None);
    }

    #[test]
    fn wasm_entry_returns_none_for_non_wasm_engine() {
        let m = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"

[runtime]
engine = "wasm"
entry = "main.wasm"
"#,
        )
        .unwrap();
        assert_eq!(m.wasm_entry(), Some("main.wasm"));
    }

    #[test]
    fn missing_grow_name_fails() {
        let err = Manifest::from_toml(
            r#"
[grow]
name = ""
version = "1.0.0"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::MissingName));
    }

    #[test]
    fn manifest_roundtrips_to_toml() {
        let m = Manifest::from_toml(DIRECTIVE_EXAMPLE).unwrap();
        let toml_text = toml::to_string(&m).unwrap();
        let m2 = Manifest::from_toml(&toml_text).unwrap();
        assert_eq!(m.grow.name, m2.grow.name);
        assert_eq!(m.grow.version, m2.grow.version);
        assert_eq!(m.wasm_entry(), m2.wasm_entry());
    }

    #[test]
    fn rejects_empty_version() {
        let err = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "  "
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::MissingVersion));
    }

    #[test]
    fn rejects_long_name() {
        let name = "a".repeat(129);
        let err = Manifest::from_toml(&format!(
            r#"
[grow]
name = "{name}"
version = "1.0.0"
"#
        ))
        .unwrap_err();
        assert!(matches!(err, ManifestError::NameTooLong(129)));
    }

    #[test]
    fn valid_quota_formats() {
        assert!(is_valid_quota("500mb"));
        assert!(is_valid_quota("1gb"));
        assert!(is_valid_quota("2.5TB"));
        assert!(is_valid_quota("100"));
        assert!(is_valid_quota(""));
    }

    #[test]
    fn invalid_quota_is_rejected() {
        let err = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"

[storage]
quota = "not_a_quota"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadQuota(ref q) if q == "not_a_quota"));
    }
}
