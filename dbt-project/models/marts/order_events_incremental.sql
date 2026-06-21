-- SPIKE (cute-dbt#145 discovery) — incremental model to inspect how fusion
-- serializes `given: - input: this`, config.materialized, and overrides in
-- manifest.json. May become the committed dogfood fixture after shaping.
{{ config(materialized='incremental', unique_key='order_id', incremental_strategy='merge') }}

with orders as (
    select * from {{ ref('stg_orders') }}
)

select
    order_id,
    customer_id,
    order_date,
    status
from orders
{% if is_incremental() %}
-- on incremental runs, only process orders newer than the loaded high-water
-- mark. The mark subquery is now the shared incremental_high_water_mark()
-- macro (cute-dbt live-dogfood PR #440 — macro adoption, body change): its
-- coalesce guards an existing-but-empty target where max() over zero rows is
-- NULL and `order_date > NULL` is NULL (never true), which would silently
-- filter out every row on that run.
where order_date > {{ incremental_high_water_mark('order_date') }}
{% endif %}
