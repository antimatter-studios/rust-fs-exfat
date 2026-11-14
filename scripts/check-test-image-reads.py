#!/usr/bin/env python3
import collections
import pathlib
import re
import sys


def check(root):
    allow = {}
    for number, line in enumerate((root / "tests/image-reads.allow").read_text().splitlines(), 1):
        if not line or line.startswith("#"):
            continue
        path, pattern, count, reason = line.split("\t")
        key = (path, pattern)
        if key in allow or not reason.strip() or int(count) < 1:
            raise ValueError(f"invalid allow-list entry {number}")
        allow[key] = (int(count), reason)
    pattern = re.compile(
        r"\b(?:std\s*::\s*)?fs\s*::\s*(?:read|read_to_string)\s*\("
        r"|\.\s*(?:read_to_end|read_to_string)\s*\("
        r"|\buse\s+(?:std\s*::\s*)?fs\s*::\s*(?:read\b|\{[^}]*\bread\b)"
    )
    found = collections.Counter()
    problems = []
    tests = root / "tests"
    paths = sorted(tests.rglob("*.rs"))
    if not paths:
        raise ValueError("no Rust test sources found")
    for path in paths:
        source = path.read_text()
        name = path.relative_to(root).as_posix()
        for match in pattern.finditer(source):
            line = source.count("\n", 0, match.start()) + 1
            text = source.splitlines()[line - 1].strip()
            key = (name, text)
            found[key] += 1
            if key not in allow or found[key] > allow[key][0]:
                problems.append(f"{name}:{line}: unbounded read: {text}")
    for key, (count, _) in allow.items():
        if found[key] != count:
            problems.append(f"stale allow-list entry: {key[0]}: {key[1]}")
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(check(pathlib.Path(sys.argv[1])))
    except (OSError, ValueError) as error:
        print(f"test image reads: cannot check: {error}", file=sys.stderr)
        sys.exit(1)
