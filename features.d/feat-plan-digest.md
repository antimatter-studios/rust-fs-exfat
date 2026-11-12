## Reading

| Salvage plan digest | Experimental; every materialization plan has a stable digest, the SHA-256 of a documented, versioned canonical encoding of its steps, lengths and metadata, pinned by a test against bytes written out by hand from the documented layout and hashed with macOS `shasum -a 256`; any change to a step, length or metadata field changes it, and the same plan materialized twice gives identical bytes and reports | Unreleased (#94) | #94 | `src/salvage_plan.rs`, `tests/salvage_cases/plans.rs` |
