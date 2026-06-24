{# cute-dbt live-dogfood — a SECOND root-project macro so the macro-lens
   picker (.macro-select) is non-degenerate. Called by order_metrics_audit
   (a new mart) so it surfaces as an added macro with its own caller. #}
{% macro cents_to_dollars(col, scale=100) %}({{ col }} / {{ scale }}){% endmacro %}
