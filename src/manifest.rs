//! Reads a compiled dbt `manifest.json` -- just the subset this addon
//! actually needs (node/source identity, dependency edges, `event_time`,
//! and the `config.meta.zhao` block), not a full manifest model. Field
//! shapes verified directly against real `dbt parse` output (dbt-core
//! 1.10 and dbt Fusion 2.0.0-preview, both manifest schema v12), not
//! assumed. Selection (`--select`/`--exclude`, which would otherwise
//! need `tags`/`original_file_path` here) is instead resolved via `dbt
//! ls` -- see `select.rs`'s module doc comment -- so this module doesn't
//! carry fields nothing here actually reads.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// Everything that can go wrong reading a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The file couldn't be read at all.
    #[error("{path}: {source}")]
    Io {
        /// The path that failed.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file was read but isn't valid/expected JSON.
    #[error("{path}: {source}")]
    Parse {
        /// The path that failed.
        path: String,
        /// The underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// Where a Node's dbt-microbatch cascading config comes from --
/// `config.meta.zhao` in the compiled manifest, already merged from
/// `dbt_project.yml` defaults and the model's own `{{ config(...) }}`
/// call, the same way `config.materialized`/`config.event_time` are.
///
/// Distinct from dbt's own native `config.lookback` (a different,
/// narrower mechanism for late-arriving *source* data -- see the spec's
/// §9 "Sources" section for why this addon doesn't touch that field at
/// all).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ZhaoMeta {
    /// Default lookback (in days) applied against every selected
    /// upstream, unless overridden per-upstream in `lookback_overrides`.
    #[serde(default)]
    pub lookback_days: i64,
    /// Default lookahead (in days) applied against every selected
    /// upstream, unless overridden per-upstream in `lookahead_overrides`.
    #[serde(default)]
    pub lookahead_days: i64,
    /// Per-upstream lookback override, keyed by the upstream model's bare
    /// name (not its full `unique_id`) -- e.g. `{"model_a": 7}` for "7
    /// days back from Model A specifically, regardless of the default."
    #[serde(default)]
    pub lookback_overrides: HashMap<String, i64>,
    /// Per-upstream lookahead override, same keying as
    /// `lookback_overrides`.
    #[serde(default)]
    pub lookahead_overrides: HashMap<String, i64>,
}

/// A resolved Node or Source from the manifest -- whichever fields matter
/// for anchor identification and cascading window expansion.
/// `depends_on`/`event_time`/`zhao_meta` are always empty/`None` for a
/// Source (dbt's manifest never gives a Source any of these; see
/// [`ResourceType::Source`]).
#[derive(Debug, Clone)]
pub struct Node {
    /// dbt's own fully-qualified id, e.g. `model.my_project.stg_orders`.
    pub unique_id: String,
    /// The bare name, e.g. `stg_orders` -- what `dbt ls`'s resolved
    /// output and a `lookback_overrides`/`lookahead_overrides` key both
    /// refer to (see `select.rs`).
    pub name: String,
    /// Model or Source.
    pub resource_type: ResourceType,
    /// Direct upstream dependencies (model *and* source unique_ids) --
    /// always empty for a Source itself.
    pub depends_on: Vec<String>,
    /// `config.event_time`, if this model declares one. Not itself
    /// consulted by the cascading-window math (§6 of the spec: the
    /// *anchor's* window comes from `--event-time-start`/
    /// `--event-time-end` or the default-yesterday fallback, not from
    /// re-deriving it out of the manifest) -- kept for completeness/
    /// future diagnostics (e.g. warning if a selected model has no
    /// `event_time` at all).
    pub event_time: Option<String>,
    /// `config.meta.zhao`, if declared. `None` (not a zero-valued
    /// `ZhaoMeta`) when absent, so callers can distinguish "no config at
    /// all" (per §6 of the spec, zero expansion, not silently inheriting
    /// some default) from "explicitly configured to zero."
    pub zhao_meta: Option<ZhaoMeta>,
}

/// Whether a [`Node`] came from `manifest.nodes` (only `resource_type ==
/// "model"` entries are kept -- seeds/snapshots/tests are dropped, they're
/// never part of a microbatch cascading chain) or `manifest.sources`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    /// A dbt model.
    Model,
    /// A dbt source.
    Source,
}

/// A parsed manifest: every model and source, keyed by `unique_id`.
#[derive(Debug, Default)]
pub struct Manifest {
    /// Every Node (model or source), keyed by `unique_id`.
    pub nodes: HashMap<String, Node>,
    /// `manifest.metadata.generated_at` -- the timestamp dbt itself
    /// wrote when it compiled this manifest, recorded in the plan's own
    /// metadata block (`manifest_generated_at`) for traceability.
    pub generated_at: Option<String>,
}

impl Manifest {
    /// Reads and parses a manifest from `path`.
    pub fn load(path: &Path) -> Result<Manifest, ManifestError> {
        let contents = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let raw: RawManifest =
            serde_json::from_str(&contents).map_err(|source| ManifestError::Parse {
                path: path.display().to_string(),
                source,
            })?;

        let mut nodes = HashMap::new();
        for (id, raw_node) in raw.nodes {
            if raw_node.resource_type != "model" {
                continue;
            }
            nodes.insert(
                id.clone(),
                Node {
                    unique_id: id,
                    name: raw_node.name,
                    resource_type: ResourceType::Model,
                    depends_on: raw_node.depends_on.nodes,
                    event_time: raw_node.config.event_time,
                    zhao_meta: raw_node.config.meta.zhao,
                },
            );
        }
        for (id, raw_source) in raw.sources {
            nodes.insert(
                id.clone(),
                Node {
                    unique_id: id,
                    name: raw_source.name,
                    resource_type: ResourceType::Source,
                    depends_on: Vec::new(),
                    event_time: None,
                    zhao_meta: None,
                },
            );
        }

        Ok(Manifest {
            nodes,
            generated_at: raw.metadata.and_then(|m| m.generated_at),
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default)]
    nodes: HashMap<String, RawNode>,
    #[serde(default)]
    sources: HashMap<String, RawSource>,
    #[serde(default)]
    metadata: Option<RawMetadata>,
}

#[derive(Debug, Deserialize)]
struct RawMetadata {
    #[serde(default)]
    generated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawNode {
    resource_type: String,
    name: String,
    #[serde(default)]
    depends_on: RawDependsOn,
    #[serde(default)]
    config: RawNodeConfig,
}

#[derive(Debug, Default, Deserialize)]
struct RawDependsOn {
    #[serde(default)]
    nodes: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawNodeConfig {
    #[serde(default)]
    event_time: Option<String>,
    #[serde(default)]
    meta: RawMeta,
}

#[derive(Debug, Default, Deserialize)]
struct RawMeta {
    #[serde(default)]
    zhao: Option<ZhaoMeta>,
}

#[derive(Debug, Deserialize)]
struct RawSource {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_manifest(json: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("should create temp file");
        file.write_all(json.as_bytes())
            .expect("should write manifest");
        file
    }

    #[test]
    fn parses_a_model_with_zhao_meta() {
        let file = write_manifest(
            r#"{
                "nodes": {
                    "model.p.b": {
                        "resource_type": "model",
                        "name": "b",
                        "original_file_path": "models/b.sql",
                        "depends_on": {"nodes": ["model.p.a"]},
                        "config": {
                            "tags": ["daily"],
                            "event_time": "order_date",
                            "meta": {"zhao": {"lookback_days": 3, "lookahead_days": 4}}
                        }
                    }
                },
                "sources": {}
            }"#,
        );
        let manifest = Manifest::load(file.path()).expect("should parse");
        let node = &manifest.nodes["model.p.b"];
        assert_eq!(node.name, "b");
        assert_eq!(node.depends_on, vec!["model.p.a".to_string()]);
        assert_eq!(node.event_time.as_deref(), Some("order_date"));
        assert_eq!(
            node.zhao_meta,
            Some(ZhaoMeta {
                lookback_days: 3,
                lookahead_days: 4,
                lookback_overrides: HashMap::new(),
                lookahead_overrides: HashMap::new(),
            })
        );
    }

    #[test]
    fn a_model_with_no_meta_zhao_block_has_none_not_a_zeroed_struct() {
        let file = write_manifest(
            r#"{
                "nodes": {
                    "model.p.a": {
                        "resource_type": "model",
                        "name": "a",
                        "original_file_path": "models/a.sql",
                        "config": {}
                    }
                },
                "sources": {}
            }"#,
        );
        let manifest = Manifest::load(file.path()).expect("should parse");
        assert_eq!(manifest.nodes["model.p.a"].zhao_meta, None);
    }

    #[test]
    fn non_model_nodes_are_dropped() {
        let file = write_manifest(
            r#"{
                "nodes": {
                    "test.p.not_null_a": {
                        "resource_type": "test",
                        "name": "not_null_a",
                        "original_file_path": "models/a.sql"
                    }
                },
                "sources": {}
            }"#,
        );
        let manifest = Manifest::load(file.path()).expect("should parse");
        assert!(manifest.nodes.is_empty());
    }

    #[test]
    fn a_source_has_no_dependencies_or_zhao_meta() {
        let file = write_manifest(
            r#"{
                "nodes": {},
                "sources": {
                    "source.p.raw.raw_orders": {
                        "name": "raw_orders",
                        "original_file_path": "models/staging/_sources.yml",
                        "tags": []
                    }
                }
            }"#,
        );
        let manifest = Manifest::load(file.path()).expect("should parse");
        let node = &manifest.nodes["source.p.raw.raw_orders"];
        assert_eq!(node.resource_type, ResourceType::Source);
        assert!(node.depends_on.is_empty());
        assert_eq!(node.zhao_meta, None);
    }

    #[test]
    fn a_missing_file_produces_a_clear_error() {
        let err = Manifest::load(Path::new("/nonexistent/manifest.json")).unwrap_err();
        assert!(matches!(err, ManifestError::Io { .. }));
    }

    #[test]
    fn malformed_json_produces_a_clear_error() {
        let file = write_manifest("not json");
        let err = Manifest::load(file.path()).unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }));
    }
}
