### Added

- `ExfatSalvage::fat_copies` compares a TexFAT volume's First and Second FAT entry by entry and its First and Second Allocation Bitmap bit by bit, passing each heap cluster they record differently, with its value and byte offset in each copy, to a sink, and returns a `CopyReport` naming the active pair and how many clusters were compared.
- `ExfatSalvage::with_fat` gives the adapter over either FAT and its Allocation Bitmap, so extent walks, tree rebuilds and stale-chain listings can be taken from the inactive pair as evidence; `ExfatSalvage::fat` says which pair an adapter uses.
- `primitives::FatLayout` locates the First and Second FAT and names the active `FatCopy`; `primitives::fat_differences` and `primitives::bitmap_differences` compare two copies' heap clusters without judging either.

### Changed

- `ExfatSalvage::stale_chains` reads the Allocation Bitmap paired with the adapter's FAT, which is the active one unless `with_fat` chose the other; on an unchanged adapter it reads the same bitmap as before.
