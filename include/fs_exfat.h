#ifndef FS_EXFAT_H
#define FS_EXFAT_H
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FsExfat fs_exfat_t;
typedef struct FsExfatDirectory fs_exfat_dir_t;
typedef int (*fs_exfat_read_callback)(void *, uint64_t, uint8_t *, size_t);
typedef int (*fs_exfat_write_callback)(void *, uint64_t, const uint8_t *, size_t);
typedef int (*fs_exfat_flush_callback)(void *);

typedef struct {
    void *context;
    uint64_t size_bytes;
    fs_exfat_read_callback read;
} fs_exfat_callbacks;

typedef struct {
    void *context;
    uint64_t size_bytes;
    fs_exfat_read_callback read;
    fs_exfat_write_callback write;
    fs_exfat_flush_callback flush;
} fs_exfat_rw_callbacks;

typedef struct {
    int64_t unix_milliseconds;
    uint16_t year;
    uint16_t millisecond;
    int16_t utc_offset_minutes;
    uint8_t month, day, hour, minute, second, offset_known, present;
    uint8_t reserved[3];
} fs_exfat_timestamp;

typedef struct {
    uint64_t data_length;
    uint64_t valid_data_length;
    uint32_t first_cluster;
    uint16_t attributes;
    uint8_t is_directory;
    uint8_t no_fat_chain;
    fs_exfat_timestamp created, modified, accessed;
} fs_exfat_stat_t;

typedef struct {
    fs_exfat_stat_t stat;
    char name[1021];
    uint8_t reserved[3];
} fs_exfat_dir_entry;

typedef struct {
    uint64_t volume_bytes;
    uint64_t cluster_size;
    uint32_t cluster_count;
    uint32_t volume_serial;
    uint16_t volume_flags;
    uint16_t sector_size;
    uint8_t guid[16];
    uint8_t guid_present;
    char label[45];
    uint8_t reserved[6];
} fs_exfat_volume_info;

fs_exfat_t *fs_exfat_mount(const char *path, uint64_t offset);
fs_exfat_t *fs_exfat_mount_with_callbacks(const fs_exfat_callbacks *, uint64_t offset);
fs_exfat_t *fs_exfat_mount_rw(const char *path, uint64_t offset);
fs_exfat_t *fs_exfat_mount_rw_with_callbacks(const fs_exfat_rw_callbacks *, uint64_t offset);
int fs_exfat_flush(const fs_exfat_t *);
int fs_exfat_umount_checked(fs_exfat_t *);
void fs_exfat_umount(fs_exfat_t *);
int fs_exfat_get_volume_info(const fs_exfat_t *, fs_exfat_volume_info *);
int fs_exfat_stat(const fs_exfat_t *, const char *path, fs_exfat_stat_t *);
fs_exfat_dir_t *fs_exfat_dir_open(const fs_exfat_t *, const char *path);
int fs_exfat_dir_next(fs_exfat_dir_t *, fs_exfat_dir_entry *);
void fs_exfat_dir_close(fs_exfat_dir_t *);
int64_t fs_exfat_read_file(const fs_exfat_t *, const char *path, uint64_t offset, uint8_t *, size_t);
int fs_exfat_create(const fs_exfat_t *, const char *path);
int fs_exfat_mkdir(const fs_exfat_t *, const char *path);
int64_t fs_exfat_pwrite(const fs_exfat_t *, const char *path, uint64_t offset, const uint8_t *, size_t);
int64_t fs_exfat_write_file(const fs_exfat_t *, const char *path, const uint8_t *, size_t);
int fs_exfat_truncate(const fs_exfat_t *, const char *path, uint64_t length);
int fs_exfat_unlink(const fs_exfat_t *, const char *path);
int fs_exfat_rmdir(const fs_exfat_t *, const char *path);
const char *fs_exfat_last_error(void);
int fs_exfat_last_errno(void);

#ifdef __cplusplus
}
#endif
#endif
