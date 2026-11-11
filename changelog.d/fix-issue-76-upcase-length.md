### Fixed

- The strict reader, the allocation audit and the deviation report refused any Up-case Table longer than 128 KiB; sections 7.2.4, 7.2.5 and 7.2.5.1 set no maximum DataLength, and a compressed table with short identity runs can be longer than an uncompressed one, so such a table is now read when it maps exactly 0000h to FFFFh. The read stays bounded by the cluster chain, `max_metadata_bytes` and the audit memory budget, and an empty or odd DataLength is still refused. Salvage keeps its own limit for now (#91).
