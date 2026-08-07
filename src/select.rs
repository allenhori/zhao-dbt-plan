//! Resolves `--select`/`--exclude` by shelling out to `dbt ls` -- dbt's
//! own selector engine is the authoritative implementation of dbt's
//! selector grammar (methods like `tag:`/`state:`/`source:`, graph
//! operators `+`/`@`, comma for intersection, space for union, ...);
//! independently reimplementing a subset of it risked silently
//! diverging from real dbt syntax the moment a user reached past that
//! subset. `dbt ls` is read-only -- no warehouse touch, nothing built or
//! run -- the same risk profile as the manifest-freshness `dbt parse`
//! call (§5 of the spec), so this is that same narrow, deliberate
//! exception extended to selection, not a new category of dependency on
//! `dbt` being invokable.
//!
//! `dbt ls --output json` was considered too (it exists, and gives full
//! per-node data, not just names) -- deliberately not used: this addon
//! already reads the schema-versioned, officially documented
//! `manifest.json` (`dbt_schema_version`) for actual node data, and
//! `dbt ls`'s JSON shape has no equivalent formal stability guarantee.
//! Using `dbt ls` for *only* what it's uniquely authoritative for --
//! resolving the selector expression -- and cross-referencing the
//! resulting names against the already-loaded manifest keeps the
//! manifest as the single source of truth for data, `dbt ls` as the
//! single source of truth for selection.
//!
//! Net effect: `dbt` must be invokable for *every* run now, not just
//! when the manifest happens to be stale -- an intentional trade-off,
//! since matching exactly what a user's own `dbt build --select ...`
//! would target matters more than avoiding one more `dbt` subprocess
//! call.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use crate::manifest::{Manifest, ResourceType};

/// Everything that can go wrong resolving a selection via `dbt ls`.
#[derive(Debug, thiserror::Error)]
pub enum SelectError {
    /// The `dbt` command itself couldn't be spawned (not found, not
    /// executable, ...), or `--select`/`--exclude`/`--dbt-command`
    /// couldn't be parsed as shell words.
    #[error("could not run `{dbt_command} ls`: {source}")]
    Spawn {
        /// The `--dbt-command` that failed to spawn.
        dbt_command: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// `dbt ls` ran but exited non-zero (e.g. an invalid `--select`
    /// expression).
    #[error("`{dbt_command} ls` failed:\nstdout:\n{stdout}\nstderr:\n{stderr}")]
    Failed {
        /// The `--dbt-command` that failed.
        dbt_command: String,
        /// Its captured stdout.
        stdout: String,
        /// Its captured stderr.
        stderr: String,
    },
    /// `dbt ls` selected a model name this addon's own (separately read)
    /// manifest doesn't know about -- almost certainly a stale manifest,
    /// since `dbt ls` and the manifest should always describe the same
    /// compiled project state.
    #[error(
        "`{dbt_command} ls` selected {name:?}, which isn't a model in the compiled manifest -- \
         the manifest at --manifest is likely out of sync with the current project state; \
         delete it and rerun (or point --manifest at a freshly compiled one)"
    )]
    UnknownModel {
        /// The `--dbt-command` used.
        dbt_command: String,
        /// The unrecognized model name.
        name: String,
    },
}

/// Resolves `select`/`exclude` (dbt's own selector syntax, forwarded
/// verbatim -- see the module doc comment) against the real project at
/// `project_dir`, returning the matched model `unique_id`s, cross-
/// referenced against `manifest` (already-loaded, so this doesn't need
/// to parse `dbt ls`'s own output into a full `Node` itself).
pub fn resolve(
    project_dir: &Path,
    manifest: &Manifest,
    dbt_command: &str,
    dbt_args: &[String],
    select: &str,
    exclude: Option<&str>,
) -> Result<HashSet<String>, SelectError> {
    let mut parts = shell_words::split(dbt_command).map_err(|e| SelectError::Spawn {
        dbt_command: dbt_command.to_string(),
        source: std::io::Error::other(e.to_string()),
    })?;
    if parts.is_empty() {
        return Err(SelectError::Spawn {
            dbt_command: dbt_command.to_string(),
            source: std::io::Error::other("--dbt-command resolved to an empty command"),
        });
    }
    let program = parts.remove(0);

    let select_parts = shell_words::split(select).map_err(|e| SelectError::Spawn {
        dbt_command: dbt_command.to_string(),
        source: std::io::Error::other(format!("could not parse --select {select:?}: {e}")),
    })?;

    let mut command = Command::new(&program);
    command
        .args(&parts)
        .arg("ls")
        .arg("--resource-type")
        .arg("model")
        .arg("--output")
        .arg("name")
        .arg("--quiet")
        .arg("--select")
        .args(&select_parts);

    if let Some(exclude) = exclude {
        let exclude_parts = shell_words::split(exclude).map_err(|e| SelectError::Spawn {
            dbt_command: dbt_command.to_string(),
            source: std::io::Error::other(format!("could not parse --exclude {exclude:?}: {e}")),
        })?;
        command.arg("--exclude").args(&exclude_parts);
    }

    command.args(dbt_args).current_dir(project_dir);

    let output = command.output().map_err(|source| SelectError::Spawn {
        dbt_command: dbt_command.to_string(),
        source,
    })?;
    if !output.status.success() {
        return Err(SelectError::Failed {
            dbt_command: dbt_command.to_string(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut selected = HashSet::new();
    for line in stdout.lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        let unique_id = manifest
            .nodes
            .values()
            .find(|n| n.resource_type == ResourceType::Model && n.name == name)
            .map(|n| n.unique_id.clone())
            .ok_or_else(|| SelectError::UnknownModel {
                dbt_command: dbt_command.to_string(),
                name: name.to_string(),
            })?;
        selected.insert(unique_id);
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Node;
    use std::os::unix::fs::PermissionsExt;

    fn model(id: &str, name: &str) -> Node {
        Node {
            unique_id: id.to_string(),
            name: name.to_string(),
            resource_type: ResourceType::Model,
            depends_on: Vec::new(),
            event_time: None,
            zhao_meta: None,
        }
    }

    fn manifest_of(nodes: Vec<Node>) -> Manifest {
        Manifest {
            nodes: nodes
                .into_iter()
                .map(|n| (n.unique_id.clone(), n))
                .collect(),
            generated_at: None,
        }
    }

    /// A stub standing in for real `dbt` on `PATH` -- a tiny shell
    /// script, so this test doesn't need a real dbt project or
    /// installation. The real behavior is proven end to end by
    /// `tests/end_to_end.rs` against a real, self-contained dbt project
    /// and real `dbt`/`dbt-fusion` installs instead.
    fn stub_dbt_command(dir: &Path, stdout: &str) -> std::path::PathBuf {
        use std::io::Write as _;

        let script_path = dir.join("stub_dbt.sh");
        // Explicit File + write_all + sync_all + drop (closing the fd),
        // rather than std::fs::write -- on Linux, execve fails with
        // ETXTBSY ("Text file busy") if the file is still considered
        // open-for-writing by the kernel at exec time; sync_all before
        // the handle drops ensures the write is fully flushed and closed
        // before this function returns, closing the same race
        // ETXTBSY-on-Linux-CI bug this project has hit before (see
        // zhao-cli's git history for the NamedTempFile variant of it).
        let mut file = std::fs::File::create(&script_path).expect("should create stub script");
        file.write_all(format!("#!/bin/sh\ncat <<'EOF'\n{stdout}EOF\n").as_bytes())
            .expect("should write stub script");
        file.sync_all().expect("should flush stub script");
        drop(file);

        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("should set executable permission");
        script_path
    }

    #[test]
    fn maps_dbt_ls_output_names_back_to_unique_ids() {
        let manifest = manifest_of(vec![model("model.p.a", "a"), model("model.p.b", "b")]);
        let dir = tempfile::tempdir().expect("should create tempdir");
        let stub = stub_dbt_command(dir.path(), "a\nb\n");

        let selected = resolve(
            dir.path(),
            &manifest,
            &stub.display().to_string(),
            &[],
            "tag:whatever",
            None,
        )
        .expect("should resolve");

        assert_eq!(
            selected,
            HashSet::from(["model.p.a".to_string(), "model.p.b".to_string()])
        );
    }

    #[test]
    fn a_name_dbt_ls_returns_but_the_manifest_does_not_know_is_a_clear_error() {
        let manifest = manifest_of(vec![model("model.p.a", "a")]);
        let dir = tempfile::tempdir().expect("should create tempdir");
        let stub = stub_dbt_command(dir.path(), "nonexistent_model\n");

        let err = resolve(
            dir.path(),
            &manifest,
            &stub.display().to_string(),
            &[],
            "tag:whatever",
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, SelectError::UnknownModel { name, .. } if name == "nonexistent_model")
        );
    }

    #[test]
    fn blank_lines_in_dbt_ls_output_are_ignored() {
        let manifest = manifest_of(vec![model("model.p.a", "a")]);
        let dir = tempfile::tempdir().expect("should create tempdir");
        let stub = stub_dbt_command(dir.path(), "a\n\n\n");

        let selected = resolve(
            dir.path(),
            &manifest,
            &stub.display().to_string(),
            &[],
            "tag:whatever",
            None,
        )
        .expect("should resolve");
        assert_eq!(selected, HashSet::from(["model.p.a".to_string()]));
    }

    #[test]
    fn a_nonexistent_dbt_command_produces_a_clear_spawn_error() {
        let manifest = manifest_of(vec![model("model.p.a", "a")]);
        let dir = tempfile::tempdir().expect("should create tempdir");

        let err = resolve(
            dir.path(),
            &manifest,
            "this-command-does-not-exist",
            &[],
            "tag:whatever",
            None,
        )
        .unwrap_err();
        assert!(matches!(err, SelectError::Spawn { .. }));
    }
}
