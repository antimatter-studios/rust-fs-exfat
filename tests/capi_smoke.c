#include "fs_exfat.h"
#include <stdio.h>
#include <string.h>

_Static_assert(sizeof(fs_exfat_timestamp) == 24, "timestamp ABI");
_Static_assert(sizeof(fs_exfat_stat_t) == 96, "stat ABI");
_Static_assert(sizeof(fs_exfat_dir_entry) == 1120, "directory ABI");
_Static_assert(sizeof(fs_exfat_volume_info) == 96, "volume information ABI");
_Static_assert(sizeof(fs_exfat_format_options) == 64, "format options ABI");

/* Formats an existing, empty file through the C ABI, then mounts it. */
static int format_and_mount(const char *path) {
    fs_exfat_format_options options;
    memset(&options, 0, sizeof(options));
    options.bytes_per_sector = 4096;
    options.label = "C ABI";
    if (fs_exfat_mkfs(path, &options) != 0) return 20;
    fs_exfat_t *handle = fs_exfat_mount(path, 0);
    if (handle == NULL) return 21;
    fs_exfat_volume_info info;
    if (fs_exfat_get_volume_info(handle, &info) != 0 || strcmp(info.label, "C ABI") != 0 || info.sector_size != 4096) return 22;
    fs_exfat_umount(handle);
    puts("capi: C formatter and mount of the formatted file passed");
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "--mkfs") == 0) return format_and_mount(argv[2]);
    if (argc != 2) return 2;
    fs_exfat_t *handle = fs_exfat_mount(argv[1], 0);
    if (handle == NULL) return 3;
    fs_exfat_volume_info info;
    if (fs_exfat_get_volume_info(handle, &info) != 0 || strcmp(info.label, "M1ORACLE") != 0 || info.guid_present != 1) return 8;
    for (size_t index = 0; index < sizeof(info.guid); ++index) if (info.guid[index] != index + 1) return 9;
    fs_exfat_stat_t metadata;
    if (fs_exfat_stat(handle, "/ascii.TXT", &metadata) != 0) return 4;
    const char expected[] = "contents of ASCII.txt\n";
    uint8_t bytes[128];
    int64_t count = fs_exfat_read_file(handle, "/ascii.TXT", 0, bytes, sizeof(bytes));
    if (count != (int64_t)(sizeof(expected) - 1) || memcmp(bytes, expected, sizeof(expected) - 1) != 0) return 5;
    fs_exfat_dir_t *directory = fs_exfat_dir_open(handle, "/");
    if (directory == NULL) return 6;
    fs_exfat_dir_entry entry;
    int result;
    size_t entries = 0;
    while ((result = fs_exfat_dir_next(directory, &entry)) == 1) ++entries;
    if (result != 0 || entries == 0) return 7;
    fs_exfat_dir_close(directory);
    fs_exfat_umount(handle);
    puts("capi: C compiler, static linking and reference-image read passed");
    return 0;
}
