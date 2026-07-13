//! grow-git: repository loading for GROW.
//!
//! Uses `gix` (pure-Rust git) — git is never shelled out to at runtime.
//! Milestone 1 scope: open a local repository, resolve and expose the HEAD
//! commit (the "verify commit" lifecycle step), and read files from the
//! verified commit tree with path-traversal protection. Remote fetch, shallow
//! clones, and object streaming arrive in Milestone 3.

use std::path::{Path, PathBuf};

use grow_core::{MANIFEST_PATH, Manifest, ManifestError};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("cannot open git repository at {path}: {reason}")]
    Open { path: PathBuf, reason: String },
    #[error("bare repositories are not supported (no working tree)")]
    Bare,
    #[error("cannot resolve HEAD commit: {0}")]
    Head(String),
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
    fn working_tree_changes_are_not_served() {
        let dir = fixture_repo(true);
        let repo = Repository::open(dir.path()).unwrap();
        fs::write(dir.path().join("hello.txt"), "uncommitted replacement").unwrap();
        fs::write(dir.path().join("untracked.txt"), "untracked").unwrap();

        assert_eq!(repo.read_file("hello.txt").unwrap(), b"hello fixture");
        assert!(matches!(
            repo.read_file("untracked.txt"),
            Err(GitError::MissingFile(_))
        ));
        assert!(matches!(
            repo.read_file("../../etc/passwd"),
            Err(GitError::MissingFile(_))
        ));
    }
}
