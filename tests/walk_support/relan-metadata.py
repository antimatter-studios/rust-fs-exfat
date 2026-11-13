"""Read the pinned relan tools, never their implementation or disk structures."""

import json
import os
import subprocess
import sys

FLAGS = {"Read-only": 1, "Hidden": 2, "System": 4, "Volume": 8,
         "Directory": 16, "Archive": 32}


def parse_attributes(text):
    seen = {}
    for line in text.splitlines():
        name, value = (part.strip() for part in line.split(":", 1))
        if name not in FLAGS or name in seen or value not in ("yes", "no"):
            raise ValueError(f"unexpected exfatattrib evidence: {line!r}")
        seen[name] = value
    if set(seen) != set(FLAGS):
        raise ValueError(f"missing exfatattrib evidence: {set(FLAGS) - set(seen)}")
    return sum(FLAGS[name] for name, value in seen.items() if value == "yes")


def metadata(device, listing, limit, mount, full):
    paths = []
    with open(listing, encoding="utf-8") as source:
        for line in source:
            if not line.startswith("#"):
                paths.append(line.rstrip("\n").split("\t", 6)[6])
    attributes = {}
    unavailable = 0
    for path in paths[:limit]:
        text = subprocess.check_output(
            ["/opt/relan-exfat/sbin/exfatattrib", "-d", device, "/" + path], text=True
        )
        attributes[path] = parse_attributes(text)
        if full:
            birth = subprocess.check_output(
                ["stat", "--format=%w", "--", os.path.join(mount, path)], text=True
            ).strip()
            if birth != "-":
                raise RuntimeError(f"relan now reports a birth time for {path}: {birth}; compare it")
            unavailable += 1
    label = subprocess.check_output(
        ["/opt/relan-exfat/sbin/exfatlabel", device], text=True
    ).rstrip("\n")
    return {"attributes": attributes, "label": label, "birth_unavailable": unavailable}


if __name__ == "__main__":
    json.dump(metadata(sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4],
                       sys.argv[5] == "all"), sys.stdout, ensure_ascii=False, sort_keys=True)
