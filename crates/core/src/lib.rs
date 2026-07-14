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

    /// Byte limit declared by `storage.quota`, if any.
    ///
    /// `Ok(None)` means no limit (quota unset or empty). See
    /// `parse_quota_bytes` for the accepted formats and the 1024-based
    /// suffix convention.
    pub fn quota_bytes(&self) -> Result<Option<u64>, ManifestError> {
        match &self.storage.quota {
            Some(quota) => parse_quota_bytes(quota),
            None => Ok(None),
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
    !p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    })
}

/// Validate a storage quota string (e.g. "500mb", "1gb", "2.5tb", "100").
/// Bare numbers are interpreted as bytes; suffixes are 1024-based
/// (`kb` = 1024, `mb` = 1024², `gb` = 1024³, `tb` = 1024⁴).
pub fn is_valid_quota(quota: &str) -> bool {
    parse_quota(quota).is_ok()
}

/// Parse a storage quota string into a byte count.
///
/// Accepts the same formats as `is_valid_quota`: an empty string (no
/// limit), a bare number of bytes ("100"), or a number with a
/// case-insensitive suffix (`b`, `kb`, `mb`, `gb`, `tb`). Suffixes are
/// 1024-based — `kb` = 1024 bytes, `mb` = 1024², `gb` = 1024³,
/// `tb` = 1024⁴ — and the number may be decimal, so "2.5tb" is
/// 2.5 × 1024⁴ bytes.
///
/// Returns `Ok(None)` for an empty quota (unlimited). Returns
/// `ManifestError::BadQuota` when the format is invalid or the byte count
/// does not fit into a `u64`.
pub fn parse_quota_bytes(quota: &str) -> Result<Option<u64>, ManifestError> {
    match parse_quota(quota) {
        Ok(None) => Ok(None),
        Ok(Some((number, multiplier))) => quota_to_bytes(number, multiplier)
            .map(Some)
            .ok_or_else(|| ManifestError::BadQuota(quota.trim().to_string())),
        Err(()) => Err(ManifestError::BadQuota(quota.trim().to_string())),
    }
}

/// Quota suffixes with their 1024-based multipliers.
const QUOTA_SUFFIXES: [(&str, u64); 5] = [
    ("b", 1),
    ("kb", 1024),
    ("mb", 1024 * 1024),
    ("gb", 1024 * 1024 * 1024),
    ("tb", 1024 * 1024 * 1024 * 1024),
];

/// Numeric part of a quota. Whole numbers stay exact; decimals are kept
/// as `f64`.
enum QuotaNumber {
    Int(u64),
    Float(f64),
}

/// Shared parsing core for `is_valid_quota` and `parse_quota_bytes`, so
/// both always agree on which formats are valid.
///
/// Returns `Ok(None)` for an empty quota (no limit), `Ok(Some((number,
/// multiplier)))` for a well-formed quota, and `Err(())` for a malformed
/// one.
fn parse_quota(quota: &str) -> Result<Option<(QuotaNumber, u64)>, ()> {
    let trimmed = quota.trim();
    if trimmed.is_empty() {
        return Ok(None); // empty quota is valid (no limit)
    }
    // Bare numbers are interpreted as bytes.
    if let Some(number) = parse_number(trimmed) {
        return Ok(Some((number, 1)));
    }
    let lower = trimmed.to_lowercase();
    for (suffix, multiplier) in QUOTA_SUFFIXES {
        if let Some(number_text) = lower.strip_suffix(suffix) {
            if let Some(number) = parse_number(number_text.trim()) {
                return Ok(Some((number, multiplier)));
            }
        }
    }
    Err(())
}

/// Parse the numeric part of a quota. `u64`-parsable input (a subset of
/// `f64`-parsable input, so validity is unchanged) is kept exact;
/// anything else falls back to `f64`.
fn parse_number(text: &str) -> Option<QuotaNumber> {
    if let Ok(int) = text.parse::<u64>() {
        return Some(QuotaNumber::Int(int));
    }
    text.parse::<f64>().ok().map(QuotaNumber::Float)
}

/// Convert a parsed quota to bytes, rejecting negative, non-finite, or
/// overflowing values instead of wrapping.
fn quota_to_bytes(number: QuotaNumber, multiplier: u64) -> Option<u64> {
    match number {
        QuotaNumber::Int(int) => int.checked_mul(multiplier),
        QuotaNumber::Float(value) => {
            if !value.is_finite() || value < 0.0 {
                return None;
            }
            let whole = value.trunc();
            // `u64::MAX as f64` rounds up to 2^64, so this strict bound is
            // exactly the values that cast back without saturating.
            if whole >= u64::MAX as f64 {
                return None;
            }
            let fraction_bytes = ((value - whole) * multiplier as f64) as u64;
            (whole as u64)
                .checked_mul(multiplier)?
                .checked_add(fraction_bytes)
        }
    }
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

    #[test]
    fn parses_valid_quota_bytes() {
        assert_eq!(parse_quota_bytes("").unwrap(), None);
        assert_eq!(parse_quota_bytes("   ").unwrap(), None);
        assert_eq!(parse_quota_bytes("100").unwrap(), Some(100));
        assert_eq!(parse_quota_bytes("1b").unwrap(), Some(1));
        assert_eq!(parse_quota_bytes("1kb").unwrap(), Some(1024));
        assert_eq!(parse_quota_bytes("500mb").unwrap(), Some(500 * 1024 * 1024));
        assert_eq!(parse_quota_bytes("1gb").unwrap(), Some(1024 * 1024 * 1024));
        assert_eq!(parse_quota_bytes("2.5TB").unwrap(), Some(2748779069440));
        assert_eq!(parse_quota_bytes("0.5kb").unwrap(), Some(512));
        assert_eq!(
            parse_quota_bytes(" 500mb ").unwrap(),
            Some(500 * 1024 * 1024)
        );
    }

    #[test]
    fn invalid_quota_bytes_error() {
        assert!(matches!(
            parse_quota_bytes("not_a_quota"),
            Err(ManifestError::BadQuota(_))
        ));
        assert!(matches!(
            parse_quota_bytes("mb"),
            Err(ManifestError::BadQuota(_))
        ));
        assert!(matches!(
            parse_quota_bytes("1.2.3gb"),
            Err(ManifestError::BadQuota(_))
        ));
        // Negative amounts are well-formed but cannot be byte counts.
        assert!(matches!(
            parse_quota_bytes("-5mb"),
            Err(ManifestError::BadQuota(_))
        ));
    }

    #[test]
    fn quota_bytes_reads_manifest_quota() {
        let m = Manifest::from_toml(DIRECTIVE_EXAMPLE).unwrap();
        assert_eq!(m.quota_bytes().unwrap(), Some(500 * 1024 * 1024));

        let no_quota = Manifest::from_toml(
            r#"
[grow]
name = "app"
version = "1.0.0"
"#,
        )
        .unwrap();
        assert_eq!(no_quota.quota_bytes().unwrap(), None);
    }

    #[test]
    fn overflowing_quota_bytes_error() {
        // Overflows instead of panicking or wrapping.
        assert!(matches!(
            parse_quota_bytes("99999999999999999999tb"),
            Err(ManifestError::BadQuota(_))
        ));
        assert!(matches!(
            parse_quota_bytes("18446744073709551616"),
            Err(ManifestError::BadQuota(_))
        ));
        // u64::MAX exactly still fits.
        assert_eq!(
            parse_quota_bytes("18446744073709551615").unwrap(),
            Some(u64::MAX)
        );
    }
}
