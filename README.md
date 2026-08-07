# zhao-dbt-plan

**A static microbatch cascading time-window planner for dbt.** dbt's `microbatch` incremental
strategy applies one flat `--event-time-start`/`--event-time-end` window across an entire
selection — so when a rolling-window model reads a wider span than its immediate upstream was
recomputed for, dbt has no way to know it needs a wider batch too. `zhao-dbt-plan` reads your
compiled manifest, walks the DAG within whatever you `--select`, and computes the correct,
per-model expanded window — as a plan you review, never a command it runs for you.

```
$ zhao-dbt-plan --select tag:microbatch_demo --event-time-start 2026-07-01 --event-time-end 2026-07-01 --pretty
mb_orders_daily [2026-07-01 .. 2026-07-01]
  mb_orders_rolling_7d [2026-06-28 .. 2026-07-05]
    mb_orders_rolling_14d [2026-06-26 .. 2026-07-06]
      mb_orders_summary [2026-06-25 .. 2026-07-07]
      mb_orders_wide_lookback [2026-04-07 .. 2026-07-11]
warning: mb_orders_wide_lookback: expanded window (96 days) exceeds max_window_expansion_days (90)
```

## Why

If Model A aggregates a 7-day trailing window and Model B reads Model A over a further
`[-3, +4]` window, a backfill to Model A on day *T* silently corrupts Model B's outputs from
*T-4* through *T+3* — and native dbt only ever re-triggers Model B for day *T* itself. Manually
widening the whole selection's window instead wastes compute recomputing everything that wasn't
actually touched. `zhao-dbt-plan` computes the real, minimal per-model window instead — see
[docs/research/zhao-dbt-plan-spec.md](https://github.com/allenhori/zhao/blob/master/docs/research/zhao-dbt-plan-spec.md)
in the `zhao` planning repo for the full spec.

**It never executes anything.** Not `dbt build`, not `dbt run` — plan-only, permanently (see
the spec's §7 for why). You decide how to actually run the plan: raw dbt, Dagster, Airflow, a
Databricks Asset Bundle, whatever you already use.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/allenhori/zhao-dbt-plan/master/scripts/install.sh | sh
```

## Usage

```bash
cd your-dbt-project
zhao-dbt-plan --select tag:daily --event-time-start 2026-07-01 --event-time-end 2026-07-01
```

`--select`/`--exclude` are forwarded verbatim to `dbt ls` — dbt's own selector engine, not a
reimplementation, so anything real `dbt build --select ...` accepts here works identically
(tags, paths, `+` graph operators, intersections, the works).

With no `--event-time-start`/`--event-time-end`, every Entry Node in the selection defaults to
yesterday — meant to be run daily (e.g. as the first step of a cron/CI job), so the dates stay
current.

A model opts into cascading expansion via `config.meta.zhao` in its own `{{ config(...) }}`:

```sql
{{ config(
    materialized='incremental',
    incremental_strategy='microbatch',
    event_time='order_date',
    batch_size='day',
    meta={'zhao': {'lookback_days': 3, 'lookahead_days': 4}}
) }}
```

No `config.meta.zhao` block at all means zero expansion for that model — deliberately, so a
forgotten declaration is visibly a no-op in the plan, not a silently inherited default.

Full flag reference, plan JSON schema, and the config-scope/edge-override rules:
[docs/research/zhao-dbt-plan-spec.md](https://github.com/allenhori/zhao/blob/master/docs/research/zhao-dbt-plan-spec.md).

## Compatibility

Tested against both dbt-core (1.10+) and dbt Fusion (2.0 preview) — both produce the same
`manifest.json` schema (`dbt_schema_version: manifest/v12.json`) this reads, and both are
verified to produce byte-identical plans from the identical project (see
`tests/end_to_end.rs`'s `dbt_core_and_dbt_fusion_produce_identical_plans`).

## Building a zhao-cli Addon

`zhao-dbt-plan` is zhao's first Addon — a standalone binary, discovered by `zhao-cli` on `PATH`
by naming convention (`zhao dbt-plan` finds and invokes `zhao-dbt-plan`), communicating purely
through files (reads `target/zhao/full_lineage.json`-style artifacts and its own compiled
manifest; writes its own plan to a fixed path). No technical dependency on `zhao-cli` either —
this binary runs standalone, same as above, with zero `zhao-cli` install required. See
[ADR 0010](https://github.com/allenhori/zhao/blob/master/docs/adr/0010-addon-interface-is-subprocess-plus-file-contract.md)
for why, and `zhao-cli`'s own `examples/hello-zhao-addon/` for a minimal reference
implementation of the same contract, if you want to build your own.

## License

AGPLv3 — see [LICENSE](LICENSE). Contributions require a signed CLA (see
[CONTRIBUTING.md](CONTRIBUTING.md)) so this project can keep offering a commercial license
alongside the open one.

## Status

Early. `zhao-core`/`zhao-cli` (Apache 2.0) are the format-agnostic engine and CLI this addon
extends — see [github.com/allenhori/zhao-cli](https://github.com/allenhori/zhao-cli).
