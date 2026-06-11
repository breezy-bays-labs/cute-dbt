-- customers_with_no_orders — the cute-dbt#173 anti-join dogfood.
--
-- The canonical `LEFT JOIN … WHERE <right key> IS NULL` idiom:
-- deliberately keep the customers with NO orders. This model is the
-- live supersedes showcase: the general `join.left-null-propagation`
-- check's own trigger fires here too (a right-side column reaches the
-- projection), but the more specific `join.anti-join` check recognizes
-- the pattern, SILENCES it, and emits the INVERTED recommendation —
-- a given row that DOES match, proving the matched class is excluded.
-- There is deliberately no unit test on this model, so the PR preview
-- shows the UNCOVERED finding with its copy-pasteable given sketch.

with customers as (
    select * from {{ ref('stg_customers') }}
),

orders as (
    select * from {{ ref('stg_orders') }}
),

final as (
    select
        customers.customer_id,
        customers.first_name,
        customers.last_name,
        -- always NULL on the kept rows; projected so the GENERAL
        -- left-null check's trigger provably fires and is visibly
        -- superseded by join.anti-join (cute-dbt#173).
        orders.customer_id as matched_customer_id
    from customers
    left join orders
        on customers.customer_id = orders.customer_id
    where orders.customer_id is null
)

select * from final
