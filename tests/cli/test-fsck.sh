# fsck.exfat: its exit statuses and JSON report on the synthetic volume, as
# installed. It is check-only: -n is accepted, and nothing repairs.
source "$(dirname "$0")/lib.sh"

img="$(sample_image)"
before="$(sha256_of "$img")"

for flags in "" "-n" "--no-repair"; do
    # shellcheck disable=SC2086  # the words are the point
    fsck.exfat $flags "$img" >"$SANDBOX/fsck.json" 2>"$SANDBOX/fsck.err"
    rc=$?
    check "fsck.exfat $flags exits 0 ($(cat "$SANDBOX/fsck.err"))" test "$rc" -eq 0
    jq_check "fsck.exfat $flags reports a healthy check-only audit" \
        '.check_only == true and .healthy == true and .files == 3 and .directories == 2' "$SANDBOX/fsck.json"
done
jq_check "fsck.exfat counts every allocated cluster as referenced" \
    '.allocated_clusters == .referenced_clusters and .unreferenced_clusters == 0' "$SANDBOX/fsck.json"
jq_check "fsck.exfat finishes its deviation scan" '.deviations.complete == true' "$SANDBOX/fsck.json"
check "fsck.exfat --text says healthy" grep -qx 'healthy: true' <<<"$(fsck.exfat --text "$img")"
check "rust-fs-exfat fsck is the same program" \
    test "$(rust-fs-exfat fsck --text "$img")" = "$(fsck.exfat --text "$img")"

# Cannot be opened: 8, a structured error, nothing on stdout.
fsck.exfat "$SANDBOX/absent.img" >"$SANDBOX/absent.out" 2>"$SANDBOX/absent.err"
check "a missing image exits 8" test $? -eq 8
check "a missing image prints nothing on stdout" test ! -s "$SANDBOX/absent.out"
jq_check "a missing image is a structured error with code 8" '.code == 8' "$SANDBOX/absent.err"

# A repair flag: 16, as fsck(8) documents a usage error. There is no repair mode.
fsck.exfat -y "$img" >"$SANDBOX/usage.out" 2>"$SANDBOX/usage.err"
check "-y exits 16" test $? -eq 16
check "-y prints nothing on stdout" test ! -s "$SANDBOX/usage.out"
jq_check "-y is a structured error with code 16" '.code == 16' "$SANDBOX/usage.err"

check "the source image is unchanged" test "$(sha256_of "$img")" = "$before"

finish
