-- payment_audit — cute-dbt live-dogfood: a BRAND-NEW, deliberately
-- ISOLATED mart for the mini-DAG HALO demo (cute-dbt#428).
--
-- It refs ONLY stg_payments, which this PR leaves COMPLETELY unchanged and
-- which feeds no OTHER modified model via a directed path to/from this one
-- (its mart siblings are not reachable from payment_audit). So in the
-- top-of-report mini-DAG, payment_audit is a disconnected modified seed and
-- its single unchanged parent stg_payments renders as the dimmed 1-hop
-- HALO context node. Added in this PR ⇒ also a NEW state chip in the
-- Models lens.
with payments as (
    select * from {{ ref('stg_payments') }}
)

select
    payment_method,
    count(*) as payment_count,
    sum(amount) as total_amount
from payments
group by payment_method
