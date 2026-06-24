{# cute-dbt live-dogfood — the PRIMARY changed macro for the report's
   Macros lens. Called directly by stg_customers + customers (the macro
   users), and it calls is_authorized() so the macro->macro depends_on edge
   (mask_pii -> is_authorized) is UNCONDITIONAL (not behind a Jinja branch
   fusion skips at compile) — the transitive blast-radius edge the lens
   walks. The lens lists this macro + its impacted-model directory (its
   callers + their ref()-downstream). #}
{% macro mask_pii(col) %}case when {{ is_authorized() }} then {{ col }} else '***' end{% endmacro %}
