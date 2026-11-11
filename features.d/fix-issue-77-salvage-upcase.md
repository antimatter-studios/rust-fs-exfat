## Reading

| Incomplete Up-case Table in salvage damage | Experimental; salvage's damage report flags a checksum-valid Up-case Table that maps fewer than 65536 characters (section 7.2.5.1) or whose first 128 mappings break Table 24 (section 7.2.5), as the `upcase-coverage` and `upcase-mandatory` deviation rules already did; salvage still reads the table and reports it, without rejecting it | Unreleased (#77) | #77 | `src/salvage_exfat.rs`, `tests/salvage_cases/upcase.rs`, `tests/salvage.rs` |
