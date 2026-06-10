-- order_metrics — the cute-dbt dogfood "rich" mart.
--
-- This model exists to give cute-dbt's CTE DAG + JoinType legend real,
-- varied material. Its transform CTEs deliberately reference each other
-- through every edge type in cute-dbt's v0.1 EdgeType vocabulary
-- (src/domain/cte.rs): From, Inner, Left, Right, Full, Cross, UnionAll,
-- UnionDistinct — plus a comma-join CTE that exercises the cute-dbt#40
-- "comma cross-join is NOT the simple-FROM import shape" heuristic
-- (which renders as two From edges, not a Cross edge).
--
-- Edges form CTE -> CTE only, so each join below references an
-- earlier-declared CTE on both sides.

with

-- ---- import CTEs (each renders as a `From` edge) ----
orders as (
    select * from {{ ref('stg_orders') }}
),

payments as (
    select * from {{ ref('stg_payments') }}
),

customers as (
    select * from {{ ref('stg_customers') }}
),

-- cute-dbt#172 dogfood: this import feeds the deliberately-unexercised
-- third UNION ALL arm in all_statuses below.
refunds as (
    select * from {{ ref('stg_refunds') }}
),

-- ---- INNER JOIN: orders (From) x payments (Inner) ----
paid_orders as (
    select
        orders.order_id,
        orders.customer_id,
        orders.order_date,
        orders.status,
        payments.amount
    from orders
    inner join payments
        on orders.order_id = payments.order_id
),

-- ---- LEFT JOIN: customers (From) x paid_orders (Left) ----
customer_orders as (
    select
        customers.customer_id,
        customers.first_name,
        customers.last_name,
        paid_orders.order_id,
        paid_orders.order_date,
        paid_orders.status,
        paid_orders.amount
    from customers
    left join paid_orders
        on customers.customer_id = paid_orders.customer_id
),

-- ---- RIGHT JOIN: payments (From) x orders (Right) ----
order_payment_match as (
    select
        orders.order_id,
        orders.status,
        payments.payment_method,
        payments.amount
    from payments
    right join orders
        on payments.order_id = orders.order_id
),

-- ---- FULL OUTER JOIN: customer_orders (From) x order_payment_match (Full) ----
enriched as (
    select
        customer_orders.customer_id,
        customer_orders.first_name,
        customer_orders.last_name,
        coalesce(customer_orders.order_id, order_payment_match.order_id) as order_id,
        customer_orders.order_date,
        coalesce(customer_orders.status, order_payment_match.status) as status,
        order_payment_match.payment_method,
        coalesce(customer_orders.amount, order_payment_match.amount) as amount
    from customer_orders
    full outer join order_payment_match
        on customer_orders.order_id = order_payment_match.order_id
),

-- ---- CROSS JOIN: enriched (From) x grand_total (Cross) ----
-- a single-row constant CTE crossed onto every enriched row.
grand_total as (
    select sum(amount) as all_orders_amount
    from paid_orders
),

enriched_with_share as (
    select
        enriched.customer_id,
        enriched.first_name,
        enriched.last_name,
        enriched.order_id,
        enriched.order_date,
        enriched.status,
        enriched.payment_method,
        enriched.amount,
        grand_total.all_orders_amount
    from enriched
    cross join grand_total
),

-- ---- comma cross-join (cute-dbt#40 heuristic): two `From` edges, NOT Cross ----
-- `from a, b` is a Cartesian product syntactically, but cute-dbt classifies
-- each comma source as a plain `From` edge (only explicit CROSS JOIN ->
-- Cross). Kept deliberately so the report exercises the #40 multi-source
-- shape detection.
status_per_total as (
    select
        enriched_with_share.order_id,
        enriched_with_share.status,
        enriched_with_share.amount,
        enriched_with_share.all_orders_amount,
        grand_total.all_orders_amount as total_check
    from enriched_with_share, grand_total
),

-- ---- UNION ALL: completed_rows (UnionAll) + other_rows (UnionAll) ----
completed_rows as (
    select order_id, status, amount, all_orders_amount
    from status_per_total
    where status = 'completed'
),

other_rows as (
    select order_id, status, amount, all_orders_amount
    from status_per_total
    where status <> 'completed'
),

-- cute-dbt#172 dogfood (union.arm-coverage): the third arm reads
-- `refunds`, which the unit test mocks EMPTY — the live PR-diff preview
-- report carries the UNCOVERED finding + its given-row sketch (the
-- catalog C3 charges/refunds worked example, self-dogfooded). With an
-- empty given the arm contributes zero rows, so the existing expect is
-- unchanged.
all_statuses as (
    select order_id, status, amount, all_orders_amount from completed_rows
    union all
    select order_id, status, amount, all_orders_amount from other_rows
    union all
    select
        order_id,
        'refunded' as status,
        amount,
        cast(null as double) as all_orders_amount
    from refunds
),

-- ---- UNION (distinct): distinct status pairs from two arms ----
status_dim as (
    select status from completed_rows
    union
    select status from other_rows
),

final as (
    select
        all_statuses.order_id,
        all_statuses.status,
        all_statuses.amount,
        all_statuses.all_orders_amount,
        case
            when all_statuses.all_orders_amount > 0
            then round(all_statuses.amount / all_statuses.all_orders_amount, 4)
            else 0
        end as amount_share,
        (select count(*) from status_dim) as distinct_status_count
    from all_statuses
)

select * from final
