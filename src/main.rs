//! `zhao-dbt-plan`: a static microbatch cascading time-window planner for
//! dbt. See the crate-level docs in each module for the algorithm; see
//! `README.md` for usage.

mod cli;
mod config;
mod date;
mod detect;
mod git;
mod html;
mod manifest;
mod output;
mod plan;
mod refresh;
mod select;
mod state;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args = <cli::Cli as clap::Parser>::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &cli::Cli) -> Result<(), String> {
    let project_dir = args
        .project_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    // The very first check: is this even a dbt project? Everything below
    // assumes one and fails with a confusing dbt-subprocess-shaped error
    // otherwise -- see detect.rs's module doc comment.
    detect::ensure_dbt_project(&project_dir).map_err(|e| e.to_string())?;

    let config = config::Config::load_for_project(&project_dir).map_err(|e| e.to_string())?;

    let manifest_path = args
        .manifest
        .clone()
        .unwrap_or_else(|| project_dir.join("target").join("manifest.json"));

    let dbt_command = args
        .dbt_command
        .clone()
        .or_else(|| config.dbt_command.clone())
        .unwrap_or_else(|| "dbt".to_string());
    let dbt_args = match &args.dbt_args {
        Some(raw) => shell_words::split(raw).map_err(|e| format!("--dbt-args: {e}"))?,
        None => config
            .dbt_args
            .clone()
            .map(|raw| shell_words::split(&raw))
            .transpose()
            .map_err(|e| format!("zhao.yml dbt-plan.dbt-args: {e}"))?
            .unwrap_or_default(),
    };

    // Manifest freshness (§5): the one narrow, deliberate exception to
    // "never invoke dbt" that predates the --select passthrough decision
    // below -- kept as its own step since a stale manifest is a
    // correctness problem even for the parts of the pipeline that don't
    // touch dbt ls at all (event_time, config.meta.zhao, depends_on).
    refresh::ensure_fresh(&project_dir, &manifest_path, &dbt_command, &dbt_args)?;

    let manifest = manifest::Manifest::load(&manifest_path).map_err(|e| e.to_string())?;

    // `state:`-method resolution (§ "State comparison"): explicit
    // --state wins outright; else a "state:" selector git-natively
    // resolves a merge-base baseline against --against/config/"master";
    // else None (no state: method used at all, nothing to do here).
    let against = args
        .against
        .clone()
        .or_else(|| config.against.clone())
        .unwrap_or_else(|| "master".to_string());
    let state_source = state::resolve(
        &project_dir,
        &args.select,
        args.exclude.as_deref(),
        args.state.as_deref(),
        &against,
        &dbt_command,
        &dbt_args,
    )
    .map_err(|e| e.to_string())?;

    // --select/--exclude resolution (passthrough to `dbt ls` -- see
    // select.rs's module doc comment for why this isn't reimplemented).
    let selected = select::resolve(
        &project_dir,
        &manifest,
        &dbt_command,
        &dbt_args,
        &args.select,
        args.exclude.as_deref(),
        state_source.as_ref().map(state::StateSource::manifest_dir),
    )
    .map_err(|e| e.to_string())?;

    let explicit_window = match (&args.event_time_start, &args.event_time_end) {
        (Some(start), Some(end)) => Some((
            date::Date::parse(start).map_err(|e| e.to_string())?,
            date::Date::parse(end).map_err(|e| e.to_string())?,
        )),
        (None, None) => None,
        _ => {
            return Err(
                "--event-time-start and --event-time-end must be passed together, or not at all"
                    .to_string(),
            );
        }
    };

    let max_window_expansion_days = config.max_window_expansion_days.unwrap_or(90);
    let built_plan = plan::build(
        &manifest,
        &selected,
        explicit_window,
        max_window_expansion_days,
    )
    .map_err(|e| e.to_string())?;

    let output_path = args.output_file.clone().unwrap_or_else(|| {
        project_dir
            .join("target")
            .join("zhao")
            .join("dbt_plan.json")
    });

    let rendered = output::render(
        &built_plan,
        &output::Metadata {
            anchor_selection: args.select.clone(),
            manifest_path: manifest_path.display().to_string(),
            manifest_generated_at: manifest.generated_at.clone(),
            dbt_command: dbt_command.clone(),
            max_window_expansion_days,
        },
    );

    output::write(&output_path, &rendered)?;

    if args.pretty {
        print!("{}", output::render_tree(&built_plan));
    }

    if args.html {
        let html_path = html::default_output_path(&project_dir);
        output::write(&html_path, &html::generate(&built_plan))?;
        println!("wrote {}", html_path.display());
    }

    for warning in &built_plan.warnings {
        eprintln!("warning: {}: {}", warning.model, warning.message);
    }

    Ok(())
}
