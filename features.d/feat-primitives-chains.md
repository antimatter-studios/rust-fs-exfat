## Reading

| Permissive chain and allocation primitives | Experimental; partial FAT/root/stream walks, allocation classes, predecessor indexes, chain heads, islands, statistics and boot-copy reports preserve damaged-data behavior without strict validation or device access | Unreleased (#27) | #27 | `src/primitive_chains.rs`, `tests/primitive_cases/chains.rs` |

## Validation

| Chain and allocation platform differential | Supported on macOS; formatter FAT/root/stream chains, allocation bits, offsets, predecessors, islands, statistics and every read-only driver file hash agree at both sector sizes; synthetic evidence is limited to damaged cases | Unreleased (#27) | #27 | `tests/oracle/primitive_chains.rs` |
