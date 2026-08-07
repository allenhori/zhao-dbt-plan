{{
    config(
        materialized='incremental',
        incremental_strategy='microbatch',
        event_time='order_date',
        batch_size='day',
        begin='2026-08-01',
        unique_key='order_id',
        tags=['microbatch_demo'],
        meta={'zhao': {'lookback': 1, 'lookahead': 1}}
    )
}}

-- Two upstreams with different already-expanded windows -- exercises the
-- multi-upstream bounding-box union.
select r7.order_id, r7.order_date
from {{ ref('mb_rolling_7d') }} as r7
inner join {{ ref('mb_rolling_14d') }} as r14 on r7.order_id = r14.order_id
