## Reading

| Primitive-backed strict boot parsing | Experimental; raw headers and complete boot regions report damage while strict validation and errors remain unchanged | Unreleased (#47) | #47 | `src/primitives.rs`, `src/boot.rs`, `tests/primitive_cases/boot.rs`, `tests/boot.rs` |

## Validation

| Primitive-backed boot evidence | Healthy 512- and 4096-byte boot copies are checked against the macOS formatter and checker; damaged identities, zero geometry, checksum repetitions, and extended signatures use synthetic fixtures only | Unreleased (#47) | #47 | `tests/oracle/boot.rs`, `tests/primitive_cases/boot.rs` |
