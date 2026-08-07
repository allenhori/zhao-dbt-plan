//! End-to-end tests against a real, self-contained dbt project
//! (`tests/fixtures/dbt_project/`) and a real `dbt` install -- not a
//! hand-crafted manifest fixture. This is the concrete "does the plan
//! match what we actually want, including --select/--exclude passthrough"
//! check the addon exists to satisfy.
//!
//! Every expected date below was independently cross-checked against
//! Python's `datetime` (see `src/plan.rs`'s own unit tests for the
//! from-scratch derivation of the same numbers), not just accepted from
//! a first run of this binary.
//!
//! Requires a real `dbt` (or dbt Fusion) on `PATH`, or
//! `ZHAO_DBT_PLAN_TEST_DBT_COMMAND` pointing at one -- tests skip
//! (rather than fail) if neither is available, since not every dev
//! machine running `cargo test` has dbt installed. CI always has it (see
//! `.github/workflows/ci.yml`).

use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn fixture_project_dir() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dbt_project"
    ))
    .to_path_buf()
}

/// Copies the fixture project into a fresh tempdir and returns it.
/// Necessary because `cargo test` runs tests in parallel by default, and
/// each invocation of the built binary writes `target/manifest.json`
/// (and DuckDB writes `fixture.duckdb`) into the project directory --
/// running every test directly against the single shared
/// `tests/fixtures/dbt_project/` would race different tests' `dbt parse`/
/// `dbt ls` invocations against the same files.
fn isolated_fixture_copy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("should create tempdir");
    copy_dir_recursive(&fixture_project_dir(), dir.path());
    dir
}

/// dbt-generated artifacts that must never be copied along with the
/// fixture's own source files -- if a developer ran `dbt` directly
/// against `tests/fixtures/dbt_project/` and forgot to clean up
/// afterward, copying `target/`'s stale `partial_parse.msgpack` alongside
/// otherwise-fresh source files confused dbt's partial-parse fast path
/// into producing a corrupt `manifest.json` (two runs' output
/// concatenated) -- caught while writing this suite, not hypothetical.
const NEVER_COPY: &[&str] = &["target", "logs", ".user.yml"];

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("should create dest dir");
    for entry in std::fs::read_dir(src).expect("should read source dir") {
        let entry = entry.expect("should read dir entry");
        let path = entry.path();
        let file_name = entry.file_name();
        if NEVER_COPY.contains(&file_name.to_string_lossy().as_ref())
            || file_name.to_string_lossy().ends_with(".duckdb")
        {
            continue;
        }
        let dest_path = dst.join(&file_name);
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path);
        } else {
            std::fs::copy(&path, &dest_path).expect("should copy file");
        }
    }
}

/// The `--dbt-command` to test against -- `ZHAO_DBT_PLAN_TEST_DBT_COMMAND`
/// if set (e.g. a specific venv's `dbt`, or dbt Fusion's binary), else
/// plain `"dbt"` on `PATH`.
fn dbt_command() -> String {
    std::env::var("ZHAO_DBT_PLAN_TEST_DBT_COMMAND").unwrap_or_else(|_| "dbt".to_string())
}

/// `None` if `dbt_command()` isn't actually runnable -- tests call this
/// and skip (not fail) when it returns `None`, since not every dev
/// machine has dbt installed locally.
fn skip_if_dbt_unavailable() -> bool {
    let command = dbt_command();
    let Ok(mut parts) = shell_words::split(&command) else {
        return true;
    };
    if parts.is_empty() {
        return true;
    }
    let program = parts.remove(0);
    let available = std::process::Command::new(&program)
        .args(&parts)
        .arg("--version")
        .output()
        .map(|o| o.status.success() || !o.stdout.is_empty())
        .unwrap_or(false);
    if !available {
        eprintln!(
            "skipping: {command:?} isn't runnable (set ZHAO_DBT_PLAN_TEST_DBT_COMMAND, or \
             install dbt, to run this test)"
        );
    }
    !available
}

/// Runs the built binary against an isolated copy of the fixture
/// project, returning the parsed plan JSON.
fn run_plan(select: &str, exclude: Option<&str>) -> serde_json::Value {
    let project = isolated_fixture_copy();
    let output_path = project.path().join("dbt_plan.json");

    let mut cmd = Command::cargo_bin("zhao-dbt-plan").expect("binary should build");
    cmd.arg("--project-dir")
        .arg(project.path())
        .arg("--select")
        .arg(select)
        .arg("--event-time-start")
        .arg("2026-07-01")
        .arg("--event-time-end")
        .arg("2026-07-01")
        .arg("--dbt-command")
        .arg(dbt_command())
        .arg("--dbt-args")
        .arg("--target duckdb --profiles-dir .")
        .arg("--output-file")
        .arg(&output_path);
    if let Some(exclude) = exclude {
        cmd.arg("--exclude").arg(exclude);
    }
    cmd.assert().success();

    let contents = std::fs::read_to_string(&output_path).expect("should read plan output");
    serde_json::from_str(&contents).expect("plan output should be valid JSON")
}

fn model_names(plan: &serde_json::Value) -> Vec<String> {
    plan["models"]
        .as_array()
        .expect("models should be an array")
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn select_only_includes_the_selected_subgraph_not_the_whole_project() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let plan = run_plan("tag:microbatch_demo", None);
    let names = model_names(&plan);
    assert_eq!(names.len(), 5, "{names:?}");
    assert!(
        !names.contains(&"unrelated_model".to_string()),
        "unrelated_model has no tag:microbatch_demo and must not appear: {names:?}"
    );
}

#[test]
fn exclude_removes_a_model_from_an_otherwise_matching_selection() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let plan = run_plan("tag:microbatch_demo", Some("mb_wide"));
    let names = model_names(&plan);
    assert_eq!(names.len(), 4, "{names:?}");
    assert!(!names.contains(&"mb_wide".to_string()), "{names:?}");
    // Excluding mb_wide also removes its ceiling-breach warning.
    assert!(plan["metadata"]["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn produces_the_expected_cascading_plan() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let plan = run_plan("tag:microbatch_demo", None);
    let by_name: std::collections::HashMap<&str, &serde_json::Value> = plan["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["name"].as_str().unwrap(), m))
        .collect();

    // mb_daily: Entry Node (its own dependency, an inline literal SELECT,
    // isn't a selected model at all), no config.meta.zhao -> zero
    // expansion.
    assert_eq!(by_name["mb_daily"]["event_time_start"], "2026-07-01");
    assert_eq!(by_name["mb_daily"]["event_time_end"], "2026-07-01");
    assert_eq!(by_name["mb_daily"]["depends_on"], serde_json::json!([]));

    // mb_rolling_7d: lookback=3, lookahead=4 over mb_daily.
    assert_eq!(by_name["mb_rolling_7d"]["event_time_start"], "2026-06-28");
    assert_eq!(by_name["mb_rolling_7d"]["event_time_end"], "2026-07-05");

    // mb_rolling_14d: lookback=2, lookahead=1 over mb_rolling_7d's
    // already-expanded window.
    assert_eq!(by_name["mb_rolling_14d"]["event_time_start"], "2026-06-26");
    assert_eq!(by_name["mb_rolling_14d"]["event_time_end"], "2026-07-06");

    // mb_summary: multi-upstream bounding-box union of mb_rolling_7d and
    // mb_rolling_14d, each further expanded by mb_summary's own
    // lookback=1/lookahead=1.
    assert_eq!(by_name["mb_summary"]["event_time_start"], "2026-06-25");
    assert_eq!(by_name["mb_summary"]["event_time_end"], "2026-07-07");
    let summary_deps: std::collections::HashSet<&str> = by_name["mb_summary"]["depends_on"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        summary_deps,
        std::collections::HashSet::from(["mb_rolling_7d", "mb_rolling_14d"])
    );

    // mb_wide: lookback=80, lookahead=5 over mb_rolling_14d -- exceeds
    // the default max-window-expansion-days (90).
    assert_eq!(by_name["mb_wide"]["event_time_start"], "2026-04-07");
    assert_eq!(by_name["mb_wide"]["event_time_end"], "2026-07-11");

    let warnings = plan["metadata"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["model"], "mb_wide");
    assert!(warnings[0]["message"].as_str().unwrap().contains("96 days"));
}

#[test]
fn pretty_flag_renders_a_tree_to_stdout_in_addition_to_writing_json() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let project = isolated_fixture_copy();
    let output_path = project.path().join("dbt_plan.json");

    let assert = Command::cargo_bin("zhao-dbt-plan")
        .expect("binary should build")
        .arg("--project-dir")
        .arg(project.path())
        .arg("--select")
        .arg("tag:microbatch_demo")
        .arg("--event-time-start")
        .arg("2026-07-01")
        .arg("--event-time-end")
        .arg("2026-07-01")
        .arg("--dbt-command")
        .arg(dbt_command())
        .arg("--dbt-args")
        .arg("--target duckdb --profiles-dir .")
        .arg("--output-file")
        .arg(&output_path)
        .arg("--pretty")
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("mb_daily"), "{stdout}");
    assert!(stdout.contains("mb_rolling_7d"), "{stdout}");
    assert!(
        output_path.exists(),
        "the JSON file should still be written"
    );
}

/// `--html` writes a timestamped file to `target/zhao/dbt-plan/`, a
/// directory distinct from wherever `--output-file`'s JSON goes -- and
/// leaves the JSON's own path/contents completely alone.
#[test]
fn html_flag_writes_a_report_to_its_own_directory_without_touching_the_json() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let project = isolated_fixture_copy();
    let output_path = project.path().join("dbt_plan.json");

    Command::cargo_bin("zhao-dbt-plan")
        .expect("binary should build")
        .arg("--project-dir")
        .arg(project.path())
        .arg("--select")
        .arg("tag:microbatch_demo")
        .arg("--event-time-start")
        .arg("2026-07-01")
        .arg("--event-time-end")
        .arg("2026-07-01")
        .arg("--dbt-command")
        .arg(dbt_command())
        .arg("--dbt-args")
        .arg("--target duckdb --profiles-dir .")
        .arg("--output-file")
        .arg(&output_path)
        .arg("--html")
        .assert()
        .success();

    assert!(
        output_path.exists(),
        "the JSON file should still be written"
    );

    let html_dir = project.path().join("target").join("zhao").join("dbt-plan");
    let entries: Vec<PathBuf> = std::fs::read_dir(&html_dir)
        .unwrap_or_else(|e| panic!("should read {}: {e}", html_dir.display()))
        .map(|e| e.expect("should read dir entry").path())
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let html_path = &entries[0];
    let file_name = html_path.file_name().unwrap().to_string_lossy();
    assert!(file_name.starts_with("dbt_plan_"), "{file_name}");
    assert!(file_name.ends_with(".html"), "{file_name}");

    let html = std::fs::read_to_string(html_path).expect("should read the html report");
    assert!(html.contains("mb_daily"));
    assert!(html.contains("mb_rolling_7d"));
    assert!(html.contains("2026-06-28"));
}

/// Not passing `--html` at all must never create `target/zhao/dbt-plan/`
/// -- the report is strictly opt-in.
#[test]
fn without_the_html_flag_no_html_report_directory_is_created() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let project = isolated_fixture_copy();
    let output_path = project.path().join("dbt_plan.json");

    Command::cargo_bin("zhao-dbt-plan")
        .expect("binary should build")
        .arg("--project-dir")
        .arg(project.path())
        .arg("--select")
        .arg("tag:microbatch_demo")
        .arg("--event-time-start")
        .arg("2026-07-01")
        .arg("--event-time-end")
        .arg("2026-07-01")
        .arg("--dbt-command")
        .arg(dbt_command())
        .arg("--dbt-args")
        .arg("--target duckdb --profiles-dir .")
        .arg("--output-file")
        .arg(&output_path)
        .assert()
        .success();

    let html_dir = project.path().join("target").join("zhao").join("dbt-plan");
    assert!(
        !html_dir.exists(),
        "no --html report should be written unless --html is passed"
    );
}

/// `--anchor` without both `--event-time-start`/`--event-time-end` fails
/// fast with a clear error -- checked before anything dbt-dependent runs
/// (see `main.rs`), so this needs no real `dbt` install and always runs,
/// unlike most of this file's other tests.
#[test]
fn anchor_without_both_explicit_dates_is_a_clear_error() {
    let empty_dir = tempfile::tempdir().expect("should create tempdir");

    let assert = Command::cargo_bin("zhao-dbt-plan")
        .expect("binary should build")
        .arg("--project-dir")
        .arg(empty_dir.path())
        .arg("--select")
        .arg("mb_orders_rolling_14d")
        .arg("--anchor")
        .arg("mb_orders_rolling_14d")
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("--anchor"), "{stderr}");
    assert!(stderr.contains("--event-time-start"), "{stderr}");
    assert!(stderr.contains("--event-time-end"), "{stderr}");
}

/// Same check, but with only one of the two dates supplied -- still a
/// clear error, not silently falling back to a partial anchor.
#[test]
fn anchor_with_only_one_explicit_date_is_still_a_clear_error() {
    let empty_dir = tempfile::tempdir().expect("should create tempdir");

    let assert = Command::cargo_bin("zhao-dbt-plan")
        .expect("binary should build")
        .arg("--project-dir")
        .arg(empty_dir.path())
        .arg("--select")
        .arg("mb_orders_rolling_14d")
        .arg("--anchor")
        .arg("mb_orders_rolling_14d")
        .arg("--event-time-start")
        .arg("2026-01-01")
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("--anchor"), "{stderr}");
}

/// `--anchor` naming a model outside the resolved `--select` produces a
/// clear error naming both the requested anchor and what was actually
/// selected. Needs a real `dbt ls` to resolve the selection first.
#[test]
fn anchor_naming_a_model_outside_the_selection_is_a_clear_error() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let project = isolated_fixture_copy();
    let output_path = project.path().join("dbt_plan.json");

    let assert = Command::cargo_bin("zhao-dbt-plan")
        .expect("binary should build")
        .arg("--project-dir")
        .arg(project.path())
        .arg("--select")
        .arg("tag:microbatch_demo")
        .arg("--anchor")
        .arg("unrelated_model")
        .arg("--event-time-start")
        .arg("2026-07-01")
        .arg("--event-time-end")
        .arg("2026-07-01")
        .arg("--dbt-command")
        .arg(dbt_command())
        .arg("--dbt-args")
        .arg("--target duckdb --profiles-dir .")
        .arg("--output-file")
        .arg(&output_path)
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("unrelated_model"), "{stderr}");
    // Briefly names what was actually selected.
    assert!(stderr.contains("mb_daily"), "{stderr}");
}

/// The full anchor fix from the issue: `--select '+mb_rolling_14d+'
/// --anchor mb_rolling_14d` must apply the literal window to
/// `mb_rolling_14d` itself (not the topological root, `mb_daily`), cascade
/// forward downstream of it exactly as before, and cascade backward
/// upstream of it via the new mirrored formula.
#[test]
fn anchor_pins_the_literal_window_on_the_named_model_not_the_topological_root() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let project = isolated_fixture_copy();
    let output_path = project.path().join("dbt_plan.json");

    Command::cargo_bin("zhao-dbt-plan")
        .expect("binary should build")
        .arg("--project-dir")
        .arg(project.path())
        .arg("--select")
        .arg("+mb_rolling_14d+")
        .arg("--anchor")
        .arg("mb_rolling_14d")
        .arg("--event-time-start")
        .arg("2026-01-01")
        .arg("--event-time-end")
        .arg("2026-01-31")
        .arg("--dbt-command")
        .arg(dbt_command())
        .arg("--dbt-args")
        .arg("--target duckdb --profiles-dir .")
        .arg("--output-file")
        .arg(&output_path)
        .assert()
        .success();

    let contents = std::fs::read_to_string(&output_path).expect("should read plan output");
    let plan: serde_json::Value = serde_json::from_str(&contents).expect("valid JSON");
    let by_name: std::collections::HashMap<&str, &serde_json::Value> = plan["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["name"].as_str().unwrap(), m))
        .collect();

    assert_eq!(plan["metadata"]["anchor_model"], "mb_rolling_14d");

    // The named anchor gets the literal window, not the topological root.
    assert_eq!(by_name["mb_rolling_14d"]["event_time_start"], "2026-01-01");
    assert_eq!(by_name["mb_rolling_14d"]["event_time_end"], "2026-01-31");

    // Upstream, walked backward: mb_rolling_7d (anchor's own
    // lookback=2/lookahead=1) then mb_daily (mb_rolling_7d's own
    // lookback=3/lookahead=4) -- the exact worked example from the issue.
    assert_eq!(by_name["mb_rolling_7d"]["event_time_start"], "2025-12-30");
    assert_eq!(by_name["mb_rolling_7d"]["event_time_end"], "2026-02-01");
    assert_eq!(by_name["mb_daily"]["event_time_start"], "2025-12-27");
    assert_eq!(by_name["mb_daily"]["event_time_end"], "2026-02-05");

    // Downstream, unchanged forward cascade: mb_summary's own
    // lookback=1/lookahead=1, unioned across both its upstreams (via
    // mb_rolling_7d: [2025-12-29, 2026-02-02]; via mb_rolling_14d itself:
    // [2025-12-31, 2026-02-01]) -- same multi-upstream union rule as
    // always, just with mb_rolling_14d's own window now literal.
    assert_eq!(by_name["mb_summary"]["event_time_start"], "2025-12-29");
    assert_eq!(by_name["mb_summary"]["event_time_end"], "2026-02-02");

    // No default-yesterday note on the --anchor path.
    assert!(plan["metadata"]["anchor_window"]["note"].is_null());
}

/// The default-yesterday note appears unconditionally on stderr, in the
/// JSON's `metadata.anchor_window.note`, and as an HTML banner -- but
/// only on the no-`--anchor`, no-explicit-dates path.
#[test]
fn the_default_yesterday_note_appears_in_stderr_json_and_html_only_on_that_path() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let project = isolated_fixture_copy();
    let output_path = project.path().join("dbt_plan.json");

    let assert = Command::cargo_bin("zhao-dbt-plan")
        .expect("binary should build")
        .arg("--project-dir")
        .arg(project.path())
        .arg("--select")
        .arg("tag:microbatch_demo")
        .arg("--dbt-command")
        .arg(dbt_command())
        .arg("--dbt-args")
        .arg("--target duckdb --profiles-dir .")
        .arg("--output-file")
        .arg(&output_path)
        .arg("--html")
        .assert()
        .success();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("note:"), "{stderr}");
    assert!(stderr.contains("yesterday"), "{stderr}");
    assert!(stderr.contains("--event-time-start"), "{stderr}");

    let contents = std::fs::read_to_string(&output_path).expect("should read plan output");
    let plan: serde_json::Value = serde_json::from_str(&contents).expect("valid JSON");
    assert_eq!(
        plan["metadata"]["anchor_window"]["source"],
        "default_yesterday"
    );
    let note = plan["metadata"]["anchor_window"]["note"]
        .as_str()
        .expect("note should be present in JSON for the default-yesterday path");
    assert!(note.contains("yesterday"), "{note}");

    let html_dir = project.path().join("target").join("zhao").join("dbt-plan");
    let entries: Vec<PathBuf> = std::fs::read_dir(&html_dir)
        .unwrap_or_else(|e| panic!("should read {}: {e}", html_dir.display()))
        .map(|e| e.expect("should read dir entry").path())
        .collect();
    let html = std::fs::read_to_string(&entries[0]).expect("should read the html report");
    assert!(
        html.contains(r#"<div id="default-yesterday-banner">"#),
        "{html}"
    );
    assert!(html.contains("yesterday"), "{html}");
}

/// Pointed at a directory with no `dbt_project.yml` at all, `--project-dir`
/// fails immediately with a clear, actionable error -- not a confusing
/// `dbt`-subprocess-shaped failure several steps later. Doesn't need a
/// real `dbt` install at all (the check happens before anything shells
/// out), so this test always runs, unlike the others in this file.
#[test]
fn a_project_dir_with_no_dbt_project_yml_fails_fast_with_a_clear_error() {
    let empty_dir = tempfile::tempdir().expect("should create tempdir");

    let assert = Command::cargo_bin("zhao-dbt-plan")
        .expect("binary should build")
        .arg("--project-dir")
        .arg(empty_dir.path())
        .arg("--select")
        .arg("tag:whatever")
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("dbt_project.yml"), "{stderr}");
    assert!(
        stderr.contains(&empty_dir.path().display().to_string()),
        "{stderr}"
    );
}

/// dbt-core and dbt Fusion must produce identical plans against the
/// identical fixture project. Runs automatically when
/// `ZHAO_DBT_PLAN_TEST_FUSION_COMMAND` names a real dbt Fusion install
/// (e.g. `~/.local/bin/dbt`, wherever Fusion's own installer put it --
/// distinct from `dbt_command()`/`ZHAO_DBT_PLAN_TEST_DBT_COMMAND`, which
/// should point at dbt-core); skips (not fails) otherwise, since most
/// dev machines and CI runners won't have both engines installed side by
/// side.
#[test]
fn dbt_core_and_dbt_fusion_produce_identical_plans() {
    if skip_if_dbt_unavailable() {
        return;
    }
    let Ok(fusion_command) = std::env::var("ZHAO_DBT_PLAN_TEST_FUSION_COMMAND") else {
        eprintln!(
            "skipping: set ZHAO_DBT_PLAN_TEST_FUSION_COMMAND to a dbt Fusion binary to run this"
        );
        return;
    };

    fn run_plan_with(dbt_command: &str) -> serde_json::Value {
        let project = isolated_fixture_copy();
        let output_path = project.path().join("dbt_plan.json");
        Command::cargo_bin("zhao-dbt-plan")
            .expect("binary should build")
            .arg("--project-dir")
            .arg(project.path())
            .arg("--select")
            .arg("tag:microbatch_demo")
            .arg("--event-time-start")
            .arg("2026-07-01")
            .arg("--event-time-end")
            .arg("2026-07-01")
            .arg("--dbt-command")
            .arg(dbt_command)
            .arg("--dbt-args")
            .arg("--target duckdb --profiles-dir .")
            .arg("--output-file")
            .arg(&output_path)
            .assert()
            .success();
        let contents = std::fs::read_to_string(&output_path).expect("should read plan output");
        serde_json::from_str(&contents).expect("plan output should be valid JSON")
    }

    let mut core_plan = run_plan_with(&dbt_command());
    let mut fusion_plan = run_plan_with(&fusion_command);
    for plan in [&mut core_plan, &mut fusion_plan] {
        let metadata = plan["metadata"].as_object_mut().unwrap();
        metadata.remove("generated_at");
        metadata.remove("manifest_generated_at");
        metadata.remove("manifest_path");
        metadata.remove("dbt_command");
    }
    assert_eq!(core_plan, fusion_plan);
}
