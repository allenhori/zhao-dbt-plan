//! Reads the `dbt-plan:` block of `zhao.yml` -- `dbt-command`,
//! `dbt-args`, `max-window-expansion-days` (§10 of the spec).
//!
//! Uses the same "walk up to the nearest `.git`, root-to-leaf merge"
//! discovery convention `zhao-cli` itself uses for `zhao.yml`, so a
//! project already using `zhao-cli` doesn't have to learn a second
//! config-discovery rule for this addon -- implemented independently
//! here rather than as a shared dependency, since this addon has no
//! technical dependency on `zhao-cli`/`zhao-core` at all (see ADR 0010).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Everything that can go wrong reading `zhao.yml`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A `zhao.yml` was found but isn't valid YAML, or doesn't match the
    /// expected shape.
    #[error("{path}: {source}")]
    Parse {
        /// The path that failed.
        path: String,
        /// The underlying YAML error.
        #[source]
        source: serde_yaml::Error,
    },
    /// A `zhao.yml` exists but couldn't be read (permissions, ...).
    #[error("{path}: {source}")]
    Io {
        /// The path that failed.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// The resolved `dbt-plan:` settings, root-to-leaf merged across every
/// `zhao.yml` found between the project directory and the repo root.
#[derive(Debug, Default, Clone)]
pub struct Config {
    /// `dbt-plan.dbt-command`, if set anywhere in the chain.
    pub dbt_command: Option<String>,
    /// `dbt-plan.dbt-args`, if set anywhere in the chain.
    pub dbt_args: Option<String>,
    /// `dbt-plan.max-window-expansion-days`, if set anywhere in the
    /// chain. Callers default this to `90` when `None`.
    pub max_window_expansion_days: Option<i64>,
}

impl Config {
    /// Loads the effective config for `project_dir`, same discovery
    /// convention as `zhao-cli`'s own `Config::load_for_project`.
    pub fn load_for_project(project_dir: &Path) -> Result<Config, ConfigError> {
        let mut config = Config::default();
        for dir in ancestor_dirs_from_repo_root(project_dir) {
            let layer = load_layer(&dir.join("zhao.yml"))?;
            if let Some(layer) = layer {
                config.dbt_command = layer.dbt_command.or(config.dbt_command);
                config.dbt_args = layer.dbt_args.or(config.dbt_args);
                config.max_window_expansion_days = layer
                    .max_window_expansion_days
                    .or(config.max_window_expansion_days);
            }
        }
        Ok(config)
    }
}

fn load_layer(path: &Path) -> Result<Option<DbtPlanLayer>, ConfigError> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let file: ZhaoYml = serde_yaml::from_str(&contents).map_err(|source| ConfigError::Parse {
        path: path.display().to_string(),
        source,
    })?;
    Ok(file.dbt_plan)
}

#[derive(Debug, Default, Deserialize)]
struct ZhaoYml {
    #[serde(rename = "dbt-plan", default)]
    dbt_plan: Option<DbtPlanLayer>,
}

#[derive(Debug, Default, Deserialize)]
struct DbtPlanLayer {
    #[serde(rename = "dbt-command", default)]
    dbt_command: Option<String>,
    #[serde(rename = "dbt-args", default)]
    dbt_args: Option<String>,
    #[serde(rename = "max-window-expansion-days", default)]
    max_window_expansion_days: Option<i64>,
}

/// The directories to read a possible `zhao.yml` from, root-most first:
/// every ancestor of `start` up to and including the nearest directory
/// containing a `.git` entry, or just `start` alone if no `.git` is found
/// before reaching the filesystem root.
fn ancestor_dirs_from_repo_root(start: &Path) -> Vec<PathBuf> {
    let mut chain = vec![start.to_path_buf()];
    let mut current = start.to_path_buf();

    loop {
        if current.join(".git").exists() {
            chain.reverse();
            return chain;
        }
        match current.parent() {
            Some(parent) => {
                current = parent.to_path_buf();
                chain.push(current.clone());
            }
            None => return vec![start.to_path_buf()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_dbt_plan_block() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        std::fs::write(
            dir.path().join("zhao.yml"),
            "dbt-plan:\n  dbt-command: \"uv run dbt\"\n  max-window-expansion-days: 30\n",
        )
        .expect("should write zhao.yml");

        let config = Config::load_for_project(dir.path()).expect("should load");
        assert_eq!(config.dbt_command.as_deref(), Some("uv run dbt"));
        assert_eq!(config.max_window_expansion_days, Some(30));
        assert_eq!(config.dbt_args, None);
    }

    #[test]
    fn no_zhao_yml_at_all_is_not_an_error() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        let config = Config::load_for_project(dir.path()).expect("should load");
        assert_eq!(config.dbt_command, None);
    }

    #[test]
    fn a_project_local_value_overrides_a_root_one() {
        let root = tempfile::tempdir().expect("should create tempdir");
        std::fs::create_dir(root.path().join(".git")).expect("should create .git");
        std::fs::write(
            root.path().join("zhao.yml"),
            "dbt-plan:\n  dbt-command: root-dbt\n",
        )
        .expect("should write root zhao.yml");

        let project_dir = root.path().join("sub");
        std::fs::create_dir(&project_dir).expect("should create subdir");
        std::fs::write(
            project_dir.join("zhao.yml"),
            "dbt-plan:\n  max-window-expansion-days: 10\n",
        )
        .expect("should write project zhao.yml");

        let config = Config::load_for_project(&project_dir).expect("should load");
        assert_eq!(
            config.dbt_command.as_deref(),
            Some("root-dbt"),
            "the project-local file didn't set dbt-command, so the root value should apply"
        );
        assert_eq!(config.max_window_expansion_days, Some(10));
    }
}
