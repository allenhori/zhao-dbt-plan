//! Manifest freshness auto-refresh (§5 of the spec): a narrow, deliberate
//! exception to this addon's otherwise-strict "never invoke dbt" rule --
//! `manifest.json` is conventionally gitignored, so CI environments
//! routinely won't have a fresh one checked out, and `dbt parse` is
//! read-only (never touches a warehouse).

use std::path::Path;
use std::process::Command;

/// Root-level files that feed `dbt compile`/`dbt parse` directly.
const DBT_SOURCE_ROOT_FILES: &[&str] = &["dbt_project.yml", "packages.yml", "dependencies.yml"];

/// Conventional dbt source directories -- everything under these feeds
/// compilation.
const DBT_SOURCE_DIRS: &[&str] = &[
    "models",
    "macros",
    "seeds",
    "snapshots",
    "analyses",
    "tests",
];

/// If `manifest_path` is missing or older than any of `project_dir`'s own
/// dbt source files, runs `<dbt_command> parse <dbt_args>` (cwd
/// `project_dir`) to refresh it. A no-op if the manifest is already
/// fresh, or if `project_dir` has no dbt source files to compare against
/// at all (nothing to refresh from).
pub fn ensure_fresh(
    project_dir: &Path,
    manifest_path: &Path,
    dbt_command: &str,
    dbt_args: &[String],
) -> Result<(), String> {
    let Some(newest_source) = newest_dbt_source_mtime(project_dir) else {
        return Ok(());
    };
    let manifest_mtime = std::fs::metadata(manifest_path).and_then(|m| m.modified());
    let is_stale = match manifest_mtime {
        Ok(mtime) => newest_source > mtime,
        Err(_) => true, // missing manifest counts as stale
    };
    if !is_stale {
        return Ok(());
    }

    let mut parts = shell_words::split(dbt_command)
        .map_err(|e| format!("could not parse --dbt-command {dbt_command:?}: {e}"))?;
    if parts.is_empty() {
        return Err("--dbt-command resolved to an empty command".to_string());
    }
    let program = parts.remove(0);

    let output = Command::new(&program)
        .args(&parts)
        .arg("parse")
        .args(dbt_args)
        .current_dir(project_dir)
        .output()
        .map_err(|e| format!("could not run `{dbt_command} parse`: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "`{dbt_command} parse` failed to refresh the manifest:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}

fn newest_dbt_source_mtime(project_dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut consider = |path: &Path| {
        if let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) {
            if newest.is_none_or(|current| modified > current) {
                newest = Some(modified);
            }
        }
    };

    for file_name in DBT_SOURCE_ROOT_FILES {
        consider(&project_dir.join(file_name));
    }
    for dir_name in DBT_SOURCE_DIRS {
        walk_mtimes(&project_dir.join(dir_name), &mut consider);
    }

    newest
}

fn walk_mtimes(dir: &Path, consider: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_mtimes(&path, consider);
        } else {
            consider(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_mtime(path: &Path, seconds_since_epoch: u64) {
        let time =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds_since_epoch);
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("file should be openable")
            .set_modified(time)
            .expect("mtime should be settable");
    }

    #[test]
    fn no_dbt_project_present_never_refreshes() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        let manifest = dir.path().join("manifest.json");
        // A command that would fail loudly if actually invoked --
        // proves refresh was skipped entirely.
        let result = ensure_fresh(dir.path(), &manifest, "this-command-does-not-exist", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn a_fresh_manifest_never_refreshes() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        std::fs::write(dir.path().join("dbt_project.yml"), "name: fixture\n")
            .expect("should write dbt_project.yml");
        set_mtime(&dir.path().join("dbt_project.yml"), 1_000);

        let manifest = dir.path().join("manifest.json");
        std::fs::write(&manifest, "{}").expect("should write manifest");
        set_mtime(&manifest, 2_000);

        let result = ensure_fresh(dir.path(), &manifest, "this-command-does-not-exist", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn a_missing_manifest_with_a_real_dbt_project_attempts_refresh_and_surfaces_the_failure() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        std::fs::write(dir.path().join("dbt_project.yml"), "name: fixture\n")
            .expect("should write dbt_project.yml");

        let manifest = dir.path().join("target").join("manifest.json");
        let err = ensure_fresh(dir.path(), &manifest, "this-command-does-not-exist", &[])
            .expect_err("a nonexistent dbt command should fail, not silently succeed");
        assert!(err.contains("this-command-does-not-exist"), "{err}");
    }
}
