{# cute-dbt live-dogfood — a LEAF macro reached by models only TRANSITIVELY
   through mask_pii. The macro-lens blast-radius reverse-walk
   (macro_blast_radius, forward-BFS over macro_refs) must reach this macro
   from a caller model's DIRECT set {mask_pii} — a naive first-order test
   would miss every impacted model, so this is the load-bearing demo of the
   transitive macro-walk (cute-dbt#345 spike correction). #}
{% macro is_authorized() %}{{ var('mask_enabled', true) }}{% endmacro %}
