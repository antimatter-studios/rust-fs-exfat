### Fixed

- One unreadable sector in a 64 KiB stretch of the FAT failed every FAT word in that stretch, so salvage's walks, damage report, rebuilt tree, uninterpreted entries and Up-case Table named a readable FAT word unreadable and lost the chain behind it; `primitives::FatWindow` now retries a failed chunk read per 512-byte sector and fails only the words whose own sector does not read, which the strict allocation audit and deviation checks share (#107).
