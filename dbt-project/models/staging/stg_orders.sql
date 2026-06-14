with source as (

    {#-
    Normally we would select from the table here, but we are using seeds to load
    our data in this project
    #}
    select * from {{ ref('raw_orders') }}

),

renamed as (

    select
        id as order_id,
        user_id as customer_id,
        order_date,
        -- cute-dbt live-dogfood: an additive alias so stg_orders is a
        -- body-modified node in the mini-DAG and a clean PR-comment anchor
        -- line (a RIGHT-side hunk within the diff).
        order_date as ordered_at,
        status

    from source

)

select * from renamed
