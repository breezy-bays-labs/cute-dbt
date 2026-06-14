with src as (
  select order_id, customer_id, status, amount
  from {{ source('raw', 'orders') }}
)
select * from src
