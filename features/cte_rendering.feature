# Maps: SC2 (join-type-colored CTE DAG — a distinct structural sub-behavior)
Feature: CTE dependency DAG renders with join-type-colored edges
  As a dbt analytics engineer
  I want CTE relationships colored by join type
  So that I can see the model's join structure at a glance

  Scenario Outline: Join-type edges are color-classified
    Given a model whose two CTEs are connected by a "<join>" join
    When the CTE dependency diagram for that model is rendered
    Then the edge between those CTEs carries the "<join>" color class
    And the legend maps that color to "<join>"

    Examples:
      | join  |
      | inner |
      | left  |
      | right |
      | full  |
      | cross |

  Scenario: The legend is always present when a DAG is rendered
    Given a model with at least one CTE-to-CTE join
    When the CTE dependency diagram for that model is rendered
    Then a join-type color legend is visible
    And the legend palette is colorblind-safe (not red/green alone)
