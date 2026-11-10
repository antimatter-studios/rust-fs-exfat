### Fixed

- Check-only fsck audits every valid exFAT cluster count without fixed cluster or directory-entry caps, using budgeted bitmaps and streamed FAT/directory reads instead of extraction limits.
- Incomplete audits and exhausted memory budgets fail explicitly without a partial clean report; exit statuses and read-only operation are unchanged.
