### Added
- A format-neutral salvage adapter trait finds candidates, walks extents, rebuilds trees, reports damage, reads by extent and materializes plans, taking sources only as `BlockRead` and reporting missing capabilities as `Unsupported` with the feature named.
- The exFAT salvage adapter implements it on the permissive primitives, keeping deleted and damaged records, broken chains, orphans and conflicting parents as evidence.
- Permissive root-directory entries report their byte offsets.
