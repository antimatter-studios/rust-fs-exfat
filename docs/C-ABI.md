# C ABI

Include `include/fs_exfat.h` and link `libfs_exfat.a`. `chore staticlib` builds
the default library without optional CLI dependencies. The C header is checked
by a C compiler, and the macOS oracle also links and executes a C caller.

`fs_exfat_mount(path, offset)` opens a UTF-8 image/device path read-only.
`fs_exfat_mount_with_callbacks(config, offset)` accepts only a positioned read
callback and a fixed source size, and the mount it makes never writes.
The callback must fill the entire requested buffer and return zero on success,
or return a nonzero error. Reads reaching callbacks are 512-byte aligned. A
partition offset must also be a multiple of 512 and lie inside the source.

`fs_exfat_mount_rw(path, offset)` opens the path read-write and mounts the
volume for writing; `fs_exfat_mount_rw_with_callbacks(config, offset)` does the
same through read, write and optional flush callbacks, each returning zero on
success. Writes reaching callbacks are whole volume sectors at sector-aligned
offsets. A writable mount refuses a volume already marked dirty or reporting
media failures, sets VolumeDirty in the Main Boot Sector until it is unmounted,
and serializes its calls with a lock. Every read call works on either kind of
mount and sees the writes made so far. `fs_exfat_flush` writes pending changes
and flushes the source; it returns 0, or -1 on error and on a read-only mount.
`fs_exfat_umount_checked` closes either kind of mount and returns 0 when a
writable mount was flushed and marked clean, or -1 when it could not be; a
mount on which a write failed partway is left marked dirty. `fs_exfat_umount`
does the same and discards the result.

On a writable mount, `fs_exfat_create(handle, path)` makes an empty file with
Archive set and `fs_exfat_mkdir(handle, path)` an empty directory; both return
0 or -1. Names are checked as Section 7.7.3 requires and compared with existing
ones through the volume's Up-case Table. `fs_exfat_pwrite(handle, path, offset,
data, length)` writes into an existing file, growing it, with any gap past the
old valid length reading as zeros; `fs_exfat_write_file(handle, path, data,
length)` creates the file or replaces its contents. Both return the byte count
or -1. New entries take the current time in UTC; writes set LastModified,
LastAccessed and Archive. `fs_exfat_truncate(handle, path, length)` shrinks a
file, freeing clusters past the end, or grows it with bytes that read as zeros.
`fs_exfat_unlink(handle, path)` removes a file and `fs_exfat_rmdir(handle,
path)` an empty directory; each clears InUse on the entry set before freeing
its clusters. All three return 0 or -1. `fs_exfat_rename(handle, from, to)`
renames or moves an entry, within or across directories, replacing a file
with a file or an empty directory with a directory as rename(2) does; a
directory cannot move below itself. `fs_exfat_utimens(handle, path, created,
modified, accessed)` sets the times from `fs_exfat_timestamp` records (the
same record `fs_exfat_stat` fills): `unix_milliseconds`, at
`utc_offset_minutes` when `offset_known` is set and in UTC otherwise, kept to
hundredths; a NULL record leaves that time alone. LastAccessed has a
two-second resolution. `fs_exfat_set_attributes(handle, path, attributes)`
sets ReadOnly, Hidden, System and Archive, and `fs_exfat_set_volume_label`
sets a label of up to 11 UTF-16 units, empty for none. All return 0 or -1. On
a read-only mount every write call fails with EROFS.

`fs_exfat_last_errno` returns the errno value of the last fallible call on the
thread: 0 after success; ENOENT, ENOTDIR, EEXIST, EISDIR, ENOTEMPTY, ENOSPC,
EROFS, ENOTSUP or EINVAL for the matching failure; EIO for device errors and panics.

Mount constructors return NULL on failure. `fs_exfat_stat` returns 0 on success
and -1 on error. `fs_exfat_read_file` returns a byte count, 0 at EOF, or -1 on
error; a zero-length read may pass a NULL buffer. Reads beyond ValidDataLength
return zero bytes within DataLength, even at unaligned boundaries.

`fs_exfat_get_volume_info` returns 0 on success and -1 on error. It reports the
volume and cluster geometry, serial, flags, label and optional raw GUID bytes.
The label is NUL-terminated UTF-8; `guid_present` distinguishes an absent GUID.

`fs_exfat_dir_open` returns an owned iterator, or NULL. `fs_exfat_dir_next`
returns 1 for an entry, 0 at the end, and -1 on error. Entries are copied into
the caller's output record. Names are UTF-8 and NUL-terminated, with capacity
for every valid exFAT filename. Directory iterators own their metadata and may
outlive the mount. Close each with `fs_exfat_dir_close` exactly once.

`fs_exfat_last_error` returns a thread-local error message, or NULL after
success. The pointer remains valid until the next fallible call on that thread.
Cleanup preserves the diagnostic. Rust panics are caught at ABI boundaries and
become failure returns with a message; a foreign callback must not unwind.

Close a mount with `fs_exfat_umount` exactly once, after all concurrent calls
finish. A callback context must remain valid and support concurrent positioned
reads throughout the mount lifetime. Directory iteration requires exclusive
access to that iterator. NULL cleanup handles are harmless; other pointer
arguments must refer to valid, aligned memory with the documented extent.
Input strings must be NUL-terminated UTF-8. Buffers cannot overlap handles or
callback storage. An error may leave a read buffer partially filled.

Timestamp records preserve calendar fields and the volume's UTC offset.
`present` distinguishes absent root timestamps; `offset_known` distinguishes
unknown UTC offsets. Unix milliseconds are meaningful only when both are set.
No UTC offset is guessed from the host clock.

## Formatter

`fs_exfat_mkfs(path, options)` formats an existing UTF-8 image or device path,
opened read-write. `fs_exfat_mkfs_with_callbacks(device, options)` formats a
callback device described by the writable mount's `fs_exfat_rw_callbacks`:
`read` and `write` are required and `flush` is optional; each returns zero on
success. Both return 0, or -1 with `fs_exfat_last_error` set and
`fs_exfat_last_errno` EINVAL for a refused layout, EROFS for a read-only device
and EIO for any other device failure.

Every zero field of `fs_exfat_format_options` takes its default, and NULL
options take every default: the whole device, 512-byte sectors, a cluster size
chosen by volume size, the FAT and heap aligned to the cluster size, one FAT,
no label, no GUID and a serial number from the clock. `serial_present` and
`guid_present` select `volume_serial` and `guid`; `number_of_fats` 2 makes a
TexFAT volume; `label` is NUL-terminated UTF-8 of at most 11 UTF-16 code units,
or NULL for no Volume Label entry. The options record is 64 bytes, which the C smoke test asserts. A refused layout writes
nothing.
