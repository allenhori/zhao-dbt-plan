# Set up zhao-dbt-plan with an AI coding agent

This page is written **for a coding agent** that has been asked to add `zhao-dbt-plan` to a dbt
project that uses microbatch models. It is also readable by a human.

**Do the `zhao-cli` setup first**
([ai-setup.md](https://raw.githubusercontent.com/allenhori/zhao-cli/master/docs/ai-setup.md)).
This guide only covers what `zhao-dbt-plan` adds, and assumes `dbt-command`, `dbt-args`, and
`against` are already right in `zhao.yml`, which both tools read.

**To use it,** tell your agent:

> Read https://raw.githubusercontent.com/allenhori/zhao-dbt-plan/master/docs/ai-setup.md and set
> up zhao-dbt-plan for this repository.

---

## Instructions for the agent

### Ground rules

- **Do not invent flags or config keys.** The source of truth is this repo's
  [README](../README.md) (flag reference and `config.meta.zhao` section) and
  `zhao-dbt-plan --help`.
- **Never guess a model's `lookback` / `lookahead`.** They encode how much history each model
  actually reads, which is domain knowledge. A wrong value silently produces a wrong window and
  therefore wrong data. Derive it from the model's SQL only when it is unambiguous (a literal
  `interval '7 days'` window, say), state your reasoning, and confirm with the user; otherwise
  ask. A missing `meta.zhao` block means "no expansion" and is safe to leave.
- **`zhao-dbt-plan` never runs dbt builds and never will.** It only writes a plan. Do not add a
  step that executes the plan unless the user explicitly asks for that and tells you which
  orchestrator to target.
- **Show a diff and get a yes before writing anything.** No credentials in any file. Do not push
  or change CI settings outside the repo unless asked.
- Tell the user before running an install command.

### Step 1 — Confirm this is the right tool

`zhao-dbt-plan` matters only if the project has **`incremental_strategy='microbatch'`** models
where a downstream model reads a *wider window* of an upstream than that upstream was recomputed
for (rolling 7-day aggregates, trailing averages, and similar). Check:

```bash
grep -rl "microbatch" models/
```

If there are no microbatch models, tell the user it won't help and stop. If there are, but none
read a wider window of their upstreams, say so and let the user decide.

### Step 2 — Install

```bash
curl -fsSL https://raw.githubusercontent.com/allenhori/zhao-dbt-plan/master/scripts/install.sh | sh
zhao-dbt-plan --version
```

- It installs into `~/.zhao/bin`, the same directory as `zhao-cli`, so the addon is also
  reachable as `zhao dbt-plan ...`. If that directory isn't on `PATH`, tell the user rather than
  editing their shell profile.
- Pin a release for reproducible CI with `ZHAO_DBT_PLAN_VERSION=v0.x.y` before the installer.
  `ZHAO_DBT_PLAN_INSTALL_DIR` overrides the location.
- Alternatives: `brew install allenhori/zhao/zhao-dbt-plan`, Scoop, or
  `cargo install zhao-dbt-plan`.
- Check the installer's supported platforms in `scripts/install.sh`; on an unsupported CI
  architecture use `cargo install`.

### Step 3 — Declare windows on the models that need them

Only for models that read a wider window than their immediate upstream was recomputed for, add
the `zhao` meta block to that model's `config()`, as documented in the README:

```sql
{{ config(
    materialized='incremental',
    incremental_strategy='microbatch',
    event_time='order_date',
    batch_size='day',
    meta={'zhao': {'lookback': 3, 'lookahead': 4}}
) }}
```

Units default to days; `lookback_unit` / `lookahead_unit` accept `day`, `week`, `month`, `year`.
Per-upstream differences use `lookback_overrides` / `lookahead_overrides`. Follow the ground rule:
confirm every value with the user.

Optional: `zhao_utils` (a separate dbt package, `wref()`) is what makes dbt's compiled SQL
actually *use* a widened window. Without it the plan is correct but a plain `ref()` still gets
dbt's default single-batch window. Mention it, don't install it unless asked. See
<https://github.com/allenhori/zhao_dbt_utils>.

### Step 4 — Run it locally and read the output

From the dbt project directory, using the selection the team really runs (`tag:daily`, a path,
whatever their job uses; it is passed verbatim to `dbt ls`):

```bash
zhao-dbt-plan --select "<their selector>" \
  --event-time-start <YYYY-MM-DD> --event-time-end <YYYY-MM-DD> --pretty
```

- The plan JSON is written to `<project-dir>/target/zhao/dbt_plan.json` (override with
  `--output-file`). `--pretty` also prints a layered tree; `--html` also writes an interactive
  report under `target/zhao/dbt-plan/`.
- With no dates it **defaults to yesterday** and prints a note to stderr saying so. That is right
  for a daily scheduled job and wrong for a backfill; for a backfill, pass explicit dates.
- If the project's dbt is invoked via a wrapper, it should already come from `zhao.yml`'s
  `dbt-command`; override once with `--dbt-command` only if the environment differs.
- A `state:` selector needs a baseline; it resolves git-natively (merge-base against `against`)
  or takes `--state <dir>` with a compiled manifest.

Show the user the tree and confirm the windows match their expectations for a model they know
well. Look at any warnings, in particular the `max_window_expansion_days` one (default 90,
warn-only; configurable under `dbt-plan:` in `zhao.yml`).

### Step 5 — Decide how the plan gets consumed

The plan is JSON with one entry per model and its computed window. What runs it is the user's
choice (a dbt CLI script, Dagster, Airflow, a Databricks bundle, ...). **Ask which.** Then:

- Run the planner once and read the actual JSON it produced (and the inline schema documentation
  in `src/output.rs`) before writing any consumer, so the consumer matches the real shape.
- For a plain-dbt consumer, the natural form is a small script that reads the plan and issues one
  `dbt build --select <model> --event-time-start ... --event-time-end ...` per model in layer
  order. Write it only if asked, keep the dbt invocation consistent with `dbt-command` /
  `dbt-args`, and never hard-code credentials.
- Leave orchestrator-native integrations (Dagster, Airflow) to the user's existing patterns;
  read how their DAGs are defined first.

### Step 6 — Wire it into the pipeline

It is usually a **scheduled** job, not a pull-request check. The normal shape is: checkout →
install dbt → `dbt deps` → install `zhao-dbt-plan` → run it (the first step of the daily job) →
the consumer from Step 5. The same points as the `zhao-cli` guide apply (reuse the project's own
dbt install step and credentials, match the CI's existing syntax and secret handling, and add
`~/.zhao/bin` to `PATH`). A full-history checkout is only needed if a `state:` selector is used
without `--state`.

### Step 7 — Verify

- Run the scheduled job once by hand (or trigger it) and confirm the plan artifact exists and the
  windows are right for a model the user knows.
- Confirm nothing but the planner ran before the consumer; the planner itself executes no dbt
  builds.

| Symptom | Likely cause |
|---|---|
| `dbt` not found / wrong version | `dbt-command` in `zhao.yml` doesn't match how this environment runs dbt. |
| Every model shows zero expansion | No `meta.zhao` blocks (by design), or they are on the wrong models. |
| Plan windows look a day off | Default-yesterday was used where explicit dates were meant; check the stderr note. |
| Selection is empty | The selector matched nothing under `dbt ls`; run the same `dbt ls --select` by hand. |

### Step 8 — Hand off

Summarise what changed, the exact command that now runs (locally and in CI), and how to adjust
`lookback` / `lookahead`. Do not commit or push unless asked.
