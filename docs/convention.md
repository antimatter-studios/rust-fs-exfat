# Driver convention v1

## 1. Purpose and scope

A driver convention is a shared contract for driver layers, evidence and tests.
It applies to every rust-fs filesystem driver and every rust-img image driver.
It is not an exFAT API specification. exFAT is the first implementation case.

The eight agreed rules are in
[#51](https://github.com/antimatter-studios/rust-fs-exfat/issues/51).
This document records them without changing them. The numbered rules in the
status table retain the issue's numbering. Section numbers organize this guide.
The convention does not require a read-only driver to gain write support.

## 2. Layers

Only primitives decode on-disk structures. Each higher API uses those primitives.
Salvage is a peer of the normal APIs, not a mode of the file API.

```text
        volume API           node / file API       salvage adapter
     probe, open, check      lookup, list, read     candidates, evidence
             |                      |                     |
             +----------------------+---------------------+
                                    |
                     extensions behind capability flags
                  (used by the APIs that need each feature)
                                    |
                        primitive API: format knowledge
                                    |
                         source bytes / device reads
```

The extension row is optional, not a required hop. APIs may call primitives
directly. Extensions also sit on primitives. They do not own another decoder.

### Primitive API

Owns byte layouts, field decoding and format algorithms. It returns what the
bytes say and reports damage. Any encoding for a write-capable driver belongs
here too. Primitives must not impose the strict reader's acceptance policy or
turn a possible object into a trusted file.

exFAT example: `primitives::BootSector::parse` exposes raw boot fields and
`signature_ok` in [src/primitives.rs](../src/primitives.rs).
`primitives::clusters_for` walks stream claims in
[src/primitive_chains.rs](../src/primitive_chains.rs).
These are examples, not a claim that all current helpers meet the convention.

### Volume API

Owns volume-level operations: probe, open, mount, flush, label, UUID, capacity,
check, repair and format, where supported. It applies its documented strict
policy to primitive results. It must not decode structures again or make
salvage depend on a successful strict mount. Unsupported mutating operations
remain unsupported on a read-only driver.

exFAT examples: `Volume::open` in [src/volume.rs](../src/volume.rs) and `check`
in [src/check.rs](../src/check.rs). Opening currently uses strict boot and
metadata parsers. This is a layering gap, not the target architecture.

### Node and file API

Owns lookup, listing, reads, writes where supported, and node metadata.
It presents files that satisfy the strict API's contract. It must not expose
an orphan or conflicting recovery claim as an ordinary trusted file. It must
not decode disk structures outside primitives.

exFAT examples: `Volume::lookup` in [src/path.rs](../src/path.rs),
`Volume::root_directory` in [src/volume.rs](../src/volume.rs), and
`File::read_at` in [src/file.rs](../src/file.rs).
`directory::parse` still decodes entry sets itself in
[src/directory.rs](../src/directory.rs).

### Salvage adapter

Owns the common recovery verbs and their evidence results. It uses primitives
without requiring strict volume or file acceptance. It returns candidates,
orphans, conflicting claims and damage, not trusted files. It must not repair
the source or hide uncertainty by routing results through the file API.

There is no exFAT adapter function on the checked main revision.
`primitives::find_sets` in [src/primitives.rs](../src/primitives.rs) is a
supporting example: it finds live and deleted entry-set candidates and retains
their checksum status. It is not an implementation of the common trait.
The exported modules in [src/lib.rs](../src/lib.rs) contain no salvage module.
[#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52) adds it.

### Extensions

Own format-specific operations outside the common set. They are exposed behind
capability flags. Their format knowledge stays in driver primitives. They must
not add an exFAT-only verb to the common trait or duplicate a decoder.

exFAT example: `primitives::allocation_class` in
[src/primitive_chains.rs](../src/primitive_chains.rs) distinguishes free clusters
with nonzero FAT entries. This is existing supporting logic, not a flagged
extension interface. That interface is not present in
[src/lib.rs](../src/lib.rs).

## 3. Strict versus permissive

Primitives return what the bytes say and report damage. Strict APIs reject on
top of the same primitives. The salvage path accepts damage and reports it.
Damage alone is not a reason for a primitive or salvage operation to refuse
otherwise recoverable evidence.

The rejection rule is:

- A strict API may reject input that fails its documented validity contract.
  It makes that decision from primitive results, not a second decoder.
- A primitive or salvage operation may fail when it cannot perform the
  requested operation safely, such as an unreadable range or an unrepresentable
  offset. It must report the failure. It must not label incomplete work clean.
- A damaged but readable structure must remain evidence on the permissive path.
  A strict checksum failure must not become a prerequisite for finding it.
- An implementation limit is not evidence of disk damage. If an audit truly
  cannot finish, it must say why. An arbitrary size cap is not a format rule.
  This distinction is explicit in
  [#38](https://github.com/antimatter-studios/rust-fs-exfat/issues/38).

The rules do not settle every recognition threshold or partial-result shape.
Those questions stay open in section 8; they are not permission to discard
damage silently.

Current exFAT has useful permissive behavior but not the full contract.
`primitives::parse_set` preserves a bad checksum when its plausibility checks
pass. It returns `None` for other candidate shapes. Primitive boot parsing
requires the filesystem name. Several raw helpers document unchecked slice
preconditions ([src/primitives.rs](../src/primitives.rs)).
`read_structures` reports read and geometry errors, but supports only its fixed
512-byte-sector snapshot layout
([src/primitive_structures.rs](../src/primitive_structures.rs)).
These constraints need review under
[#46](https://github.com/antimatter-studios/rust-fs-exfat/issues/46) and
[#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52).

The strict reader separately rejects invalid entry checksums, names and stream
claims ([src/directory.rs](../src/directory.rs)). `check` refuses cluster or
entry counts beyond the configured chain limit
([src/check.rs](../src/check.rs)). The default is 1,048,576
([src/volume.rs](../src/volume.rs));
[#38](https://github.com/antimatter-studios/rust-fs-exfat/issues/38) tracks the
gap, and open implementation PR
[#39](https://github.com/antimatter-studios/rust-fs-exfat/pull/39) addresses it.

## 4. Salvage adapter trait

Each driver implements one common trait. The required verbs are:

| Verb | Contract |
|---|---|
| Find candidates | Return possible objects with their supporting evidence, without declaring them trusted files. |
| Walk chains or extents | Follow storage claims and retain available extents, breaks and conflicting claims as evidence. |
| Rebuild the tree | Reconstruct possible relationships while retaining orphans and conflicting parent claims. |
| Report damage | Expose observed damage rather than discarding it or treating it as successful validation. |
| Read by extent | Read the source bytes named by an extent without requiring a trusted file object. |
| Materialize from a plan | Produce the result selected by a recovery plan without changing the source or declaring its claims trusted. |

These are verbs, not Rust method signatures. Signatures, result types and plan
types are settled in
[#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52).
The common trait must not expose exFAT names. Format knowledge stays in the
driver, below the adapter.

Read-only is a type property. No adapter method takes a writable source device.
A runtime read-only flag is not a substitute. Materialization is not in-place
repair. Any output destination is separate from the source; its exact contract
belongs to #52. exFAT already accepts `BlockRead` and wraps devices in
`ReadOnlyDevice` in [src/device.rs](../src/device.rs) and
[src/interface.rs](../src/interface.rs). That alone does not prove the missing
adapter read-only.

Capability flags say what a driver lacks. Callers must be able to distinguish
an unavailable capability from an empty successful result. Optional extensions
are also behind flags. Flag names and granularity belong to #52. That issue
expects no missing exFAT capabilities in the first version; this is a target,
not an implemented fact.

The trait lives in the driver until the agreed rule of three is met:
**nothing goes into rust-fs-core until a second driver implements it**.
This is rule 6's exact threshold, despite its name. It does not require three
implementations. #52 explicitly keeps the first trait in this crate.

## 5. Common set and extension rule

The common set stays small. A verb belongs there only if every filesystem can
support its format-neutral meaning. It must describe evidence or an operation,
not an exFAT structure. The six verbs above are the agreed starting set.
Before adding another verb, check it against the other drivers' needs, as #52
requires. If it does not pass that test, keep it as a flagged extension.

Chain and extent forms express the same common storage-walking need. A caller
must not need a FAT-specific method to use it. The rule of three is a separate
gate on moving an abstraction to rust-fs-core; it does not enlarge the trait.

Existing exFAT work illustrates the boundary:

- [#40](https://github.com/antimatter-studios/rust-fs-exfat/issues/40) adds
  evidence from both TexFAT FATs and bitmaps. Current `fat_entry` selects the
  active FAT, and opening loads the active bitmap
  ([src/volume.rs](../src/volume.rs)). There is no both-copy comparison there.
- [#41](https://github.com/antimatter-studios/rust-fs-exfat/issues/41) adds a
  report of stale FAT chains on free clusters. `allocation_class` and
  `chain_islands` already provide supporting logic
  ([src/primitive_chains.rs](../src/primitive_chains.rs)), not the full report.
- [#42](https://github.com/antimatter-studios/rust-fs-exfat/issues/42) remains
  open, but compressed upcase decoding already exists in `UpcaseTable::parse`
  ([src/metadata.rs](../src/metadata.rs)). Lookup uses the volume table
  ([src/path.rs](../src/path.rs)). Do not treat the open issue as proof that
  decoding is absent. Move that decoding under primitives through
  [#49](https://github.com/antimatter-studios/rust-fs-exfat/issues/49), rather
  than inventing a common upcase verb.
- [#43](https://github.com/antimatter-studios/rust-fs-exfat/issues/43) adds
  evidence for uninterpreted entries. Current root parsing skips benign sets
  and rejects unknown critical entries
  ([src/metadata.rs](../src/metadata.rs)).
- [#44](https://github.com/antimatter-studios/rust-fs-exfat/issues/44) adds
  deviations with byte offsets, specification sections and severity. The
  current `CheckReport` has counts, flags and boot differences, not that list
  ([src/check.rs](../src/check.rs)). Such format details can enrich damage
  evidence without adding format-named verbs to the common trait.

### 5a. Read-only images

Inherently read-only formats, including squashfs and erofs, get the salvage
surface minus deletion-specific parts. Finding extant or damaged candidates,
walking extents, rebuilding relationships, reporting damage, reading by extent
and materializing a plan still apply. Deleted-entry discovery and deleted-chain
interpretation do not apply where the format has no such concept. Flags must
make that distinction visible; absence is not an error or a clean scan result.

A read-only implementation is not the same as a read-only format. exFAT's
current driver is read-only by type, yet its primitives recognize deleted entry
sets ([src/primitives.rs](../src/primitives.rs)). exFAT therefore does not prove
the read-only-image case. #52 must describe it, and #53 must test its capability
behavior without inventing deleted-file support for those formats.

## 6. Conformance requirements

[#53](https://github.com/antimatter-studios/rust-fs-exfat/issues/53) builds the
reusable test set in this crate, with an exFAT runner. Every driver runs it
against its own adapter and volume API. The reusable module contains no
exFAT-specific names. It must check that:

- Adapter verbs return consistent evidence across candidates, extents and trees.
- The salvage path is read-only by type, including materialization's source.
- Salvage reports damaged input instead of rejecting it for that damage.
- The strict API rejects the same damaged input under its validity contract.
- Capability flags match behavior, including absent deletion-specific parts.

An unsupported capability is not a passing exercise of that capability.
Existing exFAT primitive, strict-reader and read-only tests are supporting
evidence, not a substitute for this adapter suite
([tests/primitives.rs](../tests/primitives.rs),
[tests/read_only.rs](../tests/read_only.rs),
[tests/check.rs](../tests/check.rs)).

## 7. exFAT status against the eight rules

Snapshot: main at `6b63a7d142626d0fcf9804514abf1defb4730924`, checked on
2026-11-10. Every linked work item below was checked with `gh issue view`.
All are open in this snapshot. #39 is an open PR, not a merged fix. Work in
other branches does not count as implemented on main.

**Met** means the checked implementation respects the rule. **Partial** means
supporting pieces exist but the rule is not complete. **Not met** means the
required surface or demonstration is absent. For rule 7, this is not a claim
that exFAT is an inherently read-only format.

| Rule from #51 | Status | Checked state and remaining work |
|---|---|---|
| 1. All APIs sit on primitives; only primitives decode structures. | partial | Primitives exist, but strict paths still decode structures or duplicate chain/read logic: [src/boot.rs](../src/boot.rs), [src/directory.rs](../src/directory.rs), [src/metadata.rs](../src/metadata.rs), [src/volume.rs](../src/volume.rs), [src/file.rs](../src/file.rs). Close through [#46](https://github.com/antimatter-studios/rust-fs-exfat/issues/46) and children [#47](https://github.com/antimatter-studios/rust-fs-exfat/issues/47), [#48](https://github.com/antimatter-studios/rust-fs-exfat/issues/48), [#49](https://github.com/antimatter-studios/rust-fs-exfat/issues/49), [#50](https://github.com/antimatter-studios/rust-fs-exfat/issues/50). |
| 2. Primitives are permissive; strict rejection sits above them; salvage reports damage. | partial | [src/primitives.rs](../src/primitives.rs) retains some damaged candidates, but also filters them. Strict parsing is separate, and [src/check.rs](../src/check.rs) has audit caps. Complete [#46](https://github.com/antimatter-studios/rust-fs-exfat/issues/46), review permissive evidence in [#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52), fix limits via [#38](https://github.com/antimatter-studios/rust-fs-exfat/issues/38) / [#39](https://github.com/antimatter-studios/rust-fs-exfat/pull/39), and add deviation reporting in [#44](https://github.com/antimatter-studios/rust-fs-exfat/issues/44). |
| 3. Salvage is read-only by type, separate, and returns evidence rather than trusted files. | partial | [src/device.rs](../src/device.rs) has the read-only type boundary; [src/primitives.rs](../src/primitives.rs) returns candidate entry sets. There is no complete salvage surface for orphans and conflicts among the exports in [src/lib.rs](../src/lib.rs). Implement [#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52) and prove it with [#53](https://github.com/antimatter-studios/rust-fs-exfat/issues/53). |
| 4. Each driver implements the six-verb common salvage trait, with capabilities. | not met | No adapter trait or capability surface is exported by [src/lib.rs](../src/lib.rs). [#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52) defines and implements it. |
| 5. Only universal verbs are common; everything else is a flagged extension. | not met | No common trait or extension flags exist in [src/lib.rs](../src/lib.rs). [#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52) establishes that boundary. Format evidence work is tracked by [#40](https://github.com/antimatter-studios/rust-fs-exfat/issues/40), [#41](https://github.com/antimatter-studios/rust-fs-exfat/issues/41), [#42](https://github.com/antimatter-studios/rust-fs-exfat/issues/42), [#43](https://github.com/antimatter-studios/rust-fs-exfat/issues/43) and [#44](https://github.com/antimatter-studios/rust-fs-exfat/issues/44), with current support described in section 5. |
| 6. Nothing moves to rust-fs-core until a second driver implements it. | met | exFAT format helpers remain in [src/primitives.rs](../src/primitives.rs), [src/primitive_chains.rs](../src/primitive_chains.rs) and [src/primitive_structures.rs](../src/primitive_structures.rs); there is no extracted salvage trait. No current gap. [#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52) must keep the first trait here. |
| 7. Read-only images get salvage minus deletion-specific parts. | not met | The shared capability surface needed to express this case does not exist yet. exFAT's live/deleted parser in [src/primitives.rs](../src/primitives.rs) does not demonstrate an inherently read-only format. Define the contract in [#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52) and its capability tests in [#53](https://github.com/antimatter-studios/rust-fs-exfat/issues/53). |
| 8. Each driver runs a conformance set against its adapter. | not met | [tests/primitives.rs](../tests/primitives.rs), [tests/read_only.rs](../tests/read_only.rs) and [tests/check.rs](../tests/check.rs) test existing APIs, not a common adapter. [#53](https://github.com/antimatter-studios/rust-fs-exfat/issues/53) supplies the reusable module and exFAT runner after [#52](https://github.com/antimatter-studios/rust-fs-exfat/issues/52). |

Summary: **1 met, 3 partial, 4 not met**. This document closes the documentation
task, not the implementation gaps.

## 8. Open questions

These questions do not change the eight rules.

1. Which recognition failures may return no candidate, and which must return
   damaged evidence? Resolve filesystem-name rejection and checksum-dependent
   plausibility filtering in [src/primitives.rs](../src/primitives.rs) under
   #46 and #52. Also settle how safe wrappers handle raw slice preconditions.
2. What are the trait's signatures, evidence identities, partial-result types
   and recovery-plan representation? #52 owns those decisions. In particular,
   how does materialization deliver output and prevent source aliasing?
3. Which capability bits describe missing operations, deletion-specific parts
   and extensions? What result represents an unsupported part? #52 defines
   them; #53 checks their behavior.
4. Which damage details are common, and which are driver extensions? Decide
   how offsets, severity and specification references from #44 fit evidence
   without adding exFAT names to the trait.
5. Which read-only format will first exercise the non-deletion contract, and
   which driver will provide the second implementation before any core move?
   exFAT alone cannot supply that evidence. #52 and #53 are the current work.
6. Should the label "rule of three" be renamed to match the agreed
   second-driver threshold? Until then, the literal rule in #51 governs.
7. Does #42 need remaining acceptance work, or only issue reconciliation?
   Compressed decoding and lookup already exist in
   [src/metadata.rs](../src/metadata.rs) and [src/path.rs](../src/path.rs).
   Its open state alone does not answer whether its full acceptance criteria
   have been met.
8. What do candidates and tree rebuilding mean for a rust-img container driver
   that has no native filesystem tree? #52 must check the common verbs against
   those drivers too, without inventing on-disk objects they do not have.
