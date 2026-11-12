### Added

- `SalvageAdapter::check_plan` checks a materialization plan against a source size without reading: it refuses extents past the source, cluster runs outside the cluster layout and declared lengths that disagree with the steps, and returns a `PlanCheck` with the output and valid lengths and every pair of steps that read the same source bytes, as findings.
