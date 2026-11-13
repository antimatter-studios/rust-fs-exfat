# Sleuth Kit: negative ActiveFat verdict

On November 13, 2026, the black-box evaluation for #178 rejected **Sleuth Kit
4.11.1 as an ActiveFat-aware two-FAT oracle**. It accepts mirrored two-FAT
images, but the controlled disagreement cases below show that this executable
uses the First FAT and First Allocation Bitmap when ActiveFat selects the
Second. An accepted listing is not sufficient evidence of correct readback.

This is a verdict about the tested executable, not every Sleuth Kit version.
It neither validates this crate's two-FAT writer nor establishes TexFAT
transaction or crash-recovery interoperability.

## Pinned environment and boundary

- fs-linux-test-harness Debian 12 shared guest, arm64; Linux tools ran only
  through `tests/guest_support`, on private guest-disk copies of new images.
- Debian binary packages `sleuthkit` and `libtsk19`, both
  `4.11.1+dfsg-1+b1`; the tools report `The Sleuth Kit ver 4.11.1`.
- `fsstat` SHA-256: `61e59dc09705499d185e2109d701cf02903c0bfa7acb301b8905ba16ab483f08`.
- `fls` SHA-256: `ffd51a65ad005b35fb43d6ce46af92f5da3bac061adea71e04b063662e030a02`.
- `icat` SHA-256: `8717a071a54ff7572c1b1f6e62d69ecf4396d7ee542506fc468a90fdf35c2628`.

The mixed IPL/CPL/GPL-licensed tools remain separate CLI processes. No
implementation source was read, linked, vendored or copied, and no new crate
dependency or normal guest-provisioning requirement was added.

## Images and controls

The crate formats new 64 MiB images with 512-byte sectors and 4 KiB clusters,
then writes `chain.bin` and `blocker.bin` through its writer. Writing one
cluster to the first file, one to the blocker, then appending two clusters
forces the first file into a FAT-linked chain. Every write mount unmounts
cleanly before any oracle sees the image.

On the two-FAT image, `chain.bin` occupies clusters **7 → 9 → 10**; the blocker
occupies cluster 8. Its 12,288 bytes are `index % 251`, with SHA-256
`2ffe74f47a7bb7350e913f6b9259080cbe3cee97b2d313d5e2fe2942108d98e9`.
The formatter places the FATs at sector 24, each 128 sectors long, the cluster
heap at sector 280, and the two bitmaps at clusters 2 and 3. The probe asserts
this geometry before applying its diagnostic edits.

The disagreement cases are **controlled, post-unmount edits of clones**, not
claims about bytes the writer emits. Both Main and Backup VolumeFlags select
the same ActiveFat and remain clean. These fields are outside the boot
checksum. Only the inactive copy is changed:

- FAT disagreement replaces the inactive FAT's link from cluster 7 to 9 with
  a link to blocker cluster 8; the selected FAT remains 7 → 9 → 10.
- Bitmap disagreement clears the inactive bitmap's bits for clusters 7, 9
  and 10; the selected bitmap still marks the file's clusters allocated.

The crate's strict reader returns the expected bytes for every variant. That
is a control over the edits, not independent validation of the writer.

## Observed matrix

All `fls`, `icat` and `blkstat` invocations below exited **0**. `fls` listed
both user files in every case. Allocation probes address the file's first
cluster: sector 184 for the single-FAT control, sector 320 for two FATs.

| Image | `icat` content | `blkstat` allocation of the first file cluster |
|---|---|---|
| Single FAT | 12,288 correct bytes | Allocated |
| Two mirrored FATs, ActiveFat 0 | 12,288 correct bytes | Allocated |
| Two mirrored FATs, ActiveFat 1 | 12,288 correct bytes | Allocated |
| Inactive Second FAT disagrees, ActiveFat 0 | 12,288 correct bytes | Allocated |
| Inactive First FAT disagrees, ActiveFat 1 | **8,192 wrong bytes, despite exit 0** | Allocated |
| Inactive Second Bitmap disagrees, ActiveFat 0 | 12,288 correct bytes | Allocated |
| Inactive First Bitmap disagrees, ActiveFat 1 | 12,288 correct bytes | **Not Allocated: wrong** |

The wrong extraction is the first 4 KiB of `chain.bin` followed by the 4 KiB
blocker, not its remaining 8 KiB. Its SHA-256 is
`8c953d7eb2ab5b4fbb4b08599890ebd50d9af1874e52dc0ccde95a8127b6a2ea`.
The symmetric ActiveFat-0 controls isolate the failure from general
two-FAT rejection, failed formatting, missing files or an unavailable tool.

`fsstat -f exfat` separately exceeded a 15-second deadline on single-FAT and
two-FAT crate images and an independently exfatprogs-formatted single-FAT
control. That bounded observation is tracked in [#201][fsstat-finding], not
treated as the ActiveFat verdict. No internal cause or permanent hang is
asserted.

## Reproduce the negative evidence

Use this worktree's normal prerequisites and pinned Rust toolchain. The probe
never mounts an image or examines a host device. It creates its own images,
installs only the pinned Debian binary package inside the harness guest,
and fails on missing tools, failed controls, version drift, timeouts or an
unexpected result. It leaves host images and JSON receipts in the printed
`tmp/sleuthkit-178.*` directory; the guest's private staged copies are removed.

```sh
mkdir -p tmp
export CARGO_TARGET_DIR="$(mktemp -d "$PWD/tmp/target-sleuthkit.XXXXXX")"
bash ../rust-fs-core/scripts/tier.sh sleuthkit-build sleuthkit-build 100 10000 -- chore build
bash ../rust-fs-core/scripts/tier.sh --refuse-skips --refuse-ignored \
  sleuthkit-verdict sleuthkit-verdict 200 6000 -- bash docs/sleuthkit-probe.sh
```

The complete successful negative diagnostic measured 185 lines / 4,540 bytes
on November 13, 2026; the 200-line / 6,000-byte budget retains the matrix and
every CLI report while leaving room for path and package-manager diagnostics.

The probe is one session on the host's shared guest, and nothing has to be
set for it. The guest stays up for the next run when the probe ends. When
finished, `chore vm:down` stops it; the harness refuses to stop the guest
while another run is using it.

**A successful probe means the negative verdict was reproduced**, not that
Sleuth Kit passed an interoperability gate. This deliberately opt-in diagnostic
does not add Sleuth Kit to the regular oracle tiers. A different result requires
a fresh capability review before any acceptance claim or recurring gate.

The initial evaluation logs are `tmp/logs/issue-178-sleuthkit-install.log`,
`tmp/logs/issue-178-chain-probe.log` and
`tmp/logs/issue-178-bounded-probe.log` in worktree
`cth-cb50c5cf-b932344cc4bc9daa`. The checked-in reproducer's complete rerun
is retained in `tmp/logs/issue-178-verdict.log` there. The reproducible matrix,
not those machine-local paths, is the lasting evidence.

[fsstat-finding]: https://github.com/antimatter-studios/rust-fs-exfat/issues/201
