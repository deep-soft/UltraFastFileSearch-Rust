// ├── disk/                       # Disk-level operations (raw disk reading, block devices, etc.)
// │   ├── mod.rs                  # Disk operations module entry point
// │   ├── ntfs_disk.rs            # NTFS raw disk reader (handling partitioning, MFT parsing)
// │   ├── ext_disk.rs             # EXT raw disk reader
// │   ├── macfs_disk.rs           # macOS raw disk reader (APFS, HFS+)
// │   └── common.rs               # Shared disk-level utilities (sector reading, caching, etc.)
pub(crate) mod ntfs_disk;
pub(crate) mod ext_disk;
pub(crate) mod macfs_disk;
pub(crate) mod common;
pub mod drive_info;
pub mod wmi_volume_quota;
pub mod wmi_mount_point;
pub mod wmi_quota_setting;
pub mod wmi_shadow_copy;
pub mod wmi_perf_disk_physical_disk;
pub mod wmi_msft_partition;
pub mod wmi_volume;
pub mod wmi_logical_disk;
pub mod wmi_disk_partition;
pub mod wmi_disk_drive;
pub mod wmi_msft_disk;
pub mod wmi_physical_media;
pub mod wmi_encryptable_volume;
pub mod wim_disk_quota;
pub mod wim_defrag_analysis;
pub mod wmi_volume_change_event;