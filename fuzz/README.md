# Fuzzing

Decoder targets are added alongside the decoders. There are no format decoders
in the initial scaffold. Every target must consume only arbitrary bytes or
synthetic images and must never open a device or host filesystem path.

The eventual nightly cargo-fuzz workspace remains separate from the library
package, as in the family template. Stable regression tests replay minimized
failures; a fuzzer is not an independent filesystem oracle.
