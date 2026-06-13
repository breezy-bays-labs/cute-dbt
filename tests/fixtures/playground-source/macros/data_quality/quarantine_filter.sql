{% macro quarantine_filter(enabled=true, field_name='is_dq_valid') %}
    {% if enabled %}
    where {{ field_name }} = true
    {% endif %}
{% endmacro %}
