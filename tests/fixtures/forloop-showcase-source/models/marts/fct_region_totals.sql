{{ config(materialized='table') }}

with base as (
    select region, amount from {{ ref('stg_sales') }}
),

prepped as (
    select region, amount from base
),

{% for region in ['us', 'eu'] %}
{{ region }}_totals as (
    select amount from prepped where region = '{{ region }}'
),
{% endfor %}

combined as (
    select amount from prepped
)

select amount from combined
