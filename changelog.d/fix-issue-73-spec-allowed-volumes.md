### Fixed

- The strict reader and `check` refused FileSystemRevision 1.01 to 1.99 and an Allocation Bitmap longer than ClusterCount needs, and salvage reported the longer bitmap as damage; section 3.1.12 says implementations should mount any minor revision of major revision 1, and section 7.1.5 (Table 22) makes the bits past ClusterCount Reserved, so both volumes are now accepted, as macOS fsck_exfat accepts them. Another major revision, a minor revision past 99 and a bitmap too short for the heap are still refused.
