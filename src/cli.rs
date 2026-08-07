//! Command-line argument parsing for `zhao-dbt-plan`. See the spec's §3
//! for the full flag reference and precedence rules.

use std::path::PathBuf;

use clap::Parser;

/// zhao-dbt-plan: a static microbatch cascading time-window planner for
/// dbt. Never executes anything -- see the spec's §7 for why this is a
/// permanent boundary, not a v1 scope cut.
#[derive(Debug, Parser)]
#[command(name = "zhao-dbt-plan", version)]
pub struct Cli {
    /// The dbt selector defining the target subgraph -- forwarded
    /// verbatim to `dbt ls --select` (dbt's own selector grammar; see
    /// `select.rs`'s module doc comment for why this is a passthrough,
    /// not a reimplementation).
    #[arg(long = "select", short = 's')]
    pub select: String,

    /// Forwarded verbatim to `dbt ls --exclude`, same as `--select`.
    #[arg(long = "exclude")]
    pub exclude: Option<String>,

    /// Explicit Anchor window start. Must be passed together with
    /// `--event-time-end`, or not at all (defaults every Entry Node to
    /// yesterday -- see §4).
    #[arg(long = "event-time-start")]
    pub event_time_start: Option<String>,

    /// Explicit Anchor window end. Must be passed together with
    /// `--event-time-start`, or not at all.
    #[arg(long = "event-time-end")]
    pub event_time_end: Option<String>,

    /// The dbt project directory. Defaults to the current directory.
    #[arg(long = "project-dir")]
    pub project_dir: Option<PathBuf>,

    /// Path to the compiled manifest. Defaults to
    /// `<project-dir>/target/manifest.json`.
    #[arg(long = "manifest")]
    pub manifest: Option<PathBuf>,

    /// Destination for the generated plan JSON. Defaults to
    /// `<project-dir>/target/zhao/dbt_plan.json`.
    #[arg(long = "output-file", short = 'o')]
    pub output_file: Option<PathBuf>,

    /// Also renders an ASCII tree of the plan to the terminal, in
    /// addition to writing the JSON.
    #[arg(long = "pretty")]
    pub pretty: bool,

    /// Executable/prefix used for every internal `dbt` invocation this
    /// addon makes (`dbt parse` for manifest freshness, `dbt ls` for
    /// selection). Precedence: this flag, then `zhao.yml`'s
    /// `dbt-plan.dbt-command`, then `"dbt"`.
    #[arg(long = "dbt-command")]
    pub dbt_command: Option<String>,

    /// Extra arguments (shell-word-style, e.g. `"--target ci"`) appended
    /// to every internal `dbt` invocation this addon makes. Precedence:
    /// this flag, then `zhao.yml`'s `dbt-plan.dbt-args`, then none.
    /// `allow_hyphen_values`: the value itself starts with `--` (e.g.
    /// `--target`), which clap would otherwise mistake for a new flag.
    #[arg(long = "dbt-args", allow_hyphen_values = true)]
    pub dbt_args: Option<String>,
}
