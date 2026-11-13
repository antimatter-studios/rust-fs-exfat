"""Exercise mounted relan/exfat using filesystem calls and a separate byte model.

This script runs only in the harness guest. No exFAT structures are decoded.
Each phase returns the complete expected tree, including the kernel's seed.
"""

import hashlib
import json
import os
from pathlib import Path
import sys

MTIME_NS = 1_234_567_890_780_000_000
ATIME_NS = 1_600_000_000_120_000_000
LONG = "a long name that needs many File Name entries " * 5
LONG = "Relan/" + LONG.rstrip()
UNICODE = "Relan/Ünïcödé 目录"


def pattern(seed, length):
    block = hashlib.sha256(str(seed).encode()).digest()
    return (block * ((length + 31) // 32))[:length]


def many_count(cluster):
    # Five 32-byte entries per long name. Grow the directory at both geometries.
    return max(200, cluster // 160 + 1)


def many_name(index):
    return f"Relan/Many/entry {index:03} with a name of some length.dat"


def model(cluster, phase):
    directories = {"keep", "Relan", "Relan/Deep", "Relan/Many", UNICODE,
                   "Relan/remove-empty", "Relan/empty-source", "Relan/empty-target"}
    files = {
        "keep/sentinel.bin": bytes(range(256)) * (cluster // 256 * 3) + b"preserve this tail",
        "Relan/ascii.txt": b"plain ASCII contents\n",
        "Relan/case.txt": b"case-preserved contents",
        "Relan/empty": b"",
        "Relan/zero.bin": pattern(11, 20000),
        "Relan/Deep/large.bin": pattern(7, 3 * 1024 * 1024 + 123),
        "Relan/Deep/remove.bin": b"removed later",
        "Relan/replaced.bin": b"second contents",
        "Relan/gap.bin": b"head" + bytes(299996) + b"tail",
        LONG: b"long",
    }
    for name in ("grüße-😀.txt", "日本語のファイル名.txt", "ÀÉÎÕÜ àéîõü.bin"):
        files[f"{UNICODE}/{name}"] = name.encode()
    for index in range(many_count(cluster)):
        files[many_name(index)] = pattern(index, index * 37 if index < 200 else 0)
    for index, name in enumerate(("a", "b")):
        files[f"Relan/fragment-{name}.bin"] = b"".join(
            pattern(round_ * 2 + index, cluster + 1) for round_ in range(24)
        )
    if phase != "create":
        files["Relan/Deep/large.bin"] = files["Relan/Deep/large.bin"][:100001]
        files["Relan/fragment-a.bin"] = files["Relan/fragment-a.bin"][:7000]
        files["Relan/gap.bin"] += bytes(50000 - 3)
        files["Relan/empty"] = bytes(50001)
        files["Relan/zero.bin"] = b""
        for name in ("Relan/fragment-b.bin", "Relan/Deep/remove.bin", f"{UNICODE}/日本語のファイル名.txt"):
            del files[name]
        directories.remove("Relan/remove-empty")
    if phase in ("rename", "empty-label"):
        files["Relan/CASE.TXT"] = files.pop("Relan/case.txt")
        files["Relan/replaced.bin"] = files.pop("Relan/ascii.txt")
        directories.remove("Relan/empty-source")
        for old, new in (("Relan/Deep", "keep/Moved directory"),
                         (UNICODE, UNICODE + " renamed")):
            directories = {new + p[len(old):] if p == old or p.startswith(old + "/") else p
                           for p in directories}
            files = {new + p[len(old):] if p.startswith(old + "/") else p: data
                     for p, data in files.items()}
    rows = {path: {"directory": True, "size": None, "sha256": None, "attributes": 16}
            for path in directories}
    rows.update({path: {"directory": False, "size": len(data),
                       "sha256": hashlib.sha256(data).hexdigest(), "attributes": 32}
                 for path, data in files.items()})
    if phase in ("rename", "empty-label"):
        for path, flags in (("Relan/CASE.TXT", 0x27), ("Relan/empty", 0),
                            ("keep/Moved directory", 0x16), ("Relan/replaced.bin", 0x22)):
            rows[path]["attributes"] = flags
    return {"entries": rows, "label": "" if phase == "empty-label" else
            ("RELAN-Δ" if phase == "rename" else "ALLOCATION"),
            "modified_ms": MTIME_NS // 1_000_000_000 * 1000,
            "accessed_ms": ATIME_NS // 1_000_000_000 * 1000}


def mutate(mount, cluster, phase):
    root = Path(mount)
    if phase == "create":
        for name in ("Relan", "Relan/Deep", "Relan/Many", UNICODE,
                     "Relan/remove-empty", "Relan/empty-source", "Relan/empty-target"):
            (root / name).mkdir()
        for name, data in (("ascii.txt", b"plain ASCII contents\n"),
                           ("case.txt", b"case-preserved contents"), ("empty", b""),
                           ("zero.bin", pattern(11, 20000))):
            (root / "Relan" / name).write_bytes(data)
        (root / "Relan/Deep/large.bin").write_bytes(pattern(7, 3 * 1024 * 1024 + 123))
        (root / "Relan/Deep/remove.bin").write_bytes(b"removed later")
        for name in ("grüße-😀.txt", "日本語のファイル名.txt", "ÀÉÎÕÜ àéîõü.bin"):
            (root / UNICODE / name).write_bytes(name.encode())
        (root / LONG).write_bytes(b"long")
        for index in range(many_count(cluster)):
            (root / many_name(index)).write_bytes(pattern(index, index * 37 if index < 200 else 0))
        handles = [(root / f"Relan/fragment-{name}.bin").open("wb", buffering=0) for name in ("a", "b")]
        try:
            for round_ in range(24):
                for index, handle in enumerate(handles):
                    handle.write(pattern(round_ * 2 + index, cluster + 1))
        finally:
            for handle in handles:
                handle.close()
        with (root / "Relan/gap.bin").open("wb") as target:
            target.write(b"head")
            target.seek(300000)
            target.write(b"tail")
        (root / "Relan/replaced.bin").write_bytes(pattern(9, 100000))
        (root / "Relan/replaced.bin").write_bytes(b"second contents")
    elif phase == "truncate":
        for name, length in (("Deep/large.bin", 100001), ("fragment-a.bin", 7000),
                             ("gap.bin", 350001), ("empty", 50001), ("zero.bin", 0)):
            os.truncate(root / "Relan" / name, length)
        for name in ("Relan/fragment-b.bin", "Relan/Deep/remove.bin", f"{UNICODE}/日本語のファイル名.txt"):
            (root / name).unlink()
        (root / "Relan/remove-empty").rmdir()
    elif phase == "rename":
        for old, new in (("Relan/case.txt", "Relan/CASE.TXT"),
                         ("Relan/ascii.txt", "Relan/replaced.bin"),
                         ("Relan/empty-source", "Relan/empty-target"),
                         ("Relan/Deep", "keep/Moved directory"),
                         (UNICODE, UNICODE + " renamed")):
            os.replace(root / old, root / new)
    else:
        raise ValueError(phase)
    # Freeze all supported times, including seed and directories, after mutation.
    # The pinned FUSE interface drops these fractions. The Rust checks require
    # exactly whole seconds, so a change in that evidence is visible.
    for directory, directories, files in os.walk(root, topdown=False):
        for name in files + directories:
            os.utime(Path(directory) / name, ns=(ATIME_NS, MTIME_NS))


if __name__ == "__main__":
    mount, cluster, phase, output = sys.argv[1:]
    cluster = int(cluster)
    if phase != "empty-label":
        mutate(mount, cluster, phase)
    with open(output, "w", encoding="utf-8") as target:
        json.dump(model(cluster, phase), target, ensure_ascii=False, sort_keys=True)
