## Reading

| Entry sets past eighteen secondaries | Experimental; salvage and `parse_set` accept a File entry set with up to 255 secondary entries (sections 6.3.2, 7.4.2 and 7.8), so a set with Vendor Extension entries that the strict reader accepts is also a salvage candidate; a scan reads at most 8 KiB per set inside its 1 MiB window, whatever a damaged SecondaryCount claims | Unreleased (#70) | #70 | `src/primitives.rs`, `src/salvage_exfat.rs`, `tests/salvage_cases/secondary_count.rs` |
