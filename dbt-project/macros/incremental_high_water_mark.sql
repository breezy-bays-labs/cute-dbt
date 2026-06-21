{# cute-dbt live-dogfood (PR #440) — shared incremental high-water-mark.
   Encapsulates the `(select coalesce(max(col), floor) from {{ this }})`
   subquery the incremental models repeat. Adopted by
   order_events_incremental, customer_order_days, and
   order_events_enriched_incremental, so all three gain a depends_on.macros
   edge to this macro — fusion collects macro refs at PARSE, so the edge holds
   even where the call sits inside an {% if is_incremental() %} branch the
   default compile evaluates false and strips. The Macros-lens blast radius
   reverse-walks from this macro to its three caller models. #}
{% macro incremental_high_water_mark(column, floor='1900-01-01') -%}
(select coalesce(max({{ column }}), '{{ floor }}') from {{ this }})
{%- endmacro %}
