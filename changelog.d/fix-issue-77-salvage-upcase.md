### Fixed

- Salvage's damage report missed an Up-case Table whose TableChecksum matches and whose encoding is sound but which maps fewer than the 65536 characters section 7.2.5.1 requires, or whose first 128 mappings differ from Table 24 (section 7.2.5); the deviation report already flagged both, and salvage now reports them as malformed damage while still reading the table.
