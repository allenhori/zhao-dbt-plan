# zhao_dbt_plan_macros

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

A tiny dbt package -- three macros, nothing else -- that makes a `ref()`-style call to an
upstream model actually use the widened window a
[`zhao-dbt-plan`](https://github.com/allenhori/zhao-dbt-plan) plan computes, instead of dbt's
own default single-batch filter silently under-reading it. Licensed separately (Apache-2.0)
from `zhao-dbt-plan` itself (AGPLv3) -- see ["Why a separate license"](#why-a-separate-license)
below.

## The problem this solves

`zhao-dbt-plan` computes the *correct* cascading window for every downstream model -- but on its
own, it can't make dbt's compiled SQL actually *use* that window. dbt's microbatch `ref()`
filtering can't be overridden by a project macro (it's resolved outside normal macro dispatch, to
build the dependency graph statically before Jinja even renders -- confirmed against dbt-core's
actual behavior, not assumed), so a plain `{{ ref('model_a') }}` to an upstream still gets dbt's
own narrow, single-batch window -- silently. No error, no warning, just a rolling-window model
quietly computing on too little data.

This package is that missing piece.

## Install

```yaml
# packages.yml
packages:
  - git: "https://github.com/allenhori/zhao-dbt-plan"
    subdirectory: "integrations/dbt_package"
    revision: v0.1.0  # pin to a tag
```

Then `dbt deps`. (Not yet on dbt Hub -- see [Publishing](#publishing) below.)

**Always call these with the package namespace prefix** --
`{{ zhao_dbt_plan_macros.wref(...) }}`, not bare `{{ wref(...) }}`. Confirmed against a real
dbt-core project that bare calls don't resolve; namespaced calls do. This matches how most dbt
packages document themselves (e.g. `{{ dbt_utils.star(...) }}`).

## Naming: `expand_back`/`expand_forward`, not `lookback`/`lookahead`

This package's own arguments are deliberately **not** called `lookback`/`lookahead`, even though
that's what `meta.zhao` itself calls them (unchanged, see below) -- to avoid confusion with dbt's
own unrelated native `lookback` model config, which means something completely different
("reprocess N past batches for late-arriving data," not "widen this read"). A bare
`lookback=N` argument sitting on the same model as dbt's own `config(lookback=N, ...)` would be a
real, easy mix-up. `expand_back`/`expand_forward` map directly to `meta.zhao`'s `lookback`/
`lookahead` keys internally -- `meta.zhao` itself is unchanged and not renamed, since it's already
namespaced (`meta.zhao.lookback`, not bare `lookback`) and already established/documented within
`zhao-dbt-plan` itself.

## Usage

### `wref(upstream_name)` -- a drop-in `ref()` replacement

```sql
select * from {{ zhao_dbt_plan_macros.wref('mb_daily') }} mb_daily
```

If the **current** model (the one calling `wref()`, not `mb_daily`) has a `meta.zhao` block, this
automatically applies its widened window. If it doesn't, this behaves *exactly* like plain
`{{ ref('mb_daily') }}` -- no meta.zhao, no behavior change, completely safe to use everywhere as
a habit. Returns a derived-table subquery, so alias it at the call site like any other subquery.

**Important**: `meta.zhao` lives on the model doing the *reading* (the downstream/current model),
not on the upstream being read -- e.g. if `mb_rolling_7d` calls `wref('mb_daily')`, the
`meta.zhao` block belongs on `mb_rolling_7d` itself, describing how far back/forward *it* needs
to read from its upstreams. Use `lookback_overrides`/`lookahead_overrides` (keyed by the
upstream's bare name) for per-upstream overrides of the model's own default. This matches
`zhao-dbt-plan`'s own planner semantics exactly.

### `zhao_window_start(upstream_name)` / `zhao_window_end(upstream_name)` -- boundary helpers

If you already hand-write a custom `WHERE` clause using dbt's own `.render()` opt-out (the
documented pattern for rolling-window microbatch models), use these instead of hardcoding the
day-count:

```sql
select * from {{ ref('mb_daily').render() }}
where event_date >= {{ zhao_dbt_plan_macros.zhao_window_start('mb_daily') }}
  and event_date <  {{ zhao_dbt_plan_macros.zhao_window_end('mb_daily') }}
```

### Optional explicit arguments -- visible-in-SQL numbers, still one source of truth

All three macros accept optional `expand_back`/`expand_forward` arguments:

```sql
select * from {{ zhao_dbt_plan_macros.wref('mb_daily', expand_back=3, expand_forward=4) }} mb_daily
```

| Argument given? | `meta.zhao` present? | Behavior |
|---|---|---|
| Yes | Yes, and matches | Uses it -- widened window |
| Yes | Yes, but mismatched | **Compile error** -- prevents silent drift between the SQL and the planner's config |
| Yes | No | **Compiles and runs fine**, using the value given -- but **warns** that `zhao-dbt-plan`'s planner won't see it, so its plan for this model will be inaccurate |
| No | Yes | Reads from `meta.zhao` |
| No | No | Falls back to plain dbt default behavior, unwidened |

The "given but no meta.zhao" case is a warning, not a hard error, on purpose -- if you don't care
about `zhao-dbt-plan`'s planner and just want a convenient windowed read, that's a legitimate use
of this package on its own. The warning is there to tell you what you're leaving on the table, not
to block you.

## v1 scope: `day`/`week` only

`meta.zhao`'s `lookback_unit`/`lookahead_unit: month` or `year` raise a clear compile error rather
than silently producing an approximate (and possibly wrong) result. Calendar month/year
arithmetic is warehouse-dialect-dependent, real scope, and deliberately deferred rather than
half-implemented.

## Why a separate license

`zhao-dbt-plan` itself is AGPLv3. This package is Apache-2.0, on purpose. AGPL's copyleft trigger
is about running/modifying *the program* -- but this package's macros get copied directly into
*your* dbt project and compiled alongside your own models, which is a structurally different
situation from running `zhao-dbt-plan` as a separate tool. Apache-2.0 avoids any risk of this
package's license reaching into your own project just because you installed a few macros.

## Tested against

Empirically verified, not assumed:
- **dbt-core 1.10.22 + DuckDB**: full real `dbt build` run, both the drop-in `wref()` path and
  the explicit-args-without-meta.zhao warning path, compiled SQL manually inspected and confirmed
  correct (`where order_date >= (batch_start - 3 days) and order_date < (batch_end + 4 days)`
  for a `lookback: 3, lookahead: 4` config).
- **dbt Fusion 2.0.0-preview.203 + a real Databricks workspace**: compile-time verified --
  correct derived-table structure, correct `expand_back`/`expand_forward` direction, and correct
  per-adapter SQL dialect dispatch (`dbt.dateadd` compiled to Databricks-native
  `timestampadd(day, ...)`, vs. DuckDB's `+ interval` syntax under dbt-core -- confirming
  cross-adapter portability). A full real *run* under Fusion against Databricks was blocked by an
  unrelated, pre-existing dbt Fusion/Databricks microbatch materialization bug (reproduced with
  this package completely uninstalled, on a model that never calls any of these macros -- not
  something this package caused or can fix).

## Publishing

Not yet submitted to [dbt Hub](https://hub.getdbt.com) -- planned as a follow-up once this has
had more real-world use beyond the initial testing. Track via the
[`zhao-dbt-plan` repo](https://github.com/allenhori/zhao-dbt-plan).
