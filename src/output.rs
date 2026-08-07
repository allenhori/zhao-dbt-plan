//! Serializes a [`crate::plan::Plan`] to the JSON schema in §8 of the
//! spec, plus the `--pretty` ASCII tree render.

use std::path::Path;

use serde::Serialize;

use crate::plan::{AnchorSource, Plan};

/// The pieces of the metadata block that come from outside [`Plan`]
/// itself (the CLI invocation, the manifest file) -- kept separate from
/// [`Plan`] so `plan.rs`'s core algorithm doesn't need to know about
/// argument parsing or file paths.
pub struct Metadata {
    /// The raw `--select` string, recorded verbatim.
    pub anchor_selection: String,
    /// The manifest path that was read.
    pub manifest_path: String,
    /// The manifest's own `metadata.generated_at`, if present.
    pub manifest_generated_at: Option<String>,
    /// The `dbt-command` that would be used for a manifest refresh.
    pub dbt_command: String,
    /// The effective `max-window-expansion-days` ceiling.
    pub max_window_expansion_days: i64,
}

#[derive(Serialize)]
struct PlanDocument {
    metadata: MetadataDocument,
    models: Vec<ModelDocument>,
}

#[derive(Serialize)]
struct MetadataDocument {
    generated_at: String,
    anchor_selection: String,
    anchor_window: AnchorWindowDocument,
    manifest_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_generated_at: Option<String>,
    dbt_command: String,
    max_window_expansion_days: i64,
    warnings: Vec<WarningDocument>,
}

#[derive(Serialize)]
struct AnchorWindowDocument {
    event_time_start: String,
    event_time_end: String,
    source: &'static str,
}

#[derive(Serialize)]
struct WarningDocument {
    model: String,
    message: String,
}

#[derive(Serialize)]
struct ModelDocument {
    name: String,
    event_time_start: String,
    event_time_end: String,
    lookback: i64,
    lookback_unit: crate::date::TimeUnit,
    lookahead: i64,
    lookahead_unit: crate::date::TimeUnit,
    depends_on: Vec<String>,
    layer: usize,
}

/// Renders `built_plan` into the final plan-JSON document (pretty-printed
/// string, ready to write to disk).
pub fn render(built_plan: &Plan, metadata: &Metadata) -> String {
    let doc = PlanDocument {
        metadata: MetadataDocument {
            generated_at: now_rfc3339(),
            anchor_selection: metadata.anchor_selection.clone(),
            anchor_window: AnchorWindowDocument {
                event_time_start: built_plan.anchor_window.start.to_string(),
                event_time_end: built_plan.anchor_window.end.to_string(),
                source: match built_plan.anchor_source {
                    AnchorSource::Explicit => "explicit",
                    AnchorSource::DefaultYesterday => "default_yesterday",
                },
            },
            manifest_path: metadata.manifest_path.clone(),
            manifest_generated_at: metadata.manifest_generated_at.clone(),
            dbt_command: metadata.dbt_command.clone(),
            max_window_expansion_days: metadata.max_window_expansion_days,
            warnings: built_plan
                .warnings
                .iter()
                .map(|w| WarningDocument {
                    model: w.model.clone(),
                    message: w.message.clone(),
                })
                .collect(),
        },
        models: built_plan
            .models
            .iter()
            .map(|m| ModelDocument {
                name: m.name.clone(),
                event_time_start: m.window.start.to_string(),
                event_time_end: m.window.end.to_string(),
                lookback: m.lookback,
                lookback_unit: m.lookback_unit,
                lookahead: m.lookahead,
                lookahead_unit: m.lookahead_unit,
                depends_on: m.depends_on.clone(),
                layer: m.layer,
            })
            .collect(),
    };
    serde_json::to_string_pretty(&doc).expect("plan document is always serializable")
}

/// Writes `contents` to `path` atomically (temp file in the same
/// directory, then renamed into place) -- the same precedent `zhao-cli`'s
/// own `target/zhao/` artifacts already follow, so a failure partway
/// through (disk full, the process killed) never leaves a truncated file
/// overwriting a previously good one.
pub fn write(path: &Path, contents: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{path:?} has no parent directory"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| format!("could not create a temp file in {}: {e}", dir.display()))?;
    std::io::Write::write_all(&mut temp, contents.as_bytes())
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    temp.persist(path)
        .map_err(|e| format!("could not write {}: {}", path.display(), e.error))?;
    Ok(())
}

/// A minimal ASCII tree of the plan, for `--pretty` -- one line per
/// model, indented by its `layer` (see [`crate::plan::PlannedModel::layer`])
/// and labeling it explicitly. `built_plan.models` is already in
/// topological order and already carries `layer` (computed once in
/// `plan::build`), so this is purely a formatting pass now -- no depth
/// math of its own.
pub fn render_tree(built_plan: &Plan) -> String {
    let mut out = String::new();
    for model in &built_plan.models {
        let indent = "  ".repeat(model.layer);
        out.push_str(&format!(
            "{indent}[layer {layer}] {name} [{start} .. {end}]\n",
            layer = model.layer,
            name = model.name,
            start = model.window.start,
            end = model.window.end,
        ));
    }
    out
}

/// The current UTC date plus within-day `(hour, minute, second)` -- built
/// by hand from [`crate::date::Date`]'s own day-count arithmetic rather
/// than pulling in a date/time crate for the couple of timestamp shapes
/// this addon ever needs to print (RFC 3339 for the JSON's
/// `generated_at`, and the compact numeric form for `--html`'s
/// filename -- see [`now_rfc3339`] and [`now_compact_utc_timestamp`]).
fn now_utc_components() -> (crate::date::Date, u32, u32, u32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let total_seconds = now.as_secs();
    let days = (total_seconds / 86_400) as i64;
    let seconds_of_day = total_seconds % 86_400;
    let (h, m, s) = (
        (seconds_of_day / 3600) as u32,
        ((seconds_of_day % 3600) / 60) as u32,
        (seconds_of_day % 60) as u32,
    );
    (crate::date::Date::from_days_since_epoch(days), h, m, s)
}

/// The current time as an RFC 3339 timestamp.
fn now_rfc3339() -> String {
    let (date, h, m, s) = now_utc_components();
    format!("{date}T{h:02}:{m:02}:{s:02}Z")
}

/// The current time as a compact, zero-padded `YYYYMMDDHHMMSS` (UTC) --
/// used for `--html`'s output filename (`dbt_plan_<...>.html`), so
/// repeat runs never collide and files sort chronologically by name.
/// `Date`'s own `YYYY-MM-DD` [`Display`](std::fmt::Display) already has
/// every digit this needs; this just strips the dashes rather than
/// re-deriving year/month/day itself.
pub(crate) fn now_compact_utc_timestamp() -> String {
    let (date, h, m, s) = now_utc_components();
    format!("{}{h:02}{m:02}{s:02}", date.to_string().replace('-', ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date::Date;
    use crate::plan::{PlannedModel, Warning, Window};

    fn sample_plan() -> Plan {
        let d = Date::parse("2026-07-01").unwrap();
        Plan {
            anchor_window: Window { start: d, end: d },
            anchor_source: AnchorSource::Explicit,
            models: vec![
                PlannedModel {
                    name: "a".to_string(),
                    window: Window { start: d, end: d },
                    lookback: 0,
                    lookback_unit: crate::date::TimeUnit::Day,
                    lookahead: 0,
                    lookahead_unit: crate::date::TimeUnit::Day,
                    depends_on: Vec::new(),
                    layer: 0,
                },
                PlannedModel {
                    name: "b".to_string(),
                    window: Window {
                        start: d.minus_days(3),
                        end: d.plus_days(4),
                    },
                    lookback: 3,
                    lookback_unit: crate::date::TimeUnit::Day,
                    lookahead: 4,
                    lookahead_unit: crate::date::TimeUnit::Day,
                    depends_on: vec!["a".to_string()],
                    layer: 1,
                },
            ],
            warnings: vec![Warning {
                model: "b".to_string(),
                message: "expanded window (8 days) exceeds max_window_expansion_days (7)"
                    .to_string(),
            }],
        }
    }

    #[test]
    fn renders_valid_json_matching_the_spec_schema() {
        let built = sample_plan();
        let rendered = render(
            &built,
            &Metadata {
                anchor_selection: "tag:daily".to_string(),
                manifest_path: "target/manifest.json".to_string(),
                manifest_generated_at: Some("2026-07-01T00:00:00Z".to_string()),
                dbt_command: "dbt".to_string(),
                max_window_expansion_days: 7,
            },
        );
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");

        assert_eq!(parsed["metadata"]["anchor_selection"], "tag:daily");
        assert_eq!(parsed["metadata"]["anchor_window"]["source"], "explicit");
        assert_eq!(
            parsed["metadata"]["anchor_window"]["event_time_start"],
            "2026-07-01"
        );
        assert_eq!(parsed["metadata"]["max_window_expansion_days"], 7);
        assert_eq!(parsed["metadata"]["warnings"][0]["model"], "b");
        assert_eq!(parsed["models"][1]["name"], "b");
        assert_eq!(parsed["models"][1]["event_time_start"], "2026-06-28");
        assert_eq!(parsed["models"][1]["event_time_end"], "2026-07-05");
        assert_eq!(parsed["models"][1]["depends_on"][0], "a");
        assert_eq!(parsed["models"][0]["layer"], 0);
        assert_eq!(parsed["models"][1]["layer"], 1);
    }

    #[test]
    fn no_command_construction_ever_appears_in_the_output() {
        // Permanent scope decision (spec §7): the plan never embeds a
        // dbt build/run command string.
        let built = sample_plan();
        let rendered = render(
            &built,
            &Metadata {
                anchor_selection: "tag:daily".to_string(),
                manifest_path: "target/manifest.json".to_string(),
                manifest_generated_at: None,
                dbt_command: "dbt".to_string(),
                max_window_expansion_days: 90,
            },
        );
        assert!(!rendered.contains("dbt build"));
        assert!(!rendered.contains("dbt run"));
    }

    #[test]
    fn tree_render_indents_by_dependency_depth() {
        let built = sample_plan();
        let tree = render_tree(&built);
        let lines: Vec<&str> = tree.lines().collect();
        assert!(lines[0].starts_with("[layer 0] a "));
        assert!(lines[1].starts_with("  [layer 1] b "));
    }

    #[test]
    fn write_creates_parent_directories_and_is_readable_back() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        let path = dir.path().join("nested").join("dbt_plan.json");
        write(&path, "{}").expect("should write");
        let contents = std::fs::read_to_string(&path).expect("should read back");
        assert_eq!(contents, "{}");
    }
}
