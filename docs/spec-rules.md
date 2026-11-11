# exFAT specification rules

The rules `check` reports as spec deviations (#44), one rule per row, written
from Microsoft's
[exFAT file system specification](https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification)
(revision 1.00). Section numbers are that document's. The rule identifiers are
the `rule` values in `CheckReport::deviations` and in the fsck report's
`deviations` object; `primitives::Rule` holds the same table in code, and
`tests/check/deviations.rs` fails when the two disagree.

A deviation is a place where the bytes break a rule of the specification. It is
reported with the byte offset of the deviating field, relative to the start of
the volume (an fsck `--offset` is not added), the field's length, the section
and a severity. Deviations are evidence, apart from the existing inconsistency
result: they change neither `CheckReport::healthy`, nor the exit status, nor
what the strict reader accepts or rejects, nor what salvage finds.

## Severity

| Severity | Meaning |
|---|---|
| error | The specification makes the value invalid or the structure unusable: a valid range, a "valid value", a checksum, or an entry that "renders" a set, directory or volume invalid. |
| warning | A mandatory ("shall") rule is broken, but the specification does not invalidate the structure and a reader can still use it. |
| notice | A recommendation ("should") is not followed, or a Reserved field is not zero. Section 1.2 says implementations shall initialize Reserved fields to zero, but also that they shall preserve them and should not interpret them, so a later revision may use them. |

## Columns

- **Strict layer**: what the strict reader (`Volume::open`, `check`) does with
  a violation. *Rejects* means `check` fails; *accepts silently* means it
  reads on without a word, which is why the deviation report exists.
- **macOS**: `fsck_exfat -n` on macOS 27.2 with one rule broken at a time
  (see [the macOS comparison](#the-macos-comparison)). *Accepts* means it
  exits 0 and reports nothing; on an *error* row that is Apple accepting what
  the specification forbids. *Reports* means it exits non-zero. *Not run*
  means the rule was not broken on a macOS volume. macOS behaviour is evidence
  about one implementation, not the specification.

## Boot region

Each rule is checked on the main boot region (sector 0) and on the backup
(sector 12), at the offsets of that copy, except where a row says *main only*:
section 3.1 says implementations shall treat the backup's VolumeFlags and
PercentInUse as stale.

| Rule | Section | Severity | Requirement | Strict layer | macOS |
|---|---|---|---|---|---|
| `jump-boot` | 3.1.1 | error | JumpBoot (bytes 0–2) is EBh 76h 90h. | Rejects | Reports |
| `file-system-name` | 3.1.2 | error | FileSystemName (bytes 3–10) is `EXFAT` and three spaces. | Rejects | Reports |
| `must-be-zero` | 3.1.3 | error | MustBeZero (bytes 11–63) is zero. | Rejects | Reports |
| `volume-length` | 3.1.5 | error | VolumeLength is at least 2^20 / 2^BytesPerSectorShift sectors (1 MB). | Rejects | Reports |
| `fat-offset` | 3.1.6 | error | FatOffset is at least 24 and at most ClusterHeapOffset − FatLength × NumberOfFats. | Rejects | Not run |
| `fat-length` | 3.1.7 | error | FatLength is at least (ClusterCount + 2) × 4 / 2^BytesPerSectorShift, rounded up, and at most (ClusterHeapOffset − FatOffset) / NumberOfFats, rounded down. | Rejects | Not run |
| `cluster-heap-offset` | 3.1.8 | error | ClusterHeapOffset is at least FatOffset + FatLength × NumberOfFats and at most VolumeLength − ClusterCount × 2^SectorsPerClusterShift. | Rejects | Not run |
| `cluster-count` | 3.1.9 | warning | ClusterCount is the lesser of (VolumeLength − ClusterHeapOffset) / 2^SectorsPerClusterShift, rounded down, and 2^32 − 11. | Accepts a smaller count silently; a larger one also breaks 3.1.8 and is rejected | Accepts |
| `root-cluster` | 3.1.10 | error | FirstClusterOfRootDirectory is between 2 and ClusterCount + 1. | Rejects | Reports |
| `revision` | 3.1.12 | error | FileSystemRevision's major revision is 1, the only one this specification describes, and its minor revision is at most 99. | Rejects; also rejects 1.01 to 1.99, which section 3.1.12 says implementations should mount (#73) | Reports |
| `active-fat` | 3.1.13.1 | error | ActiveFat is 1 only when NumberOfFats is 2. Main only. | Rejects | Reports |
| `volume-flags-reserved` | 3.1.13 (Table 5) | notice | VolumeFlags bits 4–15, Reserved, are zero. Main only. | Accepts silently | Accepts |
| `bytes-per-sector-shift` | 3.1.14 | error | BytesPerSectorShift is between 9 and 12. | Rejects | Reports |
| `sectors-per-cluster-shift` | 3.1.15 | error | SectorsPerClusterShift is at most 25 − BytesPerSectorShift. | Rejects | Reports |
| `number-of-fats` | 3.1.16 | error | NumberOfFats is 1 or 2. | Rejects | Reports |
| `percent-in-use-range` | 3.1.18 | error | PercentInUse is between 0 and 100, or FFh. | Rejects | Accepts |
| `percent-in-use` | 3.1.18 | warning | PercentInUse is the active Allocation Bitmap's allocated clusters × 100 / ClusterCount, rounded down, or FFh. Main only. | Accepts silently | Accepts |
| `boot-reserved` | 3.1 (Table 4) | notice | Reserved (bytes 113–119) is zero. | Accepts silently | Accepts |
| `boot-signature` | 3.1.20 | error | BootSignature (bytes 510–511) is AA55h. | Rejects | Accepts |
| `extended-boot-signature` | 3.2.2 | error | Each Extended Boot Sector (sectors 1–8) ends with ExtendedBootSignature AA550000h. | Rejects | Accepts |
| `null-parameters` | 3.3.3 | notice | A Parameters structure whose ParametersGuid is the null GUID is a Null Parameters structure: its Reserved bytes 16–47 are zero. | Accepts silently | Accepts |
| `oem-reserved` | 3.3 (Table 7) | notice | The OEM Parameters sector's Reserved bytes, 480 to the end of the sector, are zero. | Accepts silently | Accepts |
| `boot-reserved-sector` | 2 (Table 3) | notice | The Reserved sub-region (sector 10 of the region) is zero. | Accepts silently | Accepts |
| `boot-checksum` | 3.4 | error | Every four bytes of the Boot Checksum sector (sector 11) hold the checksum of sectors 0–10, which skips VolumeFlags and PercentInUse. | Rejects | Reports |
| `backup-boot-region` | 3, 3.1 | warning | The backup boot region is a copy of the main one, outside VolumeFlags and PercentInUse. Reported once per run of differing backup bytes. | Rejects | Reports |

## File Allocation Table

The FAT rules read the active FAT (section 3.1.13.1: the inactive one is
stale). The chain rules apply to the chains the report walks: the root
directory, the Allocation Bitmap, the Up-case Table and every directory. File
data chains are walked only by the strict audit, whose inconsistency result
covers them.

| Rule | Section | Severity | Requirement | Strict layer | macOS |
|---|---|---|---|---|---|
| `fat-entry-0` | 4.1.1 | warning | FatEntry[0]'s three high bytes are FFh. | Accepts silently: never read | Accepts |
| `fat-media-type` | 4.1.1 | notice | FatEntry[0]'s low byte, the media type, should be F8h. | Accepts silently: never read | Accepts |
| `fat-entry-1` | 4.1.2 | warning | FatEntry[1] is FFFFFFFFh. | Accepts silently: never read | Accepts |
| `fat-chain-value` | 4.1.3 | error | Each FAT entry of a cluster chain is between 2 and ClusterCount + 1, FFFFFFF7h or FFFFFFFFh. | Rejects | Reports |
| `fat-chain-end` | 4.1.3 | error | The last FAT entry of a chain is FFFFFFFFh: a chain does not run into a cluster marked bad (FFFFFFF7h). | Rejects | Reports |
| `fat-chain-cycle` | 4.1.3 | error | A FAT entry never points to a cluster earlier in its own chain. | Rejects | Reports |

## Allocation Bitmap and Up-case Table

| Rule | Section | Severity | Requirement | Strict layer | macOS |
|---|---|---|---|---|---|
| `bitmap-entry-count` | 7.1 | error | The root holds exactly NumberOfFats Allocation Bitmap entries: one describing the First Allocation Bitmap and, with two FATs, one describing the Second. | Rejects | Reports |
| `bitmap-identifier` | 7.1.2.1 | error | BitmapIdentifier is 1 only when NumberOfFats is 2. | Rejects | Reports |
| `bitmap-flags-reserved` | 7.1.2 (Table 21) | notice | BitmapFlags bits 1–7, Reserved, are zero. | Rejects | Accepts |
| `bitmap-length` | 7.1.5 (Table 22) | error | The Allocation Bitmap's DataLength holds ClusterCount bits: at least ClusterCount / 8 bytes, rounded up. | Rejects; also rejects a longer bitmap, which Table 22's Reserved bits allow (#73) | Reports |
| `bitmap-reserved` | 7.1.5 (Table 22) | notice | The bitmap bits after BitmapEntry[ClusterCount + 1], Reserved, are zero. | Accepts silently | Accepts |
| `bitmap-free-cluster` | 7.1.5.1 | error | Every cluster an allocation consumes has its bitmap bit set. Checked for the walked chains above. | Rejects | Reports |
| `upcase-entry-count` | 7.2 | error | The root holds exactly one Up-case Table entry. | Rejects | Reports |
| `upcase-checksum` | 7.2.2 | error | TableChecksum is the checksum of the table as stored. | Rejects | Reports |
| `upcase-mandatory` | 7.2.5 (Table 24) | error | The first 128 mappings are those of Table 24. | Rejects | Not run |
| `upcase-coverage` | 7.2.5, 7.2.5.1 | error | The table, compressed or not, maps exactly the characters 0000h to FFFFh. | Rejects | Not run |
| `upcase-run` | 7.2.5 | error | In a compressed table, an FFFFh that starts an identity run is followed by the number of identity mappings it stands for: at least one, and no more than the characters left up to FFFFh. At the last character, FFFFh is that character's own mapping unless a count of 1 follows it. | Rejects | Reports a count past FFFFh and a missing count; accepts a count of 0 |

The Up-case Table is read in either form section 7.2.5 allows (#42): each word
maps the next character, except FFFFh, which with the count after it stands
for that many identity mappings. macOS `newfs_exfat` writes the compressed
form: the recommended table of section 7.2.5.1 (Table 25), 5836 bytes with
TableChecksum E619D30Dh, byte for byte. A run that cannot be expanded is
reported at its FFFFh, as `upcase-run`; coverage past it is unknown and is not
reported as well. Words after the mapping of FFFFh are reported at the first of
them. A table that ends early is reported at its first byte, and so is one
whose first 128 mappings break Table 24, over the words that encode those 128
mappings. A count of zero is read as a fault: section 7.2.5 calls the count
"the number of identity mappings" in "a series", and the strict reader has
always refused it. macOS accepts it (see below).

## Root directory entries

| Rule | Section | Severity | Requirement | Strict layer | macOS |
|---|---|---|---|---|---|
| `label-count` | 7.3 | error | The root holds at most one Volume Label entry. | Rejects | Accepts |
| `label-length` | 7.3.2 | error | CharacterCount is at most 11. | Rejects | Reports |
| `label-characters` | 7.3.3 | error | VolumeLabel holds none of Table 35's invalid characters. | Rejects | Reports |
| `guid-count` | 7.5 | error | The root holds at most one Volume GUID entry. | Rejects | Accepts |
| `guid-fields` | 7.5.2, 7.5.4.1 | error | A Volume GUID entry's SecondaryCount is 0 and its AllocationPossible and NoFatChain are 0. | Rejects | Accepts |
| `guid-null` | 7.5.5 | error | VolumeGuid is not the null GUID. | Rejects | Accepts |
| `guid-outside-root` | 7.5 | warning | A Volume GUID entry lies in the root directory. | Rejects | Reports |
| `system-entry-reserved` | 7.1, 7.2, 7.3, 7.5 (Tables 20, 23, 26, 32) | notice | The Reserved fields of the Allocation Bitmap (bytes 2–19), Up-case Table (1–3, 8–19), Volume Label (24–31) and Volume GUID (22–31, GeneralPrimaryFlags bits 2–15) entries are zero. | Accepts silently | Accepts |

## Directory entries and entry sets

| Rule | Section | Severity | Requirement | Strict layer | macOS |
|---|---|---|---|---|---|
| `entry-type-80` | 6.2.1 | error | No entry has EntryType 80h. | Rejects | Reports |
| `end-of-directory` | 6.2.1 | warning | Every entry after an end-of-directory marker (EntryType 00h) is also an end-of-directory marker. | Accepts silently: reading stops at the first marker | Reports |
| `set-in-use` | 6.2.1 | error | Every entry inside an entry set is a regular entry (81h–FFh): no unused entry or end-of-directory marker inside a set. | Rejects | Reports |
| `set-secondary` | 6.3 | error | The SecondaryCount entries after a primary are secondary entries. | Rejects | Not run |
| `orphan-secondary` | 6.3, 6.4 | error | A secondary entry in use belongs to the set of the primary before it. | Rejects | Reports |
| `set-checksum` | 6.3.3 | error | SetChecksum is the checksum of the set (File and benign primary sets). | Rejects | Reports |
| `set-truncated` | 6.3.2 | error | All SecondaryCount secondary entries follow the primary before the directory ends. | Rejects | Not run |
| `root-critical-primary` | 8.2, 6.3.1.2.1 | error | The root holds no critical primary entry that revision 1.00 does not define: one renders the volume invalid. | Rejects | Reports |
| `critical-primary-outside-root` | 6.3.1.2.1, 8.2 | error | No directory but the root holds a critical primary entry other than File: one renders that directory invalid. | Rejects | Reports |
| `revision-entry-type` | 3.1.12, 7 | warning | On a revision 1.00 volume every entry in use has a type revision 1.00 defines (section 7 lists them; TexFAT Padding A1h included). Unknown benign primaries and unknown critical or benign secondaries are structures of a later minor revision. | Accepts unknown benign entries silently (counted in `uninterpreted`); rejects unknown critical secondaries | Accepts an unknown benign primary (A5h) and benign secondary (E7h); reports an unknown critical secondary (C2h) |

## File entry sets

| Rule | Section | Severity | Requirement | Strict layer | macOS |
|---|---|---|---|---|---|
| `file-stream` | 7.4, 7.6 | error | Exactly one Stream Extension entry follows the File entry, immediately. | Rejects | Reports |
| `file-names` | 7.4, 7.7 | error | Exactly NameLength / 15, rounded up, File Name entries follow the Stream Extension, consecutively. | Rejects | Reports |
| `vendor-order` | 7.8, 7.9 | error | Vendor Extension and Vendor Allocation entries do not precede the Stream Extension and File Name entries. | Rejects | Reports |
| `file-attributes-reserved` | 7.4.4 (Table 28) | notice | FileAttributes bits 3 and 6–15, Reserved, are zero. | Accepts silently | Accepts |
| `file-reserved` | 7.4 (Table 27) | notice | Reserved1 (bytes 6–7) and Reserved2 (bytes 25–31) of the File entry are zero. | Accepts silently | Accepts |
| `timestamp` | 7.4.8 | error | Each Timestamp has DoubleSeconds at most 29, Minute at most 59, Hour at most 23, Month 1–12 and Day from 1 to the month's last day. | Rejects | Accepts |
| `ten-ms-increment` | 7.4.9 | error | Each 10msIncrement is at most 199. | Rejects | Accepts |
| `utc-offset` | 7.4.10.2 | error | OffsetFromUtc is 00h when OffsetValid is 0. | Rejects | Accepts |
| `stream-allocation-possible` | 7.6.2.1 | error | The Stream Extension's AllocationPossible is 1. | Rejects | Reports |
| `stream-reserved` | 7.6 (Table 33), 7.6.2 | notice | The Stream Extension's Reserved1 (byte 2), Reserved2 (bytes 6–7), Reserved3 (bytes 16–19) and GeneralSecondaryFlags bits 2–7 are zero. | Accepts silently | Accepts |
| `name-length` | 7.6.3 | error | NameLength is at least 1. | Rejects | Reports |
| `name-hash` | 7.6.4 | error | NameHash is the hash of the up-cased name. Checked when the Up-case Table is usable. | Rejects | Reports |
| `valid-data-length` | 7.6.5 | error | ValidDataLength is at most DataLength, and equal to it for a directory. | Rejects | Reports |
| `directory-length` | 7.6.7 | error | A directory's DataLength is the whole size of its allocation, a multiple of the cluster size matching its chain, and at most 256 MB. | Rejects | Accepts |
| `first-cluster` | 6.2.2, 6.4.3 | error | FirstCluster is 0 or between 2 and ClusterCount + 1 (Stream Extension, Vendor Allocation). | Rejects for a Stream Extension; accepts a Vendor Allocation silently | Reports, for a Stream Extension and for a Vendor Allocation |
| `data-length` | 6.2.3, 6.4.4 | error | DataLength is 0 when FirstCluster is 0, at most ClusterCount × the cluster size, and within what a walked chain holds. | Rejects for a Stream Extension; accepts a Vendor Allocation silently | Reports |
| `no-fat-chain` | 6.3.4.2, 6.4.3, 6.4.4 | error | NoFatChain is 1 only with AllocationPossible 1, a FirstCluster in the heap and a DataLength other than 0. | Rejects for a Stream Extension; accepts a Vendor Allocation silently | Accepts |
| `contiguous-range` | 6.3.4.2 | error | A NoFatChain allocation's DataLength / cluster size clusters, rounded up, lie inside the cluster heap. | Rejects for a Stream Extension; accepts a Vendor Allocation silently | Reports |
| `name-entry-flags` | 7.7.2.1, 6.3.4.2 | error | A File Name entry's AllocationPossible is 0, and so its NoFatChain is 0. | Rejects | Accepts |
| `name-flags-reserved` | 7.7.2 | notice | A File Name entry's GeneralSecondaryFlags bits 2–7, Reserved, are zero. | Accepts silently | Accepts |
| `name-characters` | 7.7.3 (Table 35) | error | The name holds none of Table 35's invalid characters. | Rejects | Reports |
| `name-reserved` | 7.7.3 | error | The name is not `.` or `..`. | Rejects | Accepts |
| `name-unused-zero` | 7.7.3 | notice | FileName characters past NameLength should be 0000h. | Accepts silently | Accepts |
| `name-duplicate` | 7.7 | error | No two names in one directory are equal after up-casing. Checked when the Up-case Table is usable. | Rejects | Reports |
| `vendor-guid` | 7.8.3, 7.9.3 | warning | A vendor entry's VendorGuid is not the null GUID. | Accepts silently | Accepts |
| `vendor-flags` | 7.8.2.1, 7.9.2.1 | warning | A Vendor Extension's AllocationPossible is 0; a Vendor Allocation's is 1. | Accepts silently | Accepts |

## Not deviations

These are states or strict-layer checks, not rules the bytes break:

- VolumeDirty and MediaFailure (3.1.13.2, 3.1.13.3) are states, reported as
  `CheckReport::dirty` and `media_failure`. ClearToZero (3.1.13.4) is valid
  either way.
- Lost clusters, allocated in the bitmap with no allocation consuming them,
  are the existing `unreferenced_clusters` inconsistency. Section 7.1.5.1
  does not require an owner for a set bit.
- The strict layer also rejects, with no rule of the specification behind it:
  names and labels that are not well-formed UTF-16; two allocations sharing a
  cluster; a FAT chain whose length is not DataLength / cluster size, rounded
  up, for a file; an empty stream with a FirstCluster other than 0 (sections
  6.2.3 and 6.3.6 only require DataLength 0 when FirstCluster is 0); a root
  directory chain longer than 256 MB (section 7.6.7 bounds DataLength, which
  the root does not have); and unknown critical secondary entries, which
  section 6.4.1.2.1 makes unrecognized, not invalid. #73 tracks whether each
  stays a choice of the strict reader.

## Coverage

The report walks what the strict audit walks, except file data chains: the
boot regions, FatEntry[0] and [1] of the active FAT, the root, the active
Allocation Bitmap, the Up-case Table and every directory reachable from the
root. It never fails on damage. When a structure cannot be read or does not
fit the memory budget, the parts that need it are skipped and named in
`skipped` (at most 64, the rest counted in `skipped_omitted`), and
`complete` is false. At most 64 deviations are listed per rule; the rest are
counted in `omitted`.

## The macOS comparison

Run on 2026-11-11, macOS 27.2: `newfs_exfat -R -S 512 -b 4096` formatted an
8 MiB image, and the macOS driver wrote a directory and two files to it. Each
case copied that image, broke one rule (resealing boot and set checksums so
that only the rule broke), attached the copy with `hdiutil attach -nomount
-readonly -nobrowse`, ran `fsck_exfat -n` on the device from that attachment's
own plist, detached it, and ran this crate's deviation report on the same
bytes. Only images made for the run were attached.

- The unbroken volume reported no deviation, and neither did two 64 MiB
  volumes, with 512-byte and 4096-byte sectors, populated the way the oracle
  tier populates its reference images. `tests/oracle/boot.rs` asserts the same
  of the oracle tier's reference images.
- Every broken copy reported its rule at the broken field.
- macOS accepts these *error* rules, which the specification forbids:
  `percent-in-use-range` (PercentInUse 101), `boot-signature`,
  `extended-boot-signature`, `label-count` (two Volume Labels), `guid-count`,
  `guid-fields`, `guid-null`, `timestamp` (month 13), `ten-ms-increment`,
  `utc-offset`, `directory-length` (a directory's DataLength one byte short of
  its cluster), `no-fat-chain` (NoFatChain on an empty file),
  `name-entry-flags` and `name-reserved` (a file named `..`). It also accepts
  every *warning* and *notice* rule it was given except `end-of-directory`.
- macOS also accepts what the specification allows and the strict layer
  refuses: FileSystemRevision 1.01 and a longer Allocation Bitmap (#73).
- For #42, on 2026-11-11, the same procedure ran on four copies of a fresh
  512-byte-sector volume whose compressed Up-case Table was changed, with
  DataLength and TableChecksum resealed. A count past FFFFh and a missing
  final count each reported `upcase-run` at the run's FFFFh; `fsck_exfat -n`
  exited 203 on both ("Upper case translation table is too large" and "too
  small"). A count of 0 reported `upcase-run` and the strict check refused
  it, while `fsck_exfat -n` exited 0. FFFFh's own mapping written as a run of
  one passed all three.
- Outside the deviation report, `check` exits 4 where macOS exits 0 on a
  Vendor Allocation that owns an allocated cluster (#71) and on an allocated
  cluster the FAT marks bad (#72): both are counted as unreferenced.
