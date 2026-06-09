-- cute-dbt#155 dogfood: this model's import CTE is named `orders` — the same
-- name as the model itself (the idiomatic jaffle-shop shape). It exercises the
-- DAG node-identity fix: the lineage graph renders `orders` (import) → `final`
-- → `orders.sql` (the model's final select) as three distinct nodes, with no
-- spurious `orders ↔ final` self-cycle and the import node showing its own SQL.
{% set payment_methods = ['credit_card', 'coupon', 'bank_transfer', 'gift_card'] %}

with orders as (

    select * from {{ ref('stg_orders') }}

),

payments as (

    select * from {{ ref('stg_payments') }}

),

order_payments as (

    select
        order_id,

        {% for payment_method in payment_methods -%}
        sum(case when payment_method = '{{ payment_method }}' then amount else 0 end) as {{ payment_method }}_amount,
        {% endfor -%}

        sum(amount) as total_amount

    from payments

    group by order_id

),

final as (

    select
        orders.order_id,
        orders.customer_id,
        orders.order_date,
        orders.status,

        {% for payment_method in payment_methods -%}

        coalesce(order_payments.{{ payment_method }}_amount, 0) as {{ payment_method }}_amount,

        {% endfor -%}

        coalesce(order_payments.total_amount, 0) as amount

    from orders


    left join order_payments
        on orders.order_id = order_payments.order_id

)

select * from final
