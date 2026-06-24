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
        -- cute-dbt live-dogfood: mask the projected PII name columns via the
        -- new mask_pii() macro — a second macro caller (so the Macros lens
        -- impacted-model directory is non-trivial). This is a BODY-ONLY axis
        -- model (no schema.yml edit, no unit test), so the Models lens shows
        -- a lone [Body] chip here.
        {{ mask_pii('customers.first_name') }} as first_name,
        {{ mask_pii('customers.last_name') }} as last_name,
        -- always NULL on the kept rows; projected so the GENERAL
        -- left-null check's trigger provably fires and is visibly
        -- superseded by join.anti-join (cute-dbt#173).
        orders.customer_id as matched_customer_id,
        -- cute-dbt live-dogfood body change: a snapshot timestamp marker so
        -- this model is unambiguously body-modified in the PR diff.
        current_date as snapshot_date
    from customers
    left join orders
        on customers.customer_id = orders.customer_id
    where orders.customer_id is null
)

select * from final
