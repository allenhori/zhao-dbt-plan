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
actually touched. `zhao-dbt-plan` computes the real, minimal per-model window instead.

**It never executes anything.** Not `dbt build`, not `dbt run` — plan-only, permanently. You
decide how to actually run the plan: raw dbt, Dagster, Airflow, a Databricks Asset Bundle,
whatever you already use.

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
    meta={'zhao': {'lookback': 3, 'lookahead': 4}}
) }}
```

`lookback`/`lookahead` default to days; set `lookback_unit`/`lookahead_unit` (`day`, `week`,
`month`, or `year`) if you need a different one — e.g. `{'lookback': 3, 'lookback_unit':
'month'}` for "3 calendar months back." Each direction has its own independent unit.

No `config.meta.zhao` block at all means zero expansion for that model — deliberately, so a
forgotten declaration is visibly a no-op in the plan, not a silently inherited default.

Per-upstream overrides (different lookback depending on *which* upstream) via
`lookback_overrides`/`lookahead_overrides`, keyed by the upstream model's bare name, e.g.
`{'lookback_overrides': {'orders': 5}}` to give just the `orders` upstream a 5-day lookback
while every other upstream keeps the model's own default.

Run `zhao-dbt-plan --help` for the full flag reference; the plan JSON's shape is documented
inline in `src/output.rs`.

## Compatibility

Tested against both dbt-core (1.10+) and dbt Fusion (2.0 preview) — both produce the same
`manifest.json` schema (`dbt_schema_version: manifest/v12.json`) this reads, and both are
verified to produce byte-identical plans from the identical project (see
`tests/end_to_end.rs`'s `dbt_core_and_dbt_fusion_produce_identical_plans`).

## Building a zhao-cli Addon

`zhao-dbt-plan` is zhao's first Addon — a standalone binary with no technical dependency on
`zhao-cli` (this binary runs entirely on its own, as shown above, with zero `zhao-cli` install
required). It's also discoverable as `zhao dbt-plan` once installed alongside `zhao-cli` on the
same `PATH` (this `install.sh` installs to the same `~/.zhao/bin` directory `zhao-cli` uses,
specifically for that) — `zhao-cli` finds any `zhao-<name>` binary on `PATH` and dispatches to it,
forwarding all arguments and the exit code, communicating purely through files. So
`zhao dbt-plan --select ...` and `zhao-dbt-plan --select ...` (standalone, as in every example
above) are equivalent once both are installed. The whole contract is deliberately just a
subprocess plus files — no shared library, no compiled-in knowledge on `zhao-cli`'s side of
any specific Addon, discovery purely by the `zhao-<name>` naming convention on `PATH`. See
`zhao-cli`'s own `examples/hello-zhao-addon/` for a minimal reference implementation of the
same contract, if you want to build your own Addon.

## License

AGPLv3 — see [LICENSE](LICENSE). Contributions require a signed CLA (see
[CONTRIBUTING.md](CONTRIBUTING.md)) so this project can keep offering a commercial license
alongside the open one.

## Status

Early. `zhao-core`/`zhao-cli` (Apache 2.0) are the format-agnostic engine and CLI this addon
extends — see [github.com/allenhori/zhao-cli](https://github.com/allenhori/zhao-cli).
