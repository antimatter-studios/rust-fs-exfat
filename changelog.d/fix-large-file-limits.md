### Fixed

- Files and FAT chain walks can exceed 1,048,576 clusters under volume-derived bounds and the explicit metadata-memory budget. Contiguous files no longer allocate a cluster index; FAT indexes remain bounded and failures never return partial chains or truncated files.
