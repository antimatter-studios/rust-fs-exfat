## Interfaces

| Shared salvage adapter | Experimental; `ExfatSalvage` implements the rust-fs-core salvage trait, with positioned extents and named parent claims. The deprecated exFAT API keeps its prior types and behavior for one release; synthetic conformance and compatibility coverage. | Unreleased (#241) | #241 | `src/salvage_exfat.rs`, `tests/conformance.rs`, `tests/old_api_compat.rs` |
