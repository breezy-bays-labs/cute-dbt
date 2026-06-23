select event_id, occurred_at, amount from {{ source('raw', 'raw_events') }}
