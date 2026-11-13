import datetime
import json
import os
import subprocess
import sys


def birth_times(mount):
    epoch = datetime.datetime(1970, 1, 1, tzinfo=datetime.UTC)
    result = {}
    for directory, directories, files in os.walk(mount):
        for name in directories + files:
            path = os.path.join(directory, name)
            text = subprocess.check_output(
                ["stat", "--format=%w", "--", path], text=True
            ).strip()
            if text == "-":
                raise RuntimeError(f"the kernel did not report a birth time for {path}")
            timestamp = datetime.datetime.fromisoformat(text)
            delta = timestamp - epoch
            result[os.path.relpath(path, mount)] = (
                delta.days * 86_400_000
                + delta.seconds * 1000
                + delta.microseconds // 1000
            )
    return result


if __name__ == "__main__":
    json.dump(birth_times(sys.argv[1]), sys.stdout, ensure_ascii=False, sort_keys=True)
