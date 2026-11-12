# mkfs.exfat: formats a new image file, as installed, and the installed
# checker and reader accept it. The only tool that writes, and only to its
# target: a dry run and a refused layout write nothing.
source "$(dirname "$0")/lib.sh"

img="$SANDBOX/new.img"
mkfs.exfat --size 8M -L CLI --serial 1234abcd "$img" >"$SANDBOX/mkfs.json" 2>"$SANDBOX/mkfs.err"
check "mkfs.exfat exits 0 ($(cat "$SANDBOX/mkfs.err"))" test $? -eq 0
jq_check "mkfs.exfat reports the volume it read back" \
    '.dry_run == false and .label == "CLI" and .volume_serial == "1234abcd" and .cluster_size == 4096' "$SANDBOX/mkfs.json"
check "the image is 8 MiB" test "$(wc -c <"$img" | tr -d ' ')" -eq 8388608
fsck.exfat "$img" >"$SANDBOX/fsck.json"
check "fsck.exfat finds the new volume clean" test $? -eq 0
jq_check "fsck.exfat finds no deviation" '.healthy == true and (.deviations.found | length) == 0' "$SANDBOX/fsck.json"
check "fs.exfat lists an empty root" test "$(fs.exfat "$img" ls / | jq length)" -eq 0
check "rust-fs-exfat mkfs is the same program" \
    test "$(rust-fs-exfat mkfs -n --serial 1 "$img" | jq -c .)" = "$(mkfs.exfat -n --serial 1 "$img" | jq -c .)"

before="$(sha256_of "$img")"
mkfs.exfat -n -S 4096 "$img" >"$SANDBOX/dry.json"
check "a dry run exits 0" test $? -eq 0
jq_check "a dry run says so" '.dry_run == true and .bytes_per_sector == 4096' "$SANDBOX/dry.json"
mkfs.exfat -L 'a label too long' "$img" >"$SANDBOX/usage.out" 2>"$SANDBOX/usage.err"
check "a label too long exits 2" test $? -eq 2
jq_check "a label too long is a structured error with code 2" '.code == 2' "$SANDBOX/usage.err"
check "neither wrote to the image" test "$(sha256_of "$img")" = "$before"

mkfs.exfat --size 512K "$SANDBOX/small.img" >/dev/null 2>&1
check "a volume under 1 MiB exits 2" test $? -eq 2
check "a refused layout creates no file" test ! -e "$SANDBOX/small.img"

finish
