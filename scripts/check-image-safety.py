from pathlib import Path
import hashlib
import re
import sys


def refuse(reason):
    raise ValueError(reason)


def check(source):
    if re.search(r"(?<![\w])(?:sync|purge|pkill|eval|exec|alias)(?![\w])|`|os\.(?:system|popen)|subprocess", source):
        refuse("global flushes, process killing and indirect command execution are forbidden")
    if not re.search(r"diskutil|hdiutil|newfs_exfat|fsck_exfat|\$device|/dev/", source):
        return
    for name, expected in [
        ("cleanup", "760ec1fb1b60d50b2f11f7ce4e24695109e781eb03425293dd4fdbf32312ef2d"),
        ("accept_receipt", "4f2972917a8a64e8b892dc73e7a44f550d307d02186edb31fba25c3e5f6757fe"),
    ]:
        blocks = list(re.finditer(r"(?ms)^" + name + r"\(\) \{\n.*?^\}\n", source))
        if len(blocks) != 1 or hashlib.sha256(blocks[0].group().encode()).hexdigest() != expected:
            refuse("the reviewed attachment-receipt and cleanup bindings must remain intact")
        source = source[:blocks[0].start()] + source[blocks[0].end():]
    if '/dev/' in source:
        refuse("literal device paths outside the reviewed receipt parser are forbidden")
    lines = []
    delimiter = None
    for line in source.splitlines():
        if delimiter:
            if line == delimiter:
                delimiter = None
            continue
        lines.append(line.strip())
        heredoc = re.search(r"<<\s*(['\"]?)([A-Za-z_]\w*)\1", line)
        if heredoc:
            delimiter = heredoc[2]
    if delimiter:
        refuse("unterminated source heredoc")
    initialization = 'directory="$(mktemp -d "$PWD/tmp/oracle.XXXXXX")"'
    image_binding = 'image="$directory/reference-$sector.dmg"'
    tools = 'for tool in /usr/bin/hdiutil /sbin/newfs_exfat /sbin/fsck_exfat /usr/bin/python3 /usr/sbin/diskutil; do'
    query = '/usr/sbin/diskutil info -plist "$device" > "$directory/info-$sector.plist"'
    create = '/usr/bin/hdiutil create -size 64m -layout NONE "$image"'
    detach = '/usr/bin/hdiutil detach "$device"'
    attachments = {
        '/usr/bin/hdiutil attach -blocksize "$sector" -nomount -nobrowse -plist "$image" > "$directory/attach.plist"',
        '/usr/bin/hdiutil attach -blocksize "$sector" -nobrowse -mountpoint "$directory/mount-$sector" -plist "$image" > "$directory/attach.plist"',
        '/usr/bin/hdiutil attach -blocksize "$sector" -readonly -nobrowse -mountpoint "$directory/mount-$sector" -plist "$image" > "$directory/attach.plist"',
    }
    device_commands = {
        query,
        '/sbin/newfs_exfat -R -S "$sector" -b 4096 -v M1ORACLE "$device" | tee "$directory/format-$sector.txt"',
        '/sbin/fsck_exfat -n "$device"',
    }
    if lines.count(initialization) != 1 or lines.count('trap cleanup EXIT') != 1 or lines.count(image_binding) != 1:
        refuse("images must be bound to this run's fresh private temporary directory")
    active = False
    created = False
    receipt_pending = False
    image_bound = False
    for line in lines:
        if not line:
            continue
        if receipt_pending:
            if line != 'accept_receipt':
                refuse("every attach must be followed immediately by its own receipt binding")
            active = True
            receipt_pending = False
            continue
        if line == 'accept_receipt':
            refuse("a receipt cannot be reused without a fresh image attachment")
        if line == initialization:
            continue
        if line == image_binding:
            if active:
                refuse("an attached image cannot be rebound")
            image_bound = True
            created = False
            continue
        if line == create:
            if not image_bound or active:
                refuse("create must refer only to the freshly bound image")
            created = True
            continue
        if line in attachments:
            if not created or active:
                refuse("attach requires this run's created, detached image")
            receipt_pending = True
            continue
        if line == detach or line in device_commands:
            if not active:
                refuse("device operations require this run's live attach receipt")
            if line == detach:
                active = False
            continue
        if line == "device=''":
            if active:
                refuse("a live attachment cannot lose its ownership binding")
            continue
        if re.search(r"diskutil|hdiutil|newfs_exfat|fsck_exfat", line) and line != tools:
            refuse("only the reviewed image commands and exact receipt-owned info query are permitted")
        if re.search(r"\$device|\b(?:device|image|directory)\s*\+?=|\b(?:read|for|declare|typeset|export|local|unset)\b.*\b(?:device|image|directory)\b|printf\s+-v\s+(?:device|image|directory)\b", line):
            refuse("a device or image binding outside its reviewed ownership boundary is forbidden")
    if active or receipt_pending:
        refuse("every attachment must retain its receipt through detachment")


try:
    target = Path(sys.argv[1])
    if target.is_symlink() or not target.is_file():
        refuse("a regular source file is required")
    check(target.read_text(encoding="utf-8"))
except (OSError, UnicodeError, ValueError, IndexError) as error:
    print(f"image-safety: {error}", file=sys.stderr)
    sys.exit(1)
print("image-safety: image-only ownership and host-tool restrictions passed")
