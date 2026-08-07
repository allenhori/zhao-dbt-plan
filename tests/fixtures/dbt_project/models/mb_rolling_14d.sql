{{
    config(
        materialized='incremental',
        incremental_strategy='microbatch',
        event_time='order_date',
        batch_size='day',
        begin='2026-08-01',
        unique_key='order_id',
        tags=['microbatch_demo'],
        meta={'zhao': {'lookback_days': 2, 'lookahead_days': 1}}
    )
}}

select * from {{ ref('mb_rolling_7d') }}
