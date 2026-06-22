-- SPIKE (cute-dbt#145 discovery) — incremental model to inspect how fusion
-- serializes `given: - input: this`, config.materialized, and overrides in
-- manifest.json. May become the committed dogfood fixture after shaping.
{{ config(materialized='incremental', unique_key='order_id', incremental_strategy='merge') }}

with orders as (
    select
        order_id,
        customer_id,
        order_date,
        status
        -- cute-dbt#464 (Z3): a Shape-A for-loop INSIDE one CTE body — the
        -- loop expands a derived column per name, all nested in the `orders`
        -- CTE projection, so cute-dbt's raw-zone scanner marks it a STRUCTURAL
        -- zone bound to this node (NOT a sibling-CTE-producing Shape-B loop).
        -- NOTE: keep literal Jinja tag delimiters out of these `--` comments —
        -- minijinja scans the raw text for tags before SQL comments are stripped,
        -- so a bare for-tag in prose is parsed as a real (malformed) loop.
        {% for derived in ['order_date'] %}
        , date_trunc('month', {{ derived }}) as {{ derived }}_month
        {% endfor %}
    from {{ ref('stg_orders') }}
)

select
    order_id,
    customer_id,
    order_date,
    status,
    order_date_month
from orders
{% if is_incremental() %}
-- on incremental runs, only process orders newer than the loaded high-water
-- mark. coalesce guards an existing-but-empty target: max() over zero rows is
-- NULL, and `order_date > NULL` is NULL (never true), which would silently
-- filter out every row on that run.
where order_date > (select coalesce(max(order_date), '1900-01-01') from {{ this }})
{% endif %}
