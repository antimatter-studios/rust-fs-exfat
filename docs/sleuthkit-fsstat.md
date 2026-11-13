# Sleuth Kit fsstat: empty-label control limitation

Issue [#201](https://github.com/antimatter-studios/rust-fs-exfat/issues/201)
was investigated on 2026-11-13 using black-box commands only, through
`tests/guest_support` in the fs-linux-test-harness guest. No Sleuth Kit
implementation source was read, linked, copied or vendored. Its installed
copyright metadata lists CPL/IPL and other licenses; it is not an acceptable
implementation input for this crate.

## Pinned environment

- Debian 12 arm64, Linux `6.1.0-42-cloud-arm64`.
- `sleuthkit` and `libtsk19`: `4.11.1+dfsg-1+b1`; `fsstat -V` reports 4.11.1.
- `exfatprogs`: `1.2.0-1+deb12u1`.
- `/usr/bin/fsstat` SHA-256:
  `61e59dc09705499d185e2109d701cf02903c0bfa7acb301b8905ba16ab483f08`.
- `/usr/lib/aarch64-linux-gnu/libtsk.so.19` SHA-256:
  `34b6171ad881bf1c01cfd53fdb1966919994acb5fd1ce93357e3ed360f8e69d6`.

The binary package can be installed with
`apt-get install sleuthkit=4.11.1+dfsg-1+b1` **inside the guest**, not on the
host. Check both package versions and binary hashes when comparing a run.
This probe does not change the standard guest provisioning or add a test-tier
dependency.

## Bounded result

The fresh, empty 64 MiB image created by guest `mkfs.exfat` has one FAT,
512-byte sectors and 4096-byte clusters. `fsck.exfat -n` accepts it. With an
empty label, line-buffered `fsstat -f exfat` prints the file-system heading
and volume serial, then fails to finish within 15 seconds (status 124).

Changing only the label through guest `exfatlabel` gives this reversible result
on that same image, with its serial and geometry unchanged:

| Label state | fsstat status | Observed elapsed | fsck.exfat -n | fls status |
|---|---|---|---|---|
| Initially empty | 124 | 15 seconds | Clean | 0 |
| `CONTROL` | 0 | Under one second | Clean | 0 |
| Cleared back to empty | 124 | 15 seconds | Clean | 0 |
| Restored to `CONTROL` | 0 | Under one second | Clean | 0 |

The first root entry is type `0x83`, character count zero when empty; with
`CONTROL` its count is seven and its payload is that label in UTF-16LE.
[The retained transcript](validation/sleuthkit-fsstat-4.11.1.log) includes those
entry bytes, both complete successful reports, partial timed-out reports,
the exact commands, their statuses, the binary hashes and all checker/listing
output, with trailing spaces trimmed for repository whitespace checks. The
labeled reports reach the final `CONTENT INFORMATION` fields:
sector size 512, cluster size 4096 and cluster range 2–15873.

Separate fresh, unlabeled 4 MiB and 16 MiB controls also returned 124 at
15 seconds, while their `fls` calls returned 0. Fresh 64 MiB controls labeled
`CONTROL` or `A` at format time (`mkfs.exfat -L LABEL`) completed within the
same deadline. Increasing the unlabeled 64 MiB control's cluster size from
4096 to 131072 bytes still returned 124 at an eight-second deadline.
`strace -c` reported the same 559 total system calls for unlabeled 4 MiB,
64 MiB and 64 MiB/128 KiB-cluster controls at that deadline. These bounded
observations do not diagnose an internal loop or prove a permanent hang;
they do rule out image size alone as the explanation for these controls.

## Reproduction

Run the following script **in the guest through `tests/guest_support`** after
installing the pinned binary package. It creates only its own guest-disk image,
keeps expected deadline failures as recorded statuses, and removes the image
on exit. `stdbuf` exposes partial output before `timeout` terminates the tool;
an unbuffered report file can otherwise remain empty on timeout.

```sh
set -euo pipefail
scratch="$(mktemp -d /var/tmp/tsk-fsstat.XXXXXX)"
trap 'rm -rf "$scratch"' EXIT
image="$scratch/control.img"
truncate -s 64M "$image"
mkfs.exfat "$image"
for state in empty-first labeled empty-again labeled-again; do
    case "$state" in
        labeled|labeled-again) exfatlabel "$image" CONTROL ;;
        empty-again) exfatlabel "$image" '' ;;
    esac
    echo "STATE $state"
    fsck.exfat -n "$image"
    status=0
    timeout -k 2 15 stdbuf -oL -eL fsstat -f exfat "$image" || status=$?
    echo "fsstat status=$status"
    status=0
    timeout -k 2 15 fls -f exfat "$image" || status=$?
    echo "fls status=$status"
done
```

For an ad-hoc host runner, save that script under `tmp/` and compile this Rust
bridge as `tmp/fsstat-probe.rs`, using the repository's pinned toolchain:

```rust
#[path = "../tests/guest_support/mod.rs"]
mod guest_support;

fn main() {
    let path = std::env::args().nth(1).expect("guest script path");
    let script = std::fs::read_to_string(path).expect("read guest script");
    let result = guest_support::guest("sleuthkit", "bounded fsstat control", &script);
    print!("{}", result.stdout);
    eprint!("{}", result.stderr);
    std::process::exit(result.status);
}
```

```sh
CARGO_MANIFEST_DIR="$PWD" rustc --edition=2021 tmp/fsstat-probe.rs -o tmp/fsstat-probe
bash ../rust-fs-core/scripts/tier.sh fsstat-control fsstat-control 300 20000 -- \
    ../fs-linux-test-harness/scripts/vm.sh session tmp/fsstat-probe tmp/fsstat-control.sh
```

Use the machine's configured harness environment (including `FLTH_SHARED_ROOT`
when using its shared guest). The 300-line/20000-byte ad-hoc budget covers the
measured 163-line/5317-byte recorded probe and additional startup diagnostics;
it is not a new required tier. The bridge's exit status describes execution of
the probe script, not acceptance of every image by `fsstat`: inspect each
recorded command status.

## Interpretation

For a fresh positive control with this pinned build, use a **nonempty label at
format time** and retain the full report and exit status. Do not relabel an
evidence image just to obtain a report: labeling changes evidence. An empty-label
timeout is a limitation of this oracle setup, not a rejection by the crate or
proof that a volume is corrupt. A successful `fls` listing alone is not a
successful `fsstat` report. No result here establishes ActiveFat or two-FAT
support, and no driver behavior, feature claim or required oracle is changed.
