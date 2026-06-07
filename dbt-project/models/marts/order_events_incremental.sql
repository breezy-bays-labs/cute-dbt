-- SPIKE (cute-dbt#145 discovery) — incremental model to inspect how fusion
-- serializes `given: - input: this`, config.materialized, and overrides in
-- manifest.json. May become the committed dogfood fixture after shaping.
{{ config(materialized='incremental', unique_key='order_id') }}

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
-- on incremental runs, only process orders newer than the loaded high-water mark
where order_date > (select max(order_date) from {{ this }})
{% endif %}
