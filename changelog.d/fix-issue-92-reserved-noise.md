### Fixed

- Salvage's entry-set finder returned bytes whose set checksum fails as a damaged candidate even when the File entry's reserved bytes 6-7 or 25-31 (section 7.4) or the Stream Extension's reserved bytes 2, 6-7 or 16-19 (section 7.6) were not zero; `parse_set`, `find_sets` and salvage's candidate scan now reject such bytes as noise, while a set whose checksum matches is still kept as written.
