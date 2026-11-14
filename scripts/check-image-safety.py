from pathlib import Path
import hashlib
import re
import sys


def refuse(reason):
    raise ValueError(reason)


def check_rust(source):
    # These sources create private images, derive nodes from each live receipt,
    # detach ordinarily within a deadline, and retain attached images on failure.
    # Pin both sides of that ownership boundary: changes require a safety review,
    # including changes in the fixture producer and its failure cleanup.
    reviewed = {
        "f614533157bfa4def21b4139670159a4709d33f33fd94372e6c3d1305d73008b",  # tests/oracle/scale.rs
        "a80532b0620d3661839bc50e5b26b0bfd439fd63141155d0d6abdb5f081b6207",  # tests/scale_support/mod.rs
    }
    if hashlib.sha256(source.encode()).hexdigest() not in reviewed:
        refuse("the reviewed Rust private-image, attachment-receipt and ordinary-detach lifecycle must remain intact")


def check(source):
    if re.search(r"(?<![\w])(?:sync|purge|pkill|eval|exec|alias)(?![\w])|`|os\.(?:system|popen)|subprocess", source):
        refuse("global flushes, process killing and indirect command execution are forbidden")
    if not re.search(r"diskutil|hdiutil|newfs_exfat|fsck_exfat|\$device|/dev/", source):
        return
    for name, expected in [
        ("cleanup", "4c913177349d7314b04d353248a2a4ab3f0487785b89fb811ebd42df628ada41"),
        ("detach_receipt", "e0a5e36eadea3d120108bc14e980e49326968c21f69387e2430b3c445bc71383"),
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
    fixture_binding = 'image="$directory/reencoded-fixtures-$sector.dmg"'
    windows_binding = 'image="$directory/windows-$sector.dmg"'
    windows_producer = '/usr/bin/python3 scripts/build-windows-oracle.py "$image" "$directory/reference-$sector.dmg" "$sector"'
    windows_attach = '/usr/bin/hdiutil attach -blocksize "$sector" -readonly -nobrowse -mountpoint "$directory/windows-mount-$sector" -plist "$image" > "$directory/attach.plist"'
    fixture_regular = '[ -f "$image" ] && [ ! -L "$image" ]'
    fixture_test = 'EXFAT_ORACLE_DIR="$directory" cargo test --locked --features cli,test-support --test macos_boot --test macos_metadata --test macos_directory --test macos_file --test macos_path --test macos_interfaces --test macos_fuzz_seeds -- --nocapture'
    fixture_attach = '/usr/bin/hdiutil attach -blocksize "$sector" -readonly -nomount -plist "$image" > "$directory/attach.plist"'
    written_binding = 'image="$directory/written-$sector.dmg"'
    written_producer = 'EXFAT_ORACLE_DIR="$directory" EXFAT_ORACLE_SECTOR="$sector" cargo test --locked --features cli,test-support --test macos_write -- --nocapture'
    checkpoint_kind = "case \"$kind\" in ''|*[!a-z-]*) exit 1 ;; esac"
    checkpoint_binding = 'image="$directory/checkpoint-$sector-$kind.dmg"'
    checkpoint_check = 'if /sbin/fsck_exfat -n "$device" > "$directory/checkpoint-$sector-$kind.txt" 2>&1; then'
    written_attach = '/usr/bin/hdiutil attach -blocksize "$sector" -readonly -nobrowse -mountpoint "$directory/written-mount-$sector" -plist "$image" > "$directory/attach.plist"'
    populated_copy = 'cp "$image" "$directory/populated-$sector.dmg"'
    raw_binding = 'image="$directory/raw-written-512.dmg"'
    raw_copy = 'cp "$directory/populated-512.dmg" "$image"'
    raw_attach = '/usr/bin/hdiutil attach -blocksize 4096 -nomount -nobrowse -plist "$image" > "$directory/attach.plist"'
    raw_producer = 'EXFAT_ORACLE_DEVICE="$device" EXFAT_ORACLE_DIR="$directory" cargo test --locked --features cli,test-support --test macos_raw_write -- --nocapture'
    raw_check_attach = '/usr/bin/hdiutil attach -blocksize 512 -readonly -nomount -plist "$image" > "$directory/attach.plist"'
    raw_mount_attach = '/usr/bin/hdiutil attach -blocksize 512 -readonly -nobrowse -mountpoint "$directory/raw-written-mount" -plist "$image" > "$directory/attach.plist"'
    raw_attachments = (raw_attach, raw_check_attach, raw_mount_attach)
    tools = 'for tool in /usr/bin/hdiutil /sbin/newfs_exfat /sbin/fsck_exfat /usr/bin/python3 /usr/sbin/diskutil /usr/sbin/lsof; do'
    query = '/usr/sbin/diskutil info -plist "$device" > "$directory/info-$sector.plist"'
    notices = 'export com_apple_hdiutil_nodeprecatedwarning=1'
    create = '/usr/bin/hdiutil create -size 64m -layout NONE "$image"'
    detach = 'detach_receipt'
    attachments = {
        fixture_attach,
        windows_attach,
        written_attach,
        *raw_attachments,
        '/usr/bin/hdiutil attach -blocksize "$sector" -nomount -nobrowse -plist "$image" > "$directory/attach.plist"',
        '/usr/bin/hdiutil attach -blocksize "$sector" -nobrowse -mountpoint "$directory/mount-$sector" -plist "$image" > "$directory/attach.plist"',
        '/usr/bin/hdiutil attach -blocksize "$sector" -readonly -nobrowse -mountpoint "$directory/mount-$sector" -plist "$image" > "$directory/attach.plist"',
    }
    device_commands = {
        query,
        'EXFAT_ORACLE_DEVICE="$device" EXFAT_ORACLE_SECTOR="$sector" EXFAT_ORACLE_DIR="$directory" cargo test --locked --features cli,test-support --test macos_device -- --nocapture',
        '/sbin/newfs_exfat -R -S "$sector" -b 4096 -v M1ORACLE "$device" | tee "$directory/format-$sector.txt"',
        '/sbin/fsck_exfat -n "$device"',
        '/sbin/fsck_exfat -n "$device" | tee "$directory/written-fsck-$sector.txt"',
        checkpoint_check,
    }
    if lines.count(initialization) != 1 or lines.count('trap cleanup EXIT') != 1 or lines.count(image_binding) != 1:
        refuse("images must be bound to this run's fresh private temporary directory")
    if lines.count(fixture_binding) != 1 or lines.count(fixture_test) != 1:
        refuse("reencoded fixtures require the reviewed test producer and private image binding")
    if lines.count(windows_binding) != 1 or lines.count(windows_producer) != 1:
        refuse("Windows-layout fixtures require the reviewed producer and private image binding")
    if lines.count(written_binding) != 1 or lines.count(written_producer) != 1:
        refuse("written images require the reviewed producer and private image binding")
    if lines.count(checkpoint_binding) != 1 or lines.count(checkpoint_kind) != 1:
        refuse("write checkpoints require the reviewed kind check and private image binding")
    if any(lines.count(line) != 1 for line in (populated_copy, raw_binding, raw_copy, raw_attach, raw_producer)):
        refuse("the larger-block raw write requires its reviewed copy, attachment, producer and private image binding")
    active = False
    created = False
    receipt_pending = False
    image_bound = False
    fixture_image = False
    fixtures_ready = False
    windows_image = False
    windows_ready = False
    written_image = False
    written_ready = False
    kind_checked = False
    checkpoint_image = False
    reference_image = False
    populated_ready = False
    raw_image = False
    raw_copied = False
    raw_attached = False
    raw_receipt = False
    raw_written = False
    for line in lines:
        if not line:
            continue
        if receipt_pending:
            if line != 'accept_receipt':
                refuse("every attach must be followed immediately by its own receipt binding")
            active = True
            receipt_pending = False
            raw_receipt = raw_attached and not raw_written
            continue
        if line == 'accept_receipt':
            refuse("a receipt cannot be reused without a fresh image attachment")
        if line == initialization:
            continue
        if line == windows_binding:
            if active:
                refuse("an attached image cannot be rebound")
            checkpoint_image = False
            image_bound = True
            reference_image = False
            raw_image = False
            fixture_image = False
            windows_image = True
            windows_ready = False
            written_image = False
            created = False
            continue
        if line == written_binding:
            if active:
                refuse("an attached image cannot be rebound")
            checkpoint_image = False
            image_bound = True
            reference_image = False
            raw_image = False
            fixture_image = False
            windows_image = False
            written_image = True
            written_ready = False
            created = False
            continue
        if line == checkpoint_kind:
            kind_checked = True
            continue
        if line == checkpoint_binding:
            if active:
                refuse("an attached image cannot be rebound")
            if not (written_ready and kind_checked):
                refuse("write checkpoints require this run's writer producer and a checked kind name")
            kind_checked = False
            image_bound = True
            reference_image = False
            raw_image = False
            fixture_image = False
            windows_image = False
            written_image = False
            checkpoint_image = True
            created = False
            continue
        if line == written_producer:
            if not written_image or active or created:
                refuse("the writer may change only its detached private image binding")
            written_ready = True
            continue
        if line == windows_producer:
            if not windows_image or active or created:
                refuse("Windows-layout generation requires its detached private image binding")
            windows_ready = True
            continue
        if line == fixture_test:
            if active:
                refuse("fixture generation requires detached reference images")
            fixtures_ready = True
            continue
        if line == fixture_binding:
            if active or not fixtures_ready:
                refuse("fixture attachment requires this run's completed fixture producer")
            checkpoint_image = False
            image_bound = True
            reference_image = False
            raw_image = False
            fixture_image = True
            windows_image = False
            written_image = False
            created = False
            continue
        if line == fixture_regular:
            if not (fixture_image or checkpoint_image or (windows_image and windows_ready) or (written_image and written_ready)) or active:
                refuse("fixture validation requires its detached private image binding")
            created = True
            continue
        if line == image_binding:
            if active:
                refuse("an attached image cannot be rebound")
            checkpoint_image = False
            image_bound = True
            reference_image = True
            raw_image = False
            fixture_image = False
            windows_image = False
            written_image = False
            created = False
            continue
        if line == populated_copy:
            if not reference_image or not created or active:
                refuse("the populated copy requires this run's detached reference image")
            populated_ready = True
            continue
        if line == raw_binding:
            if active:
                refuse("an attached image cannot be rebound")
            checkpoint_image = False
            image_bound = True
            reference_image = False
            raw_image = True
            fixture_image = False
            windows_image = False
            written_image = False
            created = False
            raw_copied = False
            raw_attached = False
            raw_written = False
            continue
        if line == raw_copy:
            if not raw_image or not populated_ready or active or created:
                refuse("the raw-node copy requires this run's populated volume and its own private image binding")
            created = True
            raw_copied = True
            continue
        if line == raw_producer:
            if not (raw_image and active and raw_receipt):
                refuse("the raw-node writer requires the live receipt of its own read-write larger-block attachment")
            raw_written = True
            continue
        if line == create:
            if not image_bound or active:
                refuse("create must refer only to the freshly bound image")
            created = True
            continue
        if line in attachments:
            if not created or active:
                refuse("attach requires this run's created, detached image")
            if fixture_image and line != fixture_attach:
                refuse("reencoded fixture images must be attached read-only")
            if windows_image != (line == windows_attach):
                refuse("Windows-layout fixtures require their own read-only attachment")
            if written_image and line not in (fixture_attach, written_attach):
                refuse("written images must be attached read-only")
            if checkpoint_image and line != fixture_attach:
                refuse("write checkpoints must be attached read-only without a mount")
            if not written_image and line == written_attach:
                refuse("the written-image mount point belongs to written images")
            if raw_image != (line in raw_attachments):
                refuse("the larger-block attachments belong to this run's raw-node copy alone")
            if line == raw_attach:
                if not raw_copied or raw_attached:
                    refuse("the read-write larger-block attachment is made once, of the fresh copy")
                raw_attached = True
            elif raw_image and not raw_written:
                refuse("the raw-node copy is checked only after its writer ran")
            receipt_pending = True
            continue
        if line == detach or line in device_commands:
            if not active:
                refuse("device operations require this run's live attach receipt")
            if line == checkpoint_check and not checkpoint_image:
                refuse("the checkpoint check belongs to write checkpoints")
            if line == detach:
                active = False
                raw_receipt = False
            continue
        if line == "device=''":
            if active:
                refuse("a live attachment cannot lose its ownership binding")
            continue
        if re.search(r"diskutil|hdiutil|newfs_exfat|fsck_exfat|\bdetach_receipt\b", line) and line not in (tools, notices):
            refuse("only the reviewed image commands and exact receipt-owned info query are permitted")
        if re.search(r"EXFAT_ORACLE_DEVICE|\$device|\b(?:device|image|directory)\s*\+?=|\b(?:read|for|declare|typeset|export|local|unset)\b.*\b(?:device|image|directory)\b|printf\s+-v\s+(?:device|image|directory)\b", line):
            refuse("a device or image binding outside its reviewed ownership boundary is forbidden")
    if active or receipt_pending:
        refuse("every attachment must retain its receipt through detachment")


try:
    target = Path(sys.argv[1])
    if target.is_symlink() or not target.is_file():
        refuse("a regular source file is required")
    source = target.read_text(encoding="utf-8")
    if target.suffix == '.rs':
        check_rust(source)
    else:
        check(source)
except (OSError, UnicodeError, ValueError, IndexError) as error:
    print(f"image-safety: {error}", file=sys.stderr)
    sys.exit(1)
print("image-safety: image-only ownership and host-tool restrictions passed")
