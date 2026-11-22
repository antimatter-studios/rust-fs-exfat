## Interfaces

| Shared volume, primitive and file layers | Experimental; `layers::ExfatVolume` implements rust-fs-core's `VolumeAPI`, `OpenVolume`, `PrimitiveAPI` and `FileAPI` over the strict reader, the boot regions and `ExfatSalvage`. rust-fs-core's conformance helpers pass on synthetic volumes at 512 and 4096 bytes per sector and on every kernel-made fixture, healthy and damaged, and the shared `FileAPI` lists each undamaged fixture as the Linux kernel did | Unreleased (#238) | #238 | `src/layers.rs`, `tests/primitive_cases/shared_layers.rs`, `tests/images.rs` |
