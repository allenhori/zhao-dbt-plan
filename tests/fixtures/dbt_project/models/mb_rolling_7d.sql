{{
    config(
        materialized='incremental',
        incremental_strategy='microbatch',
        event_time='order_date',
        batch_size='day',
        begin='2026-08-01',
        unique_key='order_id',
        tags=['microbatch_demo'],
        meta={'zhao': {'lookback': 3, 'lookahead': 4}}
    )
}}

select * from {{ ref('mb_daily') }}
