{{ config(materialized='incremental', unique_key='event_id') }}

with base as (
    select event_id, occurred_at, amount from {{ ref('stg_events') }}
)

{% if is_incremental() %}
, recent as (
    select event_id, occurred_at, amount
    from base
    where occurred_at > (select max(occurred_at) from {{ this }})
)
{% endif %}

select event_id, occurred_at, amount from base
