-- order_events_enriched_incremental — cute-dbt live-dogfood (PR #440):
-- the INCREMENTAL-CTE-ZONE showcase. Unlike the project's other two
-- incremental models (which gate only a bare WHERE), this model's
-- is_incremental() block wraps a COUPLE OF CTEs — a high-water-mark
-- CTE and a delta CTE — so the incremental "section" is a real sub-structure,
-- not a one-line filter. A for-loop over a status-weight dict (outside the
-- block) gives the model a Jinja loop too.
--
-- HONEST NOTE: cute-dbt builds the CTE DAG from COMPILED sql, and a fresh
-- `dbt compile` evaluates is_incremental() = false, so high_water + new_events
-- are stripped before the DAG is built — today the DAG shows orders -> events
-- -> (final), and the full incremental structure is visible only in the raw
-- Model-SQL card (Jinja-highlighted) + the amber incremental badge. This model
-- is the motivating fixture for a future "incremental zone in the CTE DAG"
-- affordance (parse raw_code, group the is_incremental() CTEs into a clickable
-- zone that reveals the Jinja wrapper + inner CTE structure).
{{ config(materialized='incremental', unique_key='event_id', incremental_strategy='merge', on_schema_change='append_new_columns') }}

-- a Jinja loop: weight each order status (drives status_weight below).
{% set status_weights = {'completed': 3, 'shipped': 2, 'placed': 1, 'returned': 0} %}

with orders as (
    select * from {{ ref('stg_orders') }}
),

events as (
    select
        order_id   as event_id,
        customer_id,
        order_date as event_at,
        status,
        case status
        {% for status_name, weight in status_weights.items() -%}
            when '{{ status_name }}' then {{ weight }}
        {% endfor -%}
            else 0
        end as status_weight
    from orders
)

{% if is_incremental() %}
,

-- ---- incremental-only zone (a couple of CTEs) ----
-- only on incremental runs: the loaded high-water mark for this table…
high_water as (
    select {{ incremental_high_water_mark('event_at') }} as max_event_at
),

-- …and the delta — events strictly newer than that mark.
new_events as (
    select events.*
    from events
    cross join high_water
    where events.event_at > high_water.max_event_at
)

select * from new_events
{% else %}

-- first (full) build: emit every event.
select * from events
{% endif %}
