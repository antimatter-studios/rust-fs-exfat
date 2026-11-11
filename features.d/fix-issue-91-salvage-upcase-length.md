## Reading

| Up-case Tables longer than 128 KiB in salvage | Experimental; salvage's damage report and `upcase_table` read and check a compressed Up-case Table of any nonzero DataLength (sections 7.2.4, 7.2.5 and 7.2.5.1), bounded by its cluster chain and a 256 MiB read limit; past the limit the chain is checked that far and the content is not read | Unreleased (#91) | #91 | `src/salvage_exfat.rs`, `src/salvage_upcase.rs`, `tests/salvage_cases/upcase.rs` |
