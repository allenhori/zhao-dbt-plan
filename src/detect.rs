//! The very first check `run()` makes: is `--project-dir` actually a dbt
//! project at all? `zhao-dbt-plan` is a dbt-only Addon by design (see the
//! `zhao` planning repo's ADR 0011 for why zhao-cli itself gets a
//! pluggable multi-tool adapter boundary and this doesn't -- there's only
//! ever one "adapter" here, so there's nothing to select between) -- but
//! everything downstream of this check (`refresh::ensure_fresh`,
//! `select::resolve`, `state::resolve`) assumes a real dbt project and
//! fails with a `dbt`-subprocess-shaped error if it isn't one, which is a
//! confusing way to learn "you're in the wrong directory." Checking the
//! one thing dbt itself requires -- a `dbt_project.yml` -- upfront, before
//! any of that runs, turns that into one clear, actionable message
//! instead.

use std::path::Path;

/// `dbt_project.yml` not found in `project_dir` produced this error.
#[derive(Debug, thiserror::Error)]
#[error(
    "{project_dir} does not look like a dbt project -- no dbt_project.yml found there. \
     zhao-dbt-plan only works inside a dbt project: run it from the project root, or pass \
     --project-dir pointing at one."
)]
pub struct NotADbtProjectError {
    project_dir: String,
}

/// Fails fast with a clear, actionable error if `project_dir` doesn't
/// contain a `dbt_project.yml` -- the one file dbt itself requires to
/// recognize a directory as a project at all, so it's the natural, only
/// signal to check here too.
pub fn ensure_dbt_project(project_dir: &Path) -> Result<(), NotADbtProjectError> {
    if project_dir.join("dbt_project.yml").exists() {
        return Ok(());
    }
    Err(NotADbtProjectError {
        project_dir: project_dir.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_with_dbt_project_yml_passes() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        std::fs::write(dir.path().join("dbt_project.yml"), "name: whatever\n")
            .expect("should write dbt_project.yml");

        assert!(ensure_dbt_project(dir.path()).is_ok());
    }

    #[test]
    fn a_directory_without_dbt_project_yml_produces_a_clear_error() {
        let dir = tempfile::tempdir().expect("should create tempdir");

        let err = ensure_dbt_project(dir.path()).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("dbt_project.yml"),
            "error should name the specific file that's missing: {message}"
        );
        assert!(
            message.contains(&dir.path().display().to_string()),
            "error should name the directory that was checked: {message}"
        );
    }
}
