-- orders_never_refunded — the cute-dbt#196 NOT IN anti-join dogfood.
--
-- The membership anti-join idiom: deliberately keep the orders whose
-- order_id is NOT IN the refunded set. cute-dbt#173 shipped this form
-- as a declared exclusion (silent); cute-dbt#196's correlated-subquery
-- evidence family lifts it — join.anti-join now fires on the
-- not_in[…] construct. Unlike customers_with_no_orders (the UNCOVERED
-- showcase), this model carries a unit test whose givens include a
-- MATCHING pair (order 1 is refunded) with expect proving the matched
-- row is excluded — the live PR preview renders the arm COVERED with
-- attribution.

select
    o.order_id,
    o.customer_id,
    o.order_date,
    o.status
from {{ ref('stg_orders') }} o
where o.order_id not in (
    select r.order_id
    from {{ ref('stg_refunds') }} r
)
