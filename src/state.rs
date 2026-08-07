//! Resolves the manifest directory to pass as `--state` to `dbt ls`, for
//! a `--select`/`--exclude` expression that uses dbt's `state:` method
//! (e.g. `state:modified+`).
//!
//! Three ways this gets satisfied, in priority order:
//! 1. `--state <path>` explicitly given -- used as-is, no git involved
//!    at all. Wins over `--against` outright if both are somehow
//!    relevant, the same precedent `zhao-cli`'s own git-native Baseline
//!    resolution already sets for `--state` vs `--against`.
//! 2. Neither given, but `--select`/`--exclude` contains the substring
//!    `"state:"` -- git-native resolution: the merge-base commit between
//!    `HEAD` and `--against` (default `"master"`, or `zhao.yml`'s
//!    `against` -- the same key `zhao-cli` itself uses), compiled in a
//!    temporary worktree.
//! 3. Neither given, and no `"state:"` anywhere in the selector -- no
//!    state resolution attempted at all, since `dbt ls` doesn't need one
//!    and compiling a whole extra worktree would be pure waste.

use std::path::{Path, PathBuf};

use crate::git::{self, GitError, Worktree};

/// The resolved `--state` value to pass to `dbt ls`, if any was needed.
pub struct StateSource {
    manifest_dir: PathBuf,
    /// `None` for an explicitly-supplied `--state` path (nothing to
    /// clean up); `Some` for a git-natively resolved one, kept alive for
    /// as long as this value lives so the worktree isn't removed out
    /// from under `dbt ls` before it runs.
    _worktree: Option<Worktree>,
}

impl StateSource {
    /// The directory to pass as `dbt ls --state <dir>`.
    pub fn manifest_dir(&self) -> &Path {
        &self.manifest_dir
    }
}

/// Everything that can go wrong resolving a `state:` baseline.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// A git operation failed (see [`GitError`]).
    #[error(transparent)]
    Git(#[from] GitError),
    /// The internal `dbt deps`/`dbt parse` call (compiling the merge-base
    /// commit in its worktree) failed.
    #[error("could not compile the state:-comparison baseline: {0}")]
    DbtFailed(String),
}

/// Resolves the `--state` value, if one is needed -- see the module doc
/// comment for the three-way priority.
#[allow(clippy::too_many_arguments)]
pub fn resolve(
    project_dir: &Path,
    select: &str,
    exclude: Option<&str>,
    explicit_state: Option<&Path>,
    against: &str,
    dbt_command: &str,
    dbt_args: &[String],
) -> Result<Option<StateSource>, StateError> {
    if let Some(path) = explicit_state {
        return Ok(Some(StateSource {
            manifest_dir: path.to_path_buf(),
            _worktree: None,
        }));
    }

    let needs_state =
        select.contains("state:") || exclude.is_some_and(|exclude| exclude.contains("state:"));
    if !needs_state {
        return Ok(None);
    }

    let repo_root = git::repo_root(project_dir)?;
    let canonical_repo_root = repo_root.canonicalize().unwrap_or(repo_root.clone());
    let canonical_project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let relative_project_dir = canonical_project_dir
        .strip_prefix(&canonical_repo_root)
        .unwrap_or(Path::new("."));

    let merge_base = git::resolve_merge_base(&canonical_repo_root, against)?;
    let worktree = git::create_worktree(&canonical_repo_root, &merge_base)?;
    let worktree_project_dir = worktree.path().join(relative_project_dir);

    if worktree_project_dir.join("packages.yml").exists()
        || worktree_project_dir.join("dependencies.yml").exists()
    {
        run_dbt(&worktree_project_dir, dbt_command, "deps", dbt_args)?;
    }
    run_dbt(&worktree_project_dir, dbt_command, "parse", dbt_args)?;

    let manifest_dir = worktree_project_dir.join("target");
    Ok(Some(StateSource {
        manifest_dir,
        _worktree: Some(worktree),
    }))
}

fn run_dbt(
    project_dir: &Path,
    dbt_command: &str,
    subcommand: &str,
    dbt_args: &[String],
) -> Result<(), StateError> {
    let mut parts = shell_words::split(dbt_command)
        .map_err(|e| StateError::DbtFailed(format!("could not parse --dbt-command: {e}")))?;
    if parts.is_empty() {
        return Err(StateError::DbtFailed(
            "--dbt-command resolved to an empty command".to_string(),
        ));
    }
    let program = parts.remove(0);

    let output = std::process::Command::new(&program)
        .args(&parts)
        .arg(subcommand)
        .args(dbt_args)
        .current_dir(project_dir)
        .output()
        .map_err(|e| {
            StateError::DbtFailed(format!("could not run `{dbt_command} {subcommand}`: {e}"))
        })?;

    if !output.status.success() {
        return Err(StateError::DbtFailed(format!(
            "`{dbt_command} {subcommand}` failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_state_path_is_used_as_is_with_no_git_involved() {
        let project_dir = Path::new("/does/not/need/to/exist/for/this/case");
        let explicit = Path::new("/some/precompiled/manifest/dir");

        let resolved = resolve(
            project_dir,
            "state:modified+",
            None,
            Some(explicit),
            "master",
            "dbt",
            &[],
        )
        .expect("should resolve without touching git at all");

        assert_eq!(resolved.unwrap().manifest_dir(), explicit);
    }

    #[test]
    fn no_state_method_in_the_selector_resolves_to_none() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        let resolved = resolve(dir.path(), "tag:daily", None, None, "master", "dbt", &[])
            .expect("should resolve");
        assert!(resolved.is_none());
    }

    #[test]
    fn a_state_method_in_exclude_also_triggers_resolution_need() {
        // Not inside a git repo -- this specifically proves the
        // "needs_state" check considered `exclude`, not just `select`,
        // by observing it actually attempted (and failed on) git
        // resolution rather than silently returning None.
        let dir = tempfile::tempdir().expect("should create tempdir");
        let result = resolve(
            dir.path(),
            "tag:daily",
            Some("state:modified"),
            None,
            "master",
            "dbt",
            &[],
        );
        assert!(matches!(
            result,
            Err(StateError::Git(GitError::NotAGitRepository { .. }))
        ));
    }
}
