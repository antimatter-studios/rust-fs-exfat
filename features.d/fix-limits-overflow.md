## Reading

| Entry-set limits outside the specification's range | Experimental; `parse_set` and `find_sets` accept any `Limits` value, keeping a checksum-failed set whose first cluster is zero or from 2 to `cluster_count + 1` and whose length is at most `cluster_count * cluster_size`, both worked out without overflow; synthetic evidence only, since no oracle formats a volume outside the specification's range | Unreleased (#112) | #112 | `src/primitives.rs`, `tests/primitive_cases/parse.rs` |
