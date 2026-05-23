# Maps: SC2 (edge-colored CTE DAG — a distinct structural sub-behavior)
Feature: CTE dependency DAG renders with edge-colored edges
  As a dbt analytics engineer
  I want CTE relationships colored by edge type
  So that I can see the model's structural dependencies at a glance

  Scenario Outline: Edge-type edges are color-classified
    Given a model whose two CTEs are connected by a "<edge>" relationship
    When the CTE dependency diagram for that model is rendered
    Then the edge between those CTEs carries the "<edge>" color class
    And the legend maps that color to "<edge>"

    Examples:
      | edge          |
      | from          |
      | inner         |
      | left          |
      | right         |
      | full          |
      | cross         |
      | union_all     |
      | union_distinct|

  Scenario: The legend is always present when a DAG is rendered
    Given a model with at least one CTE-to-CTE dependency
    When the CTE dependency diagram for that model is rendered
    Then an edge-type color legend is visible
    And the legend palette is colorblind-safe (not red/green alone)
