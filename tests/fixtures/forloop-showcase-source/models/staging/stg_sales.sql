select region, amount from {{ source('raw', 'raw_sales') }}
