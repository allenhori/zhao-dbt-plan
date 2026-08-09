# zhao-dbt-plan

[![Crates.io](https://img.shields.io/crates/v/zhao-dbt-plan.svg)](https://crates.io/crates/zhao-dbt-plan)
[![License](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue.svg)](LICENSE)

**A static microbatch cascading time-window planner for dbt.** dbt's `microbatch` incremental
strategy applies one flat `--event-time-start`/`--event-time-end` window across an entire
selection — so when a rolling-window model reads a wider span than its immediate upstream was
recomputed for, dbt has no way to know it needs a wider batch too. `zhao-dbt-plan` reads your
compiled manifest, walks the DAG within whatever you `--select`, and computes the correct,
per-model expanded window — as a plan you review, never a command it runs for you.

```
$ zhao-dbt-plan --select tag:microbatch_demo --event-time-start 2026-07-01 --event-time-end 2026-07-01 --pretty
[layer 0] mb_orders_daily [2026-07-01 .. 2026-07-01]
  [layer 1] mb_orders_rolling_7d [2026-06-28 .. 2026-07-05]
    [layer 2] mb_orders_rolling_14d [2026-06-26 .. 2026-07-06]
      [layer 3] mb_orders_summary [2026-06-25 .. 2026-07-07]
      [layer 3] mb_orders_wide_lookback [2026-04-07 .. 2026-07-11]
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

Rust users: `cargo install zhao-dbt-plan` (via
[crates.io](https://crates.io/crates/zhao-dbt-plan)).

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
current. Whenever this default-yesterday path is taken, a note is printed to stderr
unconditionally (not gated behind `--pretty`) so the assumption is never silent:

```
note: --event-time-start/--event-time-end not supplied, defaulting every Entry Node to yesterday (2026-08-07)
```

The same note is also recorded in the JSON's `metadata.anchor_window.note`, and shown as a
banner in the `--html` report's header — see `--anchor` below for the one case where this
default never applies.

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

Every model in the plan (JSON and `--pretty`) carries a `layer`: its longest-path depth from an
Entry Node within the selected subgraph. An Entry Node is `layer: 0`; every other model is
`1 + max(every upstream's layer)` — a diamond dependency (two upstream paths of different
length) still collapses to one number, the longer path's `+1`. Lets you read the DAG's tier
structure straight off the plan without tracing `depends_on` by hand.

### `--anchor <model>`: pinning the literal window to a specific model

By default, the literal `--event-time-start`/`--event-time-end` window (or the default-yesterday
fallback) applies to every Entry Node in the selection — a selected model with no upstream
dependency *within the selection* — and cascades forward from there. `--select '+model_c+'`
(or `+model_c`, or `model_c+`) does **not**, by itself, pin the literal window on `model_c` —
it still applies to whichever selected model(s) have no upstream dependency within the
selection, which may be several hops upstream of `model_c`.

`--anchor <model>` pins the literal window on that one named model instead, wherever it sits in
the selected subgraph:

```bash
zhao-dbt-plan --select '+mb_orders_rolling_14d+' --anchor mb_orders_rolling_14d \
  --event-time-start 2026-01-01 --event-time-end 2026-01-31
```

- **Downstream of the anchor**: the same forward-cascade formula as always, just starting from
  the anchor's window instead of an Entry Node's.
- **Upstream of the anchor**: walked backward, one edge at a time, applying the *same* formula
  in reverse — at each hop, the upstream model's needed window is the downstream
  (closer-to-anchor) model's own window, padded outward by that downstream model's own
  `(lookback, lookahead)` config. Per-upstream overrides and multi-path bounding-box union both
  apply with the same precedence they already have going forward.
- **Anything in the selection with no path to/from the anchor**: untouched, using the normal
  Entry-Node-based algorithm exactly as if `--anchor` weren't passed.

`--anchor` is a single bare model name — not inferred from `--select`'s `+`/graph-operator shape,
deliberately, so this addon never needs to parse any part of dbt's own selector grammar (the
same reason `--select`/`--exclude` are forwarded verbatim to `dbt ls` rather than reimplemented).
It must name a model actually present in `--select`'s *resolved* selection (post `dbt ls`), or
this fails with a clear error naming both the requested anchor and what was actually selected.

`--event-time-start`/`--event-time-end` become **mandatory** when `--anchor` is used — there is
no yesterday-default on this path. `--anchor` is a deliberate, occasional, investigative
operation (fixing a known bad date range), where silently defaulting to yesterday on a forgotten
date flag would confidently compute a plan for the wrong window with no error at all.

The plan JSON's `metadata` records which model was named, if any: `anchor_model` (omitted
entirely, not `null`, when `--anchor` wasn't given).

### `--html`: an interactive visual report

```bash
zhao-dbt-plan --select tag:daily --event-time-start 2026-07-01 --event-time-end 2026-07-01 --html
```

Opt-in, like `--pretty` — never generated unless `--html` is passed, since most runs (typically
CI, disposable) don't need it. Writes a self-contained, interactive HTML file to
`<project-dir>/target/zhao/dbt-plan/dbt_plan_<YYYYMMDDHHMMSS>.html` (UTC, timestamped so repeat
runs never collide and nothing needs cleaning up) — a directory distinct from wherever the JSON's
`--output-file` goes, and it never changes the JSON's own default path or contents.

**[Open a live demo](https://htmlpreview.github.io/?https://github.com/allenhori/zhao-dbt-plan/blob/master/docs/assets/dbt-plan-demo.html)**
(rendered via [htmlpreview.github.io](https://github.com/htmlpreview/htmlpreview.github.com),
since GitHub shows raw HTML as source rather than rendering it — the
[file itself](docs/assets/dbt-plan-demo.html) is also there to download and open locally).

Each model is a node showing its full name, computed date-range window, and layer, laid out by
layer with its downstream connections drawn so the cascading structure is visible without reading
JSON at all. All interactivity (search, highlighting the upstream/downstream chain on selection,
a resizable side panel) runs client-side in plain JavaScript against an embedded JSON blob — no
network access, no CDN reference anywhere, works fully offline. Model names are never truncated,
regardless of length — node boxes wrap to fit the full name rather than clipping it.

## Making the plan actually apply: `zhao_utils`

`zhao-dbt-plan` computes the correct window — but on its own, it can't make dbt's compiled SQL
actually *use* it. dbt's own microbatch `ref()` filtering can't be overridden by a project macro,
so a plain `ref()` to a widened upstream still gets dbt's default single-batch window, silently.
[`zhao_utils`](https://github.com/allenhori/zhao_dbt_utils) is a small, separately-licensed
(Apache-2.0), separately-repo'd dbt package — `wref()` ("windowed ref"), a drop-in `ref()`
replacement, plus two boundary helpers — that closes that gap, reading the exact same `meta.zhao`
block this planner already does. Completely optional: it's for whoever's starting the
rolling-window pattern fresh, and `zhao-dbt-plan` itself works identically without it. See its
own README for install/usage.

## Flag reference

| Flag | Default | What it does |
|---|---|---|
| `-s, --select <selector>` | — | Required. dbt's own selector syntax, forwarded verbatim to `dbt ls --select` (tags, paths, `+` graph operators, intersections — anything `dbt build --select` accepts). |
| `--exclude <selector>` | — | Forwarded verbatim to `dbt ls --exclude`, same syntax as `--select`. |
| `--event-time-start <date>` | yesterday | Explicit Anchor window start (`YYYY-MM-DD`). Must be passed together with `--event-time-end`, or not at all. |
| `--event-time-end <date>` | yesterday | Explicit Anchor window end. Mandatory (both this and `--event-time-start`) whenever `--anchor` is used — see below. |
| `--anchor <model>` | — | Pins the literal window to this one named model instead of every Entry Node — see [`--anchor`](#--anchor-model-pinning-the-literal-window-to-a-specific-model) above. |
| `--project-dir <dir>` | `.` | The dbt project directory. Everything else (manifest path, `dbt ls`/`dbt parse` invocations) is resolved relative to this. |
| `--manifest <path>` | `<project-dir>/target/manifest.json` | Path to the compiled manifest to read. |
| `-o, --output-file <path>` | `<project-dir>/target/zhao/dbt_plan.json` | Destination for the plan JSON. |
| `--pretty` | — | Also renders an ASCII tree of the plan to the terminal (`[layer N] name [start .. end]`), in addition to writing the JSON. |
| `--html` | — | Also writes the self-contained, interactive HTML report — see [`--html`](#--html-an-interactive-visual-report) above. |
| `--dbt-command <cmd>` | `dbt` | Executable/prefix for every internal `dbt` call this addon makes (`dbt parse` for manifest freshness, `dbt ls` for selection). Shell-word-split, so a multi-word wrapper (`"uv run dbt"`, `"myshell custom-flag"`) works as a genuine prefix. Overrides `zhao.yml`'s top-level `dbt-command` (shared with `zhao-cli`) when given. |
| `--dbt-args "<string>"` | — | Extra arguments appended to every internal `dbt` call, e.g. `"--target ci"`. Overrides `zhao.yml`'s top-level `dbt-args` when given. |
| `--against <ref>` | `master` | The ref `state:`-method selectors are compared against, when `--state` isn't given explicitly. Overrides `zhao.yml`'s top-level `against` (shared with `zhao-cli`'s own git-native Baseline resolution) when given. |
| `--state <dir>` | — | An explicit, already-compiled manifest directory to pass as `dbt ls --state`, for a `state:`-method selector — resolves git-natively (merge-base against `--against`, compiled in a temporary worktree) when omitted and the selector actually needs one. Wins outright over `--against` if given. |
| `-h, --help` | — | Full flag reference, same as this table, printed from the binary itself. |
| `-V, --version` | — | Print version. |

`zhao.yml`'s `dbt-plan:` block also has its own, addon-specific `max-window-expansion-days` key
(default `90`, warn-only — the ceiling `mb_orders_wide_lookback` trips in the example above),
with no CLI flag equivalent; it has no equivalent concept in `zhao-cli` to share a top-level key
with. The plan JSON's shape is documented inline in `src/output.rs`.

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
