# fs.exfat: ls, stat, cat and export on the synthetic volume, as installed,
# and that the source is never written.
source "$(dirname "$0")/lib.sh"

img="$(sample_image)"
before="$(sha256_of "$img")"

fs.exfat "$img" ls / >"$SANDBOX/ls.json" 2>"$SANDBOX/ls.err"
check "fs.exfat ls / exits 0 ($(cat "$SANDBOX/ls.err"))" test $? -eq 0
jq_check "ls / lists the three root entries" '[.[].name] == ["Example.txt", "Empty.bin", "Sub"]' "$SANDBOX/ls.json"
jq_check "ls / reports Sub as a directory" '.[] | select(.name == "Sub") | .type == "directory"' "$SANDBOX/ls.json"

fs.exfat "$img" stat /eXAMPLE.TXT >"$SANDBOX/stat.json" 2>"$SANDBOX/stat.err"
check "stat matches a name in another case ($(cat "$SANDBOX/stat.err"))" test $? -eq 0
jq_check "stat reports size and valid data length" '.size == 1030 and .valid_data_length == 513' "$SANDBOX/stat.json"
jq_check "stat reports the creation year" '.created.year == 2026' "$SANDBOX/stat.json"

fs.exfat "$img" cat Sub/Child.bin >"$SANDBOX/child" 2>"$SANDBOX/child.err"
check "cat of a nested file exits 0 ($(cat "$SANDBOX/child.err"))" test $? -eq 0
check "cat writes the nested file's bytes" test "$(cat "$SANDBOX/child")" = nested
fs.exfat "$img" cat Empty.bin >"$SANDBOX/empty"
check "cat of an empty file writes nothing" test ! -s "$SANDBOX/empty"
fs.exfat "$img" cat Example.txt >"$SANDBOX/example"
check "cat writes the whole size, zero past the valid data length" test "$(wc -c <"$SANDBOX/example" | tr -d ' ')" -eq 1030
check "the bytes past the valid data length are zero" \
    test "$(tail -c 517 "$SANDBOX/example" | od -An -v -tx1 | tr -d ' \n' | tr -d 0)" = ""

fs.exfat "$img" export Example.txt "$SANDBOX/exported" --verify >"$SANDBOX/export.json" 2>"$SANDBOX/export.err"
check "export --verify exits 0 ($(cat "$SANDBOX/export.err"))" test $? -eq 0
jq_check "export reports a verified copy of 1030 bytes" '.verified == true and .bytes == 1030 and .replaced == false' "$SANDBOX/export.json"
check "export's digest is the exported file's" test "$(jq -r .sha256 "$SANDBOX/export.json")" = "$(sha256_of "$SANDBOX/exported")"
check "export writes what cat writes" cmp -s "$SANDBOX/exported" "$SANDBOX/example"

# A path that is not there: 1, a structured error, nothing on stdout.
fs.exfat "$img" cat /absent >"$SANDBOX/absent.out" 2>"$SANDBOX/absent.err"
check "cat of a missing path exits 1" test $? -eq 1
check "cat of a missing path prints nothing on stdout" test ! -s "$SANDBOX/absent.out"
jq_check "a missing path is a structured not_found error" '.kind == "not_found" and .code == 1' "$SANDBOX/absent.err"

check "rust-fs-exfat fs is the same program" \
    test "$(rust-fs-exfat fs --text "$img" ls /)" = "$(fs.exfat --text "$img" ls /)"
check "the source image is unchanged" test "$(sha256_of "$img")" = "$before"

finish
