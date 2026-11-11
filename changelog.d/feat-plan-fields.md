### Added

- Materialization plans can name cluster runs (`PlanStep::Clusters`), which map onto the source through the adapter's new `cluster_layout`, and the exFAT adapter maps them onto the Cluster Heap.
- Plans can declare a final length, which may cut off part of the last step, and a valid length, past which the output reads as zeros without the source being read.
- Plans carry the rebuilt file's name and times as `PlanMetadata`.
- `SalvageError::Plan` names a plan whose clusters lie outside the layout or whose declared lengths disagree with its steps, before anything is read or written.

### Changed

- `Plan` has new public fields, so a struct literal of it needs `Plan::new(steps)` or `..Plan::default()`; `PlanStep`, `Feature` and `SalvageError` have new variants. Plans without the new fields serialize as before.
