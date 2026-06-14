-- cute-dbt#169 dogfood — an incremental model declaring a COMPOSITE
-- unique_key (the list wire form of fusion's DbtUniqueKey) with
-- deliberately NO uniqueness data test backing it: the committed gap the
-- `grain.unique-key-unbacked` check flags as UNCOVERED at the payload
-- level (the report findings surface lands with cute-dbt#170). The
-- not_null test on customer_id (see _incremental__models.yml) is there
-- to prove a non-uniqueness test never satisfies the grain check.
-- cute-dbt#320 dogfood — a no-op source touch so this PR's own sticky
-- preview lights up the LIVE explore rows (the `dbt-project/` touched
-- gate), exercising the new Explore section end to end on its own PR.
{{ config(materialized='incremental', unique_key=['customer_id', 'order_date'], incremental_strategy='delete+insert') }}

with orders as (
    select * from {{ ref('stg_orders') }}
)

select
    customer_id,
    order_date,
    count(*) as orders_placed,
    -- cute-dbt live-dogfood body change: an additive aggregate so this model
    -- is unambiguously body-modified in the PR diff (a clear comment-target
    -- line). Counts the distinct order statuses seen on that customer-day.
    count(distinct status) as status_variety
from orders
{% if is_incremental() %}
-- delete+insert reprocesses whole key partitions: take every order day at
-- or after the loaded high-water mark. coalesce guards the empty target.
where order_date >= (select coalesce(max(order_date), '1900-01-01') from {{ this }})
{% endif %}
group by customer_id, order_date
