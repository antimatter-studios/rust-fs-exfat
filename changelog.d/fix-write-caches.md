### Fixed

- A write mount no longer re-reads every directory on a path, walks a file's whole cluster chain or re-reads the root directory on each call: directories and cluster chains are checked once and cached, and every write updates them, so streaming a large file in small `pwrite` calls or creating many files in one directory costs time linear in the work.
