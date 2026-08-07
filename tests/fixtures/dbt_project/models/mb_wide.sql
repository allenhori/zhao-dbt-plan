{{
    config(
        materialized='incremental',
        incremental_strategy='microbatch',
        event_time='order_date',
        batch_size='day',
        begin='2026-08-01',
        unique_key='order_id',
        tags=['microbatch_demo'],
        meta={'zhao': {'lookback': 80, 'lookahead': 5}}
    )
}}

-- Deliberately exceeds max-window-expansion-days (default 90) once
-- cascaded from mb_rolling_14d's own already-expanded window.
select * from {{ ref('mb_rolling_14d') }}
