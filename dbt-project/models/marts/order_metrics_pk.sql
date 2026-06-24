-- order_metrics_pk — cute-dbt live-dogfood: the enforcement.constraint-unbacked
-- coverage check (governance-gated). Declares a primary_key constraint on
-- order_id with an ENFORCED contract but NO unique/not_null data test backing
-- it, so duckdb maps the PK -> NotEnforced and the check fires UNCOVERED.
--
-- OPTIONAL + LAST: enforced:true forces a data_type on every column. If the
-- fusion duckdb :memory: compile rejects it, this model + its YAML entry are
-- dropped (4/5 coverage checks still fire) — see the PR body's halo/contract
-- outcome note.
select
    order_id,
    status,
    amount
from {{ ref('orders') }}
