{% macro mask_pii(col) %}
  case when is_authorized() then {{ col }} else '***' end
{% endmacro %}
