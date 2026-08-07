{{
    config(
        materialized='incremental',
        incremental_strategy='microbatch',
        event_time='order_date',
        batch_size='day',
        begin='2026-08-01',
        unique_key='order_id',
        tags=['microbatch_demo']
    )
}}

-- No config.meta.zhao at all, deliberately -- an Entry Node with zero
-- window expansion. No seed/source needed: a literal row is enough to
-- give dbt something real to compile and (if ever run outside these
-- tests) execute against.
select 1 as order_id, current_date as order_date
