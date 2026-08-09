{#
  zhao_dbt_plan_macros -- wref() / zhao_window_start() / zhao_window_end()

  Full design rationale lives in README.md. Short version: dbt's own
  microbatch ref() filtering is hardcoded (not macro-overridable -- see
  README for why), so a real ref() to an upstream model still gets dbt's
  default single-batch window, silently under-reading it when the
  *current* (downstream) model needs a wider read. These macros read
  the SAME meta.zhao block zhao-dbt-plan's own planner reads -- never a
  second place to maintain the numbers.

  NAMING: this package's own arguments are `expand_back`/`expand_forward`
  -- deliberately NOT `lookback`/`lookahead`, to avoid confusion with
  dbt's own unrelated native `lookback` model config (which means "re-
  process N past batches," a completely different concept). Internally
  these map directly to meta.zhao's `lookback`/`lookahead` keys, which
  are UNCHANGED and NOT renamed -- that vocabulary is already
  established and documented within zhao-dbt-plan itself, where the
  namespacing (`meta.zhao.lookback`) already disambiguates it from dbt's
  own config. The rename only applies here, at the macro call site,
  which is the one place a bare, unnamespaced `lookback=N` would
  actually risk being confused with dbt's own `config(lookback=N)`
  sitting on the very same model.

  IMPORTANT: meta.zhao lives on the DOWNSTREAM model doing the reading,
  not on the upstream being read -- e.g. `mb_orders_rolling_7d` (which
  calls `wref('mb_orders_daily')`) carries the meta.zhao block on
  *itself*, describing how far back/forward *it* needs to read from its
  upstreams. `lookback_overrides`/`lookahead_overrides` are keyed by the
  upstream's bare name, for per-upstream overrides of the model's own
  default lookback/lookahead. This exactly matches zhao-dbt-plan's own
  planner semantics (see zhao-dbt-plan's src/manifest.rs) -- confirmed
  against the real fixture project during testing, after an earlier
  version of this file had the lookup direction backwards.

  v1 scope: only `lookback_unit`/`lookahead_unit: day` and `week` are
  supported. `month`/`year` raise a clear compile error rather than
  silently producing an approximate (and possibly wrong) result --
  calendar month/year arithmetic is warehouse-dialect-dependent and out
  of scope for this first version.

  `execute` guards throughout: `graph` (and everything derived from it)
  is only fully populated once `execute` is true -- dbt's real
  compile/run pass. During the earlier parse-time pass, where dbt
  statically discovers ref()/source()/config() calls to build the
  dependency graph in the first place, `graph` doesn't have real
  contents yet, and this package's real logic can't run. Parse-time
  doesn't need a correct return value, only one that renders without
  crashing -- every macro below that ultimately depends on `graph`
  short-circuits to a harmless placeholder in that case, confirmed via
  a real dbt-core run against this exact failure mode.

  Usage note: call these with the package namespace prefix --
  `{{ zhao_dbt_plan_macros.wref(...) }}`, not bare `{{ wref(...) }}`.
  Confirmed via testing that bare calls don't resolve in dbt-core 1.10.
#}

{#- Finds the graph node for a bare model name -- used only to look up
    an UPSTREAM model's own `event_time` column (which genuinely does
    live on that model, unlike meta.zhao). Errors loudly if the name
    doesn't resolve to anything -- a typo here should never be mistaken
    for "no event_time configured". Only call this when `execute` is
    true (see module doc comment). -#}
{% macro _zhao_find_node(model_name) %}
  {%- set found = namespace(node=none) -%}
  {%- for node in graph.nodes.values() -%}
    {%- if node.name == model_name and found.node is none -%}
      {%- set found.node = node -%}
    {%- endif -%}
  {%- endfor -%}
  {%- if found.node is none -%}
    {{ exceptions.raise_compiler_error(
      "zhao_dbt_plan_macros: no model named '" ~ model_name ~ "' found in the graph"
    ) }}
  {%- endif -%}
  {{ return(found.node) }}
{% endmacro %}

{#- The CURRENT (downstream, being-compiled) model's own meta.zhao
    dict, or none if it doesn't have one, or none unconditionally during
    dbt's parse-time pass (see module doc comment). Uses the built-in
    `model` context var directly -- no graph traversal needed, since
    it's always describing the model currently being compiled. -#}
{% macro _zhao_current_meta() %}
  {%- if not execute -%}
    {{ return(none) }}
  {%- endif -%}
  {{ return((model.config.get('meta', {}) or {}).get('zhao')) }}
{% endmacro %}

{#- Maps a public argument name to meta.zhao's internal key name --
    only used for clear error/warning messages, so they reference what
    the caller actually typed rather than the internal meta.zhao key. -#}
{%- macro _zhao_arg_name(direction) -%}
  {{ return('expand_back' if direction == 'lookback' else 'expand_forward') }}
{%- endmacro -%}

{#- Resolves the effective {amount, unit} for one direction
    ('lookback' or 'lookahead', meta.zhao's own internal key names --
    see the module doc comment on why these differ from this package's
    public expand_back/expand_forward argument names) of a read from
    `upstream_name`, applying the validated-optional-args fallback
    chain: explicit args must match the effective meta.zhao value
    (default, or the per-upstream override keyed by `upstream_name`)
    when both are present, may be given without a meta.zhao block at
    all (compiles and runs fine, but warns that the planner won't see
    it), and when no explicit arg is given at all, falls back to
    meta.zhao, and then to none (meaning: no widening, plain dbt
    default behavior). -#}
{% macro _zhao_resolve(upstream_name, direction, explicit_value) %}
  {%- set meta = zhao_dbt_plan_macros._zhao_current_meta() -%}
  {%- set arg_name = zhao_dbt_plan_macros._zhao_arg_name(direction) -%}

  {%- if explicit_value is not none and execute -%}
    {%- if meta is none -%}
      {%- do exceptions.warn(
        "zhao_dbt_plan_macros: " ~ arg_name ~ "=" ~ explicit_value ~ " was passed for a read of '"
        ~ upstream_name ~ "', but the current model has no meta.zhao block. This compiles and runs "
        ~ "fine using the value you gave -- but zhao-dbt-plan's planner won't see it, so its plan "
        ~ "for this model will be inaccurate. Add a meta.zhao config to unlock accurate planning."
      ) -%}
      {{ return({'amount': explicit_value, 'unit': 'day'}) }}
    {%- endif -%}
    {%- set overrides = meta.get(direction ~ '_overrides', {}) or {} -%}
    {%- set configured_value = overrides.get(upstream_name, meta.get(direction)) -%}
    {%- if configured_value != explicit_value -%}
      {{ exceptions.raise_compiler_error(
        "zhao_dbt_plan_macros: " ~ arg_name ~ "=" ~ explicit_value ~ " passed at the call site for a "
        ~ "read of '" ~ upstream_name ~ "' does not match the effective meta.zhao." ~ direction
        ~ " value (" ~ configured_value ~ "). Keep these in sync, or drop the argument to trust "
        ~ "meta.zhao."
      ) }}
    {%- endif -%}
    {{ return({'amount': explicit_value, 'unit': meta.get(direction ~ '_unit', 'day')}) }}
  {%- endif -%}

  {%- if meta is none -%}
    {{ return(none) }}
  {%- endif -%}
  {%- set overrides = meta.get(direction ~ '_overrides', {}) or {} -%}
  {%- set amount = overrides.get(upstream_name, meta.get(direction, 0)) -%}
  {{ return({'amount': amount, 'unit': meta.get(direction ~ '_unit', 'day')}) }}
{% endmacro %}

{%- macro _zhao_days(resolved) -%}
  {%- set unit = resolved['unit'] -%}
  {%- if unit not in ('day', 'week') -%}
    {{ exceptions.raise_compiler_error(
      "zhao_dbt_plan_macros: meta.zhao's lookback_unit/lookahead_unit '" ~ unit
      ~ "' isn't supported yet (v1 only supports day/week) -- see this package's README."
    ) }}
  {%- endif -%}
  {{ return(resolved['amount'] * (7 if unit == 'week' else 1)) }}
{%- endmacro -%}

{#- The widened window start for a read of `upstream_name` from the
    current model's batch. Falls back to the batch's own unmodified
    event_time_start when there's no meta.zhao block and no explicit
    expand_back given (plain dbt default behavior, no widening), and
    during dbt's parse-time pass (see module doc comment). #}
{% macro zhao_window_start(upstream_name, expand_back=none) %}
  {%- if not execute -%}
    {{ return('') }}
  {%- endif -%}
  {%- set resolved = zhao_dbt_plan_macros._zhao_resolve(upstream_name, 'lookback', expand_back) -%}
  {%- set literal_start = "cast('" ~ model.batch.event_time_start ~ "' as " ~ dbt.type_timestamp() ~ ")" -%}
  {%- if resolved is none -%}
    {{ return(literal_start) }}
  {%- endif -%}
  {%- set days = zhao_dbt_plan_macros._zhao_days(resolved) -%}
  {{ return(dbt.dateadd('day', -1 * days, literal_start)) }}
{% endmacro %}

{#- The widened window end for a read of `upstream_name`. Same fallback
    behavior as zhao_window_start. #}
{% macro zhao_window_end(upstream_name, expand_forward=none) %}
  {%- if not execute -%}
    {{ return('') }}
  {%- endif -%}
  {%- set resolved = zhao_dbt_plan_macros._zhao_resolve(upstream_name, 'lookahead', expand_forward) -%}
  {%- set literal_end = "cast('" ~ model.batch.event_time_end ~ "' as " ~ dbt.type_timestamp() ~ ")" -%}
  {%- if resolved is none -%}
    {{ return(literal_end) }}
  {%- endif -%}
  {%- set days = zhao_dbt_plan_macros._zhao_days(resolved) -%}
  {{ return(dbt.dateadd('day', days, literal_end)) }}
{% endmacro %}

{#- A drop-in replacement for `ref(upstream_name)` that automatically
    applies the current model's widened window (from its own meta.zhao
    block, default or per-upstream override) when it has one, and
    behaves exactly like plain ref() when it doesn't -- including during
    dbt's parse-time pass (see module doc comment; parse-time always
    just returns plain ref(), which is always safe to call at parse
    time since that's dbt's own static-dependency-discovery mechanism).
    Returns a derived-table subquery -- alias it yourself at the call
    site, same as any other subquery:

      select * from {{ zhao_dbt_plan_macros.wref('model_a') }} model_a #}
{% macro wref(upstream_name, expand_back=none, expand_forward=none) %}
  {%- if not execute -%}
    {{ return(ref(upstream_name)) }}
  {%- endif -%}
  {%- set meta = zhao_dbt_plan_macros._zhao_current_meta() -%}
  {%- if meta is none and expand_back is none and expand_forward is none -%}
    {{ return(ref(upstream_name)) }}
  {%- endif -%}
  {%- set upstream_node = zhao_dbt_plan_macros._zhao_find_node(upstream_name) -%}
  {%- set event_time_col = upstream_node.config.get('event_time') -%}
  {%- if event_time_col is none -%}
    {{ exceptions.raise_compiler_error(
      "zhao_dbt_plan_macros: the current model has a meta.zhao block covering a read of '"
      ~ upstream_name ~ "', but '" ~ upstream_name ~ "' has no event_time configured -- wref() "
      ~ "needs event_time on the upstream to know which column to filter on."
    ) }}
  {%- endif -%}
  {%- set start = zhao_dbt_plan_macros.zhao_window_start(upstream_name, expand_back) -%}
  {%- set end = zhao_dbt_plan_macros.zhao_window_end(upstream_name, expand_forward) -%}
(
  select * from {{ ref(upstream_name).render() }}
  where {{ event_time_col }} >= {{ start }}
    and {{ event_time_col }} < {{ end }}
)
{% endmacro %}
