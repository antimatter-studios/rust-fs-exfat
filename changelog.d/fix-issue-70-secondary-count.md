### Fixed

- Salvage missed any file whose entry set has more than 18 secondary entries, because `parse_set` capped SecondaryCount at 18 and the candidate scan read only 19 entries per set; section 6.3.2 allows 255 and section 7.8 lets Vendor Extension entries fill a set to that limit, and the strict reader already accepted such files, so salvage now finds them too.
