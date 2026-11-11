### Fixed

- A compressed Up-case Table identity run with a count of 0 was refused as running outside the Unicode range; section 7.2.5 does not require a count of at least one and macOS accepts it, so the primitive decoder, the strict reader, salvage and the `upcase-run` deviation rule now read it as no mappings.
