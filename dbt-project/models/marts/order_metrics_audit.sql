-- order_metrics_audit — cute-dbt live-dogfood: a BRAND-NEW model.
--
-- Added in this PR, so the Models lens renders it with the NEW state chip
-- (cute-dbt#416 ModelState::New on the --pr-diff arm). It also calls the
-- new cents_to_dollars() macro, so it is the second macro user in the
-- Macros lens.
with payments as (
    select * from {{ ref('stg_payments') }}
)

select
    order_id,
    payment_method,
    -- the new cents_to_dollars() macro applied to the already-dollars
    -- amount (a no-op transform — this model exists for the NEW chip +
    -- macro-caller surfaces, not for numeric meaning).
    {{ cents_to_dollars('amount') }} as amount_audit
from payments
