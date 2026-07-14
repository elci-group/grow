//! grow-git: repository loading for GROW.
//!
//! Uses `gix` (pure-Rust git) — git is never shelled out to at runtime.
//! Milestone 1 scope: open a local repository, resolve and expose the HEAD
//! commit (the "verify commit" lifecycle step), and read files from the
//! verified commit tree with path-traversal protection. Repositories can
//! also be cloned, but only from local paths and `file://` URLs — remote
//! fetch, shallow clones, and object streaming arrive in Milestone 3.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use grow_core::{MANIFEST_PATH, Manifest, ManifestError};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("cannot open git repository at {path}: {reason}")]
    Open { path: PathBuf, reason: String },
    #[error("bare repositories are not supported (no working tree)")]
    Bare,
    #[error("cannot resolve HEAD commit: {0}")]
    Head(String),
    #[error("remote URL scheme is not supported (use a local path, a file:// URL, or an https:// URL from github.com or gitlab.com): {0}")]
    UnsupportedScheme(String),
    #[error("remote host is not supported (use github.com or gitlab.com): {0}")]
    UnsupportedHost(String),
    #[error("credential-bearing remote URLs are not supported: {0}")]
    CredentialsInUrl(String),
    #[error("cannot clone {url}: {reason}")]
    Clone { url: String, reason: String },
    #[error("missing {MANIFEST_PATH} at repository root {0}")]
    MissingManifest(PathBuf),
    #[error("file {0:?} does not exist in the verified commit")]
    MissingFile(String),
    #[error("path {0:?} is not a regular file in the verified commit")]
    NotRegularFile(String),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("io error reading {path}: {reason}")]
    Io { path: PathBuf, reason: String },
}

/// A GROW application repository opened from a local path.
#[derive(Debug)]
pub struct Repository {
    root: PathBuf,
    head: String,
    tree: gix::hash::ObjectId,
    manifest: Manifest,
}

impl Repository {
    /// Open the git repository at `path`, verify HEAD resolves, and load
    /// `.grow/manifest.toml` from the verified HEAD tree.
    pub fn open(path: &Path) -> Result<Self, GitError> {
        let repo = gix::open(path).map_err(|e| GitError::Open {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        let workdir = repo.workdir().ok_or(GitError::Bare)?;
        let root = workdir.canonicalize().map_err(|e| GitError::Io {
            path: workdir.to_path_buf(),
            reason: e.to_string(),
        })?;
        let commit = repo
            .head_commit()
            .map_err(|e| GitError::Head(e.to_string()))?;
        let head = commit.id.to_string();
        let tree = commit
            .tree_id()
            .map_err(|e| GitError::Head(e.to_string()))?
            .detach();

        let manifest_bytes = match read_tree_file(&repo, tree, MANIFEST_PATH) {
            Ok(bytes) => bytes,
            Err(GitError::MissingFile(_)) => return Err(GitError::MissingManifest(root.clone())),
            Err(err) => return Err(err),
        };
        let manifest_text = String::from_utf8(manifest_bytes).map_err(|e| GitError::Io {
            path: root.join(MANIFEST_PATH),
            reason: e.to_string(),
        })?;
        let manifest = Manifest::from_toml(&manifest_text)?;

        Ok(Self {
            root,
            head,
            tree,
            manifest,
        })
    }

    /// Clone the repository at `source` into `dest`, then verify HEAD and
    /// load the manifest exactly like [`Repository::open()`].
    ///
    /// `source` is a local path, a `file://` URL, or an `https://` URL whose
    /// host is `github.com` or `gitlab.com` (the hosts the `grow://` browser
    /// flow supports); any other scheme or host is rejected, as are URLs
    /// carrying credentials. `dest` must not exist yet or be an empty
    /// directory. The clone is a normal (non-bare) repository with the
    /// source `HEAD` branch checked out into the working tree.
    pub fn clone_from(source: &str, dest: &Path) -> Result<Self, GitError> {
        let url = gix::url::parse(source.as_ref()).map_err(|e| GitError::Clone {
            url: source.to_string(),
            reason: e.to_string(),
        })?;
        match url.scheme {
            gix::url::Scheme::File => {}
            gix::url::Scheme::Https => validate_https_remote(&url)?,
            _ => return Err(GitError::UnsupportedScheme(source.to_string())),
        }

        let mut fetch = gix::prepare_clone(source, dest).map_err(|e| GitError::Clone {
            url: source.to_string(),
            reason: e.to_string(),
        })?;
        let (mut checkout, _outcome) = fetch
            .fetch_then_checkout(gix::progress::Discard, &AtomicBool::new(false))
            .map_err(|e| GitError::Clone {
                url: source.to_string(),
                reason: e.to_string(),
            })?;
        checkout
            .main_worktree(gix::progress::Discard, &AtomicBool::new(false))
            .map_err(|e| GitError::Clone {
                url: source.to_string(),
                reason: e.to_string(),
            })?;

        Self::open(dest)
    }

    /// Canonicalized repository working-tree root, used only to reopen the
    /// object database for reads pinned to the verified commit.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Full HEAD commit id — logged on startup as the verified version of
    /// the application being executed.
    pub fn head_commit(&self) -> &str {
        &self.head
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Read a regular file from the verified HEAD tree. Working-tree changes
    /// made after loading the repository never affect returned bytes.
    pub fn read_file(&self, relative: &str) -> Result<Vec<u8>, GitError> {
        let repo = gix::open(&self.root).map_err(|e| GitError::Open {
            path: self.root.clone(),
            reason: e.to_string(),
        })?;
        read_tree_file(&repo, self.tree, relative)
    }

    /// Whether a regular file is present in the verified HEAD tree.
    pub fn contains_file(&self, relative: &str) -> bool {
        self.read_file(relative).is_ok()
    }
}

/// Remote hosts `grow install` and the `grow://` browser flow may clone
/// from over https. Matching is exact or on subdomains (`www.` included).
const SUPPORTED_REMOTE_HOSTS: [&str; 2] = ["github.com", "gitlab.com"];

/// Gate for `https://` clone sources: only [`SUPPORTED_REMOTE_HOSTS`], and
/// never URLs carrying credentials (which would end up in error messages,
/// logs, and the registry metadata).
fn validate_https_remote(url: &gix::Url) -> Result<(), GitError> {
    if url.user.is_some() {
        return Err(GitError::CredentialsInUrl(url.to_string()));
    }
    let host = url.host().unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(host);
    let allowed = SUPPORTED_REMOTE_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")));
    if !allowed {
        return Err(GitError::UnsupportedHost(
            url.host().unwrap_or_default().to_string(),
        ));
    }
    Ok(())
}

fn read_tree_file(
    repo: &gix::Repository,
    tree_id: gix::hash::ObjectId,
    relative: &str,
) -> Result<Vec<u8>, GitError> {
    let relative = relative.trim_start_matches('/');
    if !grow_core::is_safe_relative_path(relative) {
        return Err(GitError::MissingFile(relative.to_string()));
    }

    let tree = repo.find_tree(tree_id).map_err(|e| GitError::Io {
        path: PathBuf::from(relative),
        reason: e.to_string(),
    })?;
    let entry = tree
        .lookup_entry_by_path(relative)
        .map_err(|e| GitError::Io {
            path: PathBuf::from(relative),
            reason: e.to_string(),
        })?
        .ok_or_else(|| GitError::MissingFile(relative.to_string()))?;
    if !entry.mode().is_blob() {
        return Err(GitError::NotRegularFile(relative.to_string()));
    }
    let object = entry.object().map_err(|e| GitError::Io {
        path: PathBuf::from(relative),
        reason: e.to_string(),
    })?;
    object
        .try_into_blob()
        .map(|blob| blob.data.clone())
        .map_err(|e| GitError::Io {
            path: PathBuf::from(relative),
            reason: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    const MANIFEST: &str = r#"
[grow]
name = "fixture-app"
version = "0.1.0"

[runtime]
engine = "wasm"
entry = "main.wasm"
"#;

    fn fixture_repo(with_manifest: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        if with_manifest {
            fs::create_dir_all(dir.path().join(".grow")).unwrap();
            fs::write(dir.path().join(MANIFEST_PATH), MANIFEST).unwrap();
        }
        fs::write(dir.path().join("hello.txt"), "hello fixture").unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=fixture@grow.test",
            "-c",
            "user.name=grow-fixture",
            "commit",
            "-m",
            "fixture",
        ]);
        dir
    }

    fn git_head(path: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn opens_repo_and_loads_manifest() {
        let dir = fixture_repo(true);
        let repo = Repository::open(dir.path()).expect("repo opens");
        assert_eq!(repo.manifest().grow.name, "fixture-app");
        assert_eq!(repo.manifest().wasm_entry(), Some("main.wasm"));
        assert_eq!(repo.head_commit().len(), 40, "sha1 hex length");
        assert!(repo.read_file("hello.txt").unwrap().starts_with(b"hello"));
    }

    #[test]
    fn clones_repo_from_local_path() {
        let src = fixture_repo(true);
        let dest = tempfile::tempdir().unwrap();
        let dest = dest.path().join("clone");
        let repo =
            Repository::clone_from(src.path().to_str().unwrap(), &dest).expect("repo clones");
        assert_eq!(repo.manifest().grow.name, "fixture-app");
        assert_eq!(repo.head_commit(), git_head(src.path()));
        assert!(repo.read_file("hello.txt").unwrap().starts_with(b"hello"));
    }

    #[test]
    fn clones_repo_from_file_url() {
        let src = fixture_repo(true);
        let dest = tempfile::tempdir().unwrap();
        let url = format!("file://{}", src.path().display());
        let repo = Repository::clone_from(&url, &dest.path().join("clone")).expect("repo clones");
        assert_eq!(repo.head_commit(), git_head(src.path()));
    }

    #[test]
    fn remote_url_scheme_is_rejected() {
        let dest = tempfile::tempdir().unwrap();
        let err = Repository::clone_from("ssh://example.com/repo.git", &dest.path().join("clone"))
            .unwrap_err();
        assert!(matches!(err, GitError::UnsupportedScheme(_)));
    }

    #[test]
    fn https_host_outside_allowlist_is_rejected() {
        let dest = tempfile::tempdir().unwrap();
        let err =
            Repository::clone_from("https://example.com/repo.git", &dest.path().join("clone"))
                .unwrap_err();
        assert!(matches!(err, GitError::UnsupportedHost(_)));
    }

    #[test]
    fn https_host_allowlist_accepts_github_and_gitlab() {
        for source in [
            "https://github.com/owner/repo.git",
            "https://www.github.com/owner/repo",
            "https://gitlab.com/group/subgroup/repo.git",
        ] {
            let url = gix::url::parse(source.as_ref()).unwrap();
            assert!(validate_https_remote(&url).is_ok(), "{source}");
        }
    }

    #[test]
    fn https_url_with_credentials_is_rejected() {
        let dest = tempfile::tempdir().unwrap();
        let err = Repository::clone_from(
            "https://user:token@github.com/owner/repo.git",
            &dest.path().join("clone"),
        )
        .unwrap_err();
        assert!(matches!(err, GitError::CredentialsInUrl(_)));
    }

    #[test]
    fn clone_into_non_empty_dir_is_rejected() {
        let src = fixture_repo(true);
        let dest = tempfile::tempdir().unwrap();
        fs::write(dest.path().join("occupied.txt"), "not empty").unwrap();
        let err = Repository::clone_from(src.path().to_str().unwrap(), dest.path()).unwrap_err();
        assert!(matches!(err, GitError::Clone { .. }));
    }

    #[test]
    fn missing_manifest_is_reported() {
        let dir = fixture_repo(false);
        let err = Repository::open(dir.path()).unwrap_err();
        assert!(matches!(err, GitError::MissingManifest(_)));
    }

    #[test]
    fn not_a_repo_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let err = Repository::open(dir.path()).unwrap_err();
        assert!(matches!(err, GitError::Open { .. }));
    }

    #[test]
    fn bare_repo_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let out = Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git init --bare failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let err = Repository::open(dir.path()).unwrap_err();
        assert!(matches!(err, GitError::Bare));
    }

    #[test]
    fn read_file_returns_exact_bytes() {
        let dir = fixture_repo(true);
        let repo = Repository::open(dir.path()).unwrap();
        let bytes = repo.read_file("hello.txt").unwrap();
        assert_eq!(bytes, b"hello fixture");
    }

    #[test]
    fn contains_file_detects_present_and_absent() {
        let dir = fixture_repo(true);
        let repo = Repository::open(dir.path()).unwrap();
        assert!(repo.contains_file("hello.txt"));
        assert!(repo.contains_file(".grow/manifest.toml"));
        assert!(!repo.contains_file("nonexistent.txt"));
    }

    #[test]
    fn path_traversal_is_blocked() {
        let dir = fixture_repo(true);
        let repo = Repository::open(dir.path()).unwrap();
        assert!(matches!(
            repo.read_file("../outside.txt"),
            Err(GitError::MissingFile(_))
        ));
        assert!(matches!(
            repo.read_file("a/../../b"),
            Err(GitError::MissingFile(_))
        ));
    }

    #[test]
    fn repo_without_git_dir_fails() {
        let dir = tempfile::tempdir().unwrap();
        let err = Repository::open(dir.path()).unwrap_err();
        assert!(matches!(err, GitError::Open { .. }));
    }

    #[test]
    fn manifest_path_is_constant() {
        assert_eq!(grow_core::MANIFEST_PATH, ".grow/manifest.toml");
    }

    #[test]
    fn root_returns_canonical_path() {
        let dir = fixture_repo(true);
        let repo = Repository::open(dir.path()).unwrap();
        let root = repo.root();
        assert!(root.is_absolute());
        assert!(root.exists());
    }

    #[test]
    fn directories_are_not_regular_files() {
        let dir = fixture_repo(true);
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git runs");
            assert!(out.status.success());
        };
        git(&[
            "-c",
            "user.email=test@grow",
            "-c",
            "user.name=test",
            "commit",
            "--amend",
            "-m",
            "fixture",
        ]);
        let repo = Repository::open(dir.path()).unwrap();
        assert!(matches!(
            repo.read_file(".grow"),
            Err(GitError::NotRegularFile(_))
        ));
    }
}
