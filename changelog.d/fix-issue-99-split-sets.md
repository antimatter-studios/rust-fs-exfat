### Fixed

- Salvage's candidate scan missed a file entry set whose entries cross from one directory cluster into the next cluster of the directory's FAT chain when that cluster is not physically adjacent, so files in a directory that grew after other data was written were dropped without a trace; the scan now follows the FAT from a set that runs past its cluster and reports a checksum-valid set found that way.

### Changed

- `EntrySetDetail` gains `continuation`, the extents of a split set's entries past its first cluster in directory order; the candidate's location then covers only the entries in that first cluster, and the list is empty for a set in one piece.
