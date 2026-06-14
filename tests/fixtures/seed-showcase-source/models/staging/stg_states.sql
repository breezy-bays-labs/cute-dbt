with source as (
    select * from {{ ref('raw_state_codes') }}
)
select state_code, state_name, region
from source
