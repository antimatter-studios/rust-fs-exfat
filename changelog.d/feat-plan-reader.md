### Added

- `SalvageAdapter::plan_reader` executes a materialization plan as a `PlanReader`, whose `read_at` reads any output offset on demand, reading only the source bytes the request covers, and returns a `PlanRead` with the provenance and unreadable spans of the bytes read; `source_of` names the source byte behind any output byte without reading.

### Changed

- A failed source read during materialization is retried in 512-byte spans aligned on the source rather than on the plan step, so an unreadable sector is flagged as exactly that sector's bytes wherever a read starts.
