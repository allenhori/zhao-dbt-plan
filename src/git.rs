//! Git operations backing `state:`-selector resolution (§ "State
//! comparison" -- a `--select`/`--exclude` expression using dbt's
//! `state:` method, e.g. `state:modified+`, needs a second, already-
//! compiled manifest to compare against). When the user doesn't supply
//! one explicitly via `--state`, this resolves it git-natively: the
//! merge-base commit between `HEAD` and `--against` (default `"master"`,
//! or `zhao.yml`'s `against` -- the same key `zhao-cli` itself uses, so
//! one setting serves both tools), checked out into a temporary worktree
//! and compiled there.
//!
//! Deliberately independent of anything dbt-specific -- resolving a
//! merge-base and checking it out has nothing to do with dbt, or even
//! this addon. Mirrors `zhao-core`'s own `git.rs` in the `zhao-cli` repo
//! closely (same mechanism, same shape) -- not shared as a dependency,
//! since this addon has no technical dependency on `zhao-cli`/`zhao-core`
//! at all (see ADR 0010), but no reason to diverge in approach either.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A git worktree checked out at a specific commit, in a temporary
/// directory. Removed (`git worktree remove`) when dropped, so a caller
/// never needs to remember cleanup, and a panic partway through doesn't
/// leak it either.
#[derive(Debug)]
pub struct Worktree {
    path: PathBuf,
    repo_root: PathBuf,
}

impl Worktree {
    /// The worktree's checked-out path on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        // Best-effort: nothing meaningful to do with a failure here, and
        // no caller left to report it to from a `Drop` impl.
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(&self.path)
            .output();
    }
}

/// Everything that can go wrong resolving a git-native `state:` baseline.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// `dir` isn't inside a git repository (or `git` couldn't otherwise
    /// determine its toplevel).
    #[error("{dir}: not inside a git repository ({stderr})")]
    NotAGitRepository {
        /// The directory that isn't inside a git repository.
        dir: String,
        /// `git`'s captured stderr.
        stderr: String,
    },
    /// No merge-base could be found between `HEAD` and `against`.
    #[error(
        "could not find a merge-base between HEAD and {against:?} in {repo_root} -- the \
         histories may be unrelated, or {against:?} may not exist{stderr_detail}"
    )]
    MergeBaseNotFound {
        /// The repository the merge-base was sought in.
        repo_root: String,
        /// The ref `HEAD` was compared against.
        against: String,
        /// `git`'s captured stderr, pre-formatted as `" (...)"`, or empty.
        stderr_detail: String,
    },
    /// `git worktree add` ran but exited with a failure.
    #[error("could not create a git worktree for commit {commit} in {repo_root}: {stderr}")]
    WorktreeCreationFailed {
        /// The repository the worktree was created from.
        repo_root: String,
        /// The commit the worktree was checked out at.
        commit: String,
        /// `git`'s captured stderr.
        stderr: String,
    },
    /// `git` itself could not be run.
    #[error("could not run git -- is it installed and on PATH? ({source})")]
    CommandNotFound {
        /// The underlying I/O error from trying to spawn `git`.
        #[source]
        source: std::io::Error,
    },
}

/// Finds the root of the git repository containing `dir`.
pub fn repo_root(dir: &Path) -> Result<PathBuf, GitError> {
    let output = run_git(dir, &["rev-parse", "--show-toplevel"])?;
    if !output.status.success() {
        return Err(GitError::NotAGitRepository {
            dir: dir.display().to_string(),
            stderr: stderr_of(&output),
        });
    }
    Ok(PathBuf::from(stdout_of(&output)))
}

/// Resolves the merge-base commit SHA between `HEAD` and `against`.
pub fn resolve_merge_base(repo_root: &Path, against: &str) -> Result<String, GitError> {
    let output = run_git(repo_root, &["merge-base", "HEAD", against])?;
    if !output.status.success() {
        return Err(GitError::MergeBaseNotFound {
            repo_root: repo_root.display().to_string(),
            against: against.to_string(),
            stderr_detail: stderr_detail_of(&output),
        });
    }
    Ok(stdout_of(&output))
}

/// Creates a new worktree in a fresh temporary directory, checked out at
/// `commit`.
pub fn create_worktree(repo_root: &Path, commit: &str) -> Result<Worktree, GitError> {
    let dir = tempfile::Builder::new()
        .prefix("zhao-dbt-plan-state-")
        .tempdir()
        .map_err(|source| GitError::CommandNotFound { source })?;
    // `git worktree add <path>` refuses to check out into a directory
    // that already exists -- the temp dir only reserves a unique path,
    // then `close` removes it immediately (consuming the `TempDir`
    // outright, so nothing lingers to double-remove the real worktree
    // later). `Worktree`'s own `Drop` takes over cleanup from here.
    let path = dir.path().to_path_buf();
    dir.close()
        .map_err(|source| GitError::WorktreeCreationFailed {
            repo_root: repo_root.display().to_string(),
            commit: commit.to_string(),
            stderr: source.to_string(),
        })?;

    let output = run_git(
        repo_root,
        &[
            "worktree",
            "add",
            "--detach",
            path.to_str().unwrap_or_default(),
            commit,
        ],
    )?;
    if !output.status.success() {
        return Err(GitError::WorktreeCreationFailed {
            repo_root: repo_root.display().to_string(),
            commit: commit.to_string(),
            stderr: stderr_of(&output),
        });
    }

    Ok(Worktree {
        path,
        repo_root: repo_root.to_path_buf(),
    })
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<std::process::Output, GitError> {
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|source| GitError::CommandNotFound { source })
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn stderr_detail_of(output: &std::process::Output) -> String {
    let stderr = stderr_of(output);
    if stderr.is_empty() {
        String::new()
    } else {
        format!(" ({stderr})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRepo {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    impl TestRepo {
        fn git(&self, args: &[&str]) -> std::process::Output {
            Command::new("git")
                .current_dir(&self.path)
                .args(args)
                .output()
                .expect("git should be runnable in tests")
        }

        fn commit(&self, message: &str) -> String {
            std::fs::write(self.path.join("file.txt"), message).expect("should write file");
            self.git(&["add", "."]);
            let output = self.git(&["commit", "-m", message]);
            assert!(output.status.success(), "commit should succeed: {output:?}");
            stdout_of(&self.git(&["rev-parse", "HEAD"]))
        }
    }

    fn new_test_repo() -> TestRepo {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let path = dir.path().to_path_buf();
        let repo = TestRepo { _dir: dir, path };

        let init = repo.git(&["init", "--initial-branch=master"]);
        assert!(init.status.success(), "git init should succeed: {init:?}");
        repo.git(&["config", "user.email", "test@zhao.invalid"]);
        repo.git(&["config", "user.name", "zhao test"]);
        repo
    }

    #[test]
    fn repo_root_resolves_to_the_repositorys_toplevel() {
        let repo = new_test_repo();
        repo.commit("initial");
        let nested = repo.path.join("models");
        std::fs::create_dir_all(&nested).expect("should create nested dir");

        let root = repo_root(&nested).expect("should resolve repo root");
        assert_eq!(
            std::fs::canonicalize(&root).expect("should canonicalize"),
            std::fs::canonicalize(&repo.path).expect("should canonicalize")
        );
    }

    #[test]
    fn repo_root_produces_a_clear_error_outside_any_git_repository() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let result = repo_root(dir.path());
        assert!(matches!(result, Err(GitError::NotAGitRepository { .. })));
    }

    #[test]
    fn resolve_merge_base_finds_the_common_ancestor_of_head_and_a_branch() {
        let repo = new_test_repo();
        let base_commit = repo.commit("on master");
        repo.git(&["checkout", "-b", "feature"]);
        repo.commit("on feature, ahead of master");

        let merge_base =
            resolve_merge_base(&repo.path, "master").expect("should find a merge-base");
        assert_eq!(merge_base, base_commit);
    }

    #[test]
    fn resolve_merge_base_produces_a_clear_error_when_against_does_not_exist() {
        let repo = new_test_repo();
        repo.commit("initial");
        let result = resolve_merge_base(&repo.path, "this-branch-does-not-exist");
        assert!(matches!(result, Err(GitError::MergeBaseNotFound { .. })));
    }

    #[test]
    fn create_worktree_checks_out_the_given_commit_in_a_new_directory() {
        let repo = new_test_repo();
        let first_commit = repo.commit("first");
        repo.commit("second");

        let worktree = create_worktree(&repo.path, &first_commit).expect("should create worktree");
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("file.txt")).expect("should read file"),
            "first",
            "the worktree should reflect the checked-out commit, not HEAD"
        );
    }

    #[test]
    fn create_worktree_cleans_up_its_directory_when_dropped() {
        let repo = new_test_repo();
        let commit = repo.commit("only commit");

        let worktree = create_worktree(&repo.path, &commit).expect("should create worktree");
        let path = worktree.path().to_path_buf();
        assert!(path.exists());

        drop(worktree);
        assert!(!path.exists(), "worktree directory should be removed");
    }
}
