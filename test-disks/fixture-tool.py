#!/usr/bin/env python3
"""fixture-tool.py -- the GUEST side's helper for test-disks/guest-build-images.sh.

Runs as root inside the fs-linux-test-harness VM. It never parses exFAT
directories: where a damage needs a location, it takes it from dump.exfat
(exfatprogs) or from the kernel (FIBMAP on a file of the mounted volume), so
the record says where the oracle put a structure, not where this crate thinks
it is.

  data SEED SIZE PATH          write SIZE deterministic bytes to PATH
  bigdir DIR COUNT             create COUNT empty files in DIR, varied names
  listing MOUNT OUT [PREFIX]   write the kernel's listing of a mounted volume,
                               or of a directory in one, PREFIX before paths
  fibmap PATH                  print the byte offset on the device of PATH's
                               first block, as the kernel maps it
  damage IMAGE KIND RECORD [ARG]
                               change IMAGE in place and record every changed
                               byte range in RECORD. KIND is boot, bitmap,
                               upcase, or data (ARG: a byte offset from fibmap)

THE LISTING FORMAT (one line per entry, sorted by path, tab-separated):

  type  size  mode  mtime_ns  atime_ns  sha256  path

type is f or d; mode is the octal permission bits the kernel reports (a
read-only file shows no write bits, the volume being mounted with fmask=0 and
dmask=0); sha256 is - for a directory; path is relative to the volume root,
UTF-8, with / between components. Lines starting with # are comments.
"""

import fcntl
import hashlib
import os
import struct
import subprocess
import sys

FIBMAP = 1
FIGETBSZ = 2


def stream(seed, size):
    counter = 0
    produced = 0
    while produced < size:
        block = hashlib.sha256(f"{seed}:{counter}".encode()).digest() * 128
        take = min(len(block), size - produced)
        yield block[:take]
        produced += take
        counter += 1


def data(seed, size, path):
    with open(path, "wb") as out:
        for chunk in stream(seed, int(size)):
            out.write(chunk)


def bigdir(directory, count):
    os.makedirs(directory, exist_ok=True)
    for index in range(int(count)):
        # Name lengths from 1 to 3 File Name entries, so the sets differ in size.
        name = f"entry-{index:05d}" + "x" * (index % 37)
        os.close(os.open(os.path.join(directory, name), os.O_CREAT | os.O_WRONLY, 0o644))


def sha256_of(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        while True:
            chunk = source.read(1 << 20)
            if not chunk:
                return digest.hexdigest()
            digest.update(chunk)


def listing(mount, out, prefix=""):
    rows = []
    for directory, subdirectories, files in os.walk(mount):
        subdirectories.sort()
        for name in sorted(subdirectories) + sorted(files):
            path = os.path.join(directory, name)
            info = os.lstat(path)
            relative = prefix + os.path.relpath(path, mount)
            if "\t" in relative or "\n" in relative:
                raise SystemExit(f"fixture-tool: a name holds a tab or newline: {relative!r}")
            kind = "d" if os.path.isdir(path) else "f"
            digest = "-" if kind == "d" else sha256_of(path)
            rows.append(
                (relative, f"{kind}\t{info.st_size}\t{info.st_mode & 0o7777:o}\t"
                           f"{info.st_mtime_ns}\t{info.st_atime_ns}\t{digest}\t{relative}")
            )
    rows.sort()
    with open(out, "w", encoding="utf-8") as target:
        target.write("# type\tsize\tmode\tmtime_ns\tatime_ns\tsha256\tpath\n")
        for _, row in rows:
            target.write(row + "\n")
    print(f"[guest] listed {len(rows)} entries of {mount}")


def fibmap(path):
    with open(path, "rb") as source:
        size = struct.unpack("i", fcntl.ioctl(source, FIGETBSZ, struct.pack("i", 0)))[0]
        block = struct.unpack("i", fcntl.ioctl(source, FIBMAP, struct.pack("i", 0)))[0]
    if block <= 0:
        raise SystemExit(f"fixture-tool: the kernel maps no block for {path}")
    print(block * size)


def dump(image):
    """The fields dump.exfat prints, as a dict of label -> value."""
    out = subprocess.run(["dump.exfat", image], check=True, capture_output=True, text=True).stdout
    fields = {}
    for line in out.splitlines():
        if ":" in line:
            label, _, value = line.partition(":")
            fields[label.strip()] = value.strip()
    return fields


def damage(image, kind, record, argument=None):
    fields = dump(image)
    sector = 1 << int(fields["Sector Size Bits"])
    cluster = sector << int(fields["Sector per Cluster bits"])
    heap = int(fields["Cluster Heap Offset (sector offset)"]) * sector

    def cluster_offset(number):
        return heap + (int(number) - 2) * cluster

    changes = []
    if kind == "boot":
        # The Main Boot Sector alone. The Main Boot Region's checksum then
        # fails, and the Backup Boot Region is untouched.
        changes.append((0, b"\0" * sector, "main boot sector zeroed"))
    elif kind == "bitmap":
        start = cluster_offset(fields["Bitmap start cluster"])
        length = int(fields["Bitmap size"])
        changes.append((start, b"\0" * length, "allocation bitmap zeroed"))
    elif kind == "upcase":
        start = cluster_offset(fields["Upcase table start cluster"])
        with open(image, "rb") as source:
            source.seek(start + 200)
            byte = source.read(1)
        changes.append((start + 200, bytes([byte[0] ^ 0xFF]), "one up-case table byte inverted"))
    elif kind == "data":
        offset = int(argument)
        changes.append((offset, b"\xa5" * 512, "first 512 bytes of a file's data overwritten"))
    else:
        raise SystemExit(f"fixture-tool: unknown damage {kind}")

    with open(image, "r+b") as target, open(record, "w", encoding="utf-8") as out:
        out.write("# offset\tlength\tsha256_before\tsha256_after\twhat\n")
        for offset, replacement, what in changes:
            target.seek(offset)
            before = target.read(len(replacement))
            target.seek(offset)
            target.write(replacement)
            out.write(
                f"{offset}\t{len(replacement)}\t{hashlib.sha256(before).hexdigest()}\t"
                f"{hashlib.sha256(replacement).hexdigest()}\t{what}\n"
            )


def main(argv):
    if not argv:
        raise SystemExit(__doc__)
    command, arguments = argv[0], argv[1:]
    handlers = {"data": data, "bigdir": bigdir, "listing": listing, "fibmap": fibmap, "damage": damage}
    if command not in handlers:
        raise SystemExit(__doc__)
    handlers[command](*arguments)


if __name__ == "__main__":
    main(sys.argv[1:])
