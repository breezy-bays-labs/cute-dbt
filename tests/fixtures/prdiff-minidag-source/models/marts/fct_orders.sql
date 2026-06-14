select
  customer_id,
  count(*) as order_count,
  sum(amount) as gross_revenue
from {{ ref('int_order_items') }}
group by 1
