### Fixed

- A volume mounts for writing on a raw device whose blocks are larger than the volume's sectors, such as a 512-byte-sector volume on a device that accepts only 4096-byte transfers: the write path widens its transfers to the device's block, reading the rest of a block it only partly covers, as the strict reader already did for reads.
