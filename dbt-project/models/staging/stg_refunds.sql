-- stg_refunds — cute-dbt#172 dogfood material (union.arm-coverage).
--
-- A synthetic "refunds" staging view derived from the payments seed:
-- gift-card payments are treated as refund events (negated amount).
-- It exists so order_metrics can grow a UNION ALL arm that the
-- order_metrics unit test deliberately mocks EMPTY — the live PR-diff
-- preview then surfaces an UNCOVERED union.arm-coverage finding with
-- its given-row recommendation sketch (the catalog C3 worked example,
-- self-dogfooded).

with source as (

    {#-
    Normally we would select from the table here, but we are using seeds to load
    our data in this project
    #}
    select * from {{ ref('raw_payments') }}

),

refunds as (

    select
        id as refund_id,
        order_id,

        -- `amount` is stored in cents; refunds are negated dollars
        -(amount / 100) as amount

    from source
    where payment_method = 'gift_card'

)

select * from refunds
