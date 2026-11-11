### Fixed

- Salvage reported a compressed Up-case Table longer than 128 KiB as malformed and did not read it for name lookups; sections 7.2.4, 7.2.5 and 7.2.5.1 set no maximum DataLength, so such a table is now read, checked and used like any other. Salvage reads at most 256 MiB of it, its directory limit, and checks a longer table's chain only that far, without reporting its length as damage.
