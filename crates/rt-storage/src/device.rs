use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use rt_path::StorageProfile;

use crate::elevator::DeviceId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageTopology {
    pub device_id: Option<DeviceId>,
    pub profile: StorageProfile,
    pub fs_type: Option<String>,
    pub cow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MountInfo {
    mount_point: PathBuf,
    major_minor: String,
    fs_type: String,
    source: String,
}

#[cfg(target_os = "linux")]
const MAX_MOUNTINFO_BYTES: u64 = 8 * 1024 * 1024;

/// Best-effort storage profile detection for a path.
///
/// Linux uses `/proc/self/mountinfo` and `/sys/dev/block` so operators do not
/// have to manually pick HDD/SSD defaults. Unsupported platforms return
/// `Unknown` and keep the conservative HDD-shaped scheduler defaults.
pub fn detect_storage_profile(path: &Path) -> StorageProfile {
    detect_storage_topology(path).profile
}

pub fn detect_storage_topology(path: &Path) -> StorageTopology {
    detect_storage_topology_inner(path).unwrap_or(StorageTopology {
        device_id: None,
        profile: StorageProfile::Unknown,
        fs_type: None,
        cow: false,
    })
}

#[cfg(target_os = "linux")]
fn detect_storage_topology_inner(path: &Path) -> Option<StorageTopology> {
    detect_storage_topology_from_roots(path, Path::new("/proc/self/mountinfo"), Path::new("/sys"))
}

#[cfg(not(target_os = "linux"))]
fn detect_storage_topology_inner(_path: &Path) -> Option<StorageTopology> {
    None
}

#[cfg(target_os = "linux")]
fn detect_storage_topology_from_roots(
    path: &Path,
    mountinfo_path: &Path,
    sys_root: &Path,
) -> Option<StorageTopology> {
    let target = canonical_existing_path(path)?;
    let mountinfo = read_mountinfo(mountinfo_path)?;
    let mounts = parse_mountinfo(&mountinfo)?;
    let mount = find_best_mount(&target, &mounts)?;
    let fs_type = Some(mount.fs_type.clone());
    let cow = is_cow_fs(&mount.fs_type);
    if is_network_fs(&mount.fs_type) {
        return Some(StorageTopology {
            device_id: Some(network_device_id(mount)),
            profile: StorageProfile::Network,
            fs_type,
            cow,
        });
    }

    let device = block_device_name(sys_root, &mount.major_minor)
        .or_else(|| block_device_name_from_source(sys_root, &mount.source))?;
    Some(StorageTopology {
        device_id: Some(DeviceId(device.clone())),
        profile: profile_from_block_device(sys_root, &device),
        fs_type,
        cow,
    })
}

#[cfg(target_os = "linux")]
fn read_mountinfo(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_MOUNTINFO_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_MOUNTINFO_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_MOUNTINFO_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(target_os = "linux")]
fn canonical_existing_path(path: &Path) -> Option<PathBuf> {
    let mut current = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&current) {
            return Some(canonical);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(target_os = "linux")]
fn parse_mountinfo(input: &str) -> Option<Vec<MountInfo>> {
    let mut mounts = Vec::new();
    for line in input.lines() {
        let (left, right) = line.split_once(" - ")?;
        let left_fields: Vec<&str> = left.split_whitespace().collect();
        let right_fields: Vec<&str> = right.split_whitespace().collect();
        if left_fields.len() < 5 || right_fields.is_empty() {
            continue;
        }
        mounts.push(MountInfo {
            mount_point: PathBuf::from(unescape_mountinfo(left_fields[4])),
            major_minor: left_fields[2].to_owned(),
            fs_type: right_fields[0].to_owned(),
            source: right_fields.get(1).copied().unwrap_or("").to_owned(),
        });
    }
    Some(mounts)
}

#[cfg(target_os = "linux")]
fn find_best_mount<'a>(path: &Path, mounts: &'a [MountInfo]) -> Option<&'a MountInfo> {
    mounts
        .iter()
        .filter(|mount| path.starts_with(&mount.mount_point))
        .max_by_key(|mount| mount.mount_point.as_os_str().len())
}

#[cfg(target_os = "linux")]
fn unescape_mountinfo(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            if let Ok(octal) = std::str::from_utf8(&bytes[i + 1..i + 4]) {
                if let Ok(value) = u8::from_str_radix(octal, 8) {
                    output.push(value);
                    i += 4;
                    continue;
                }
            }
        }
        output.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(target_os = "linux")]
fn is_network_fs(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "9p" | "afs"
            | "cifs"
            | "ceph"
            | "fuse.sshfs"
            | "glusterfs"
            | "nfs"
            | "nfs4"
            | "smb3"
            | "sshfs"
            | "virtiofs"
    )
}

#[cfg(target_os = "linux")]
fn network_device_id(mount: &MountInfo) -> DeviceId {
    if mount.source.is_empty() {
        DeviceId(format!("{}:{}", mount.fs_type, mount.mount_point.display()))
    } else {
        DeviceId(format!("{}:{}", mount.fs_type, mount.source))
    }
}

pub fn is_cow_fs(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "apfs" | "bcachefs" | "btrfs" | "fuse-overlayfs" | "overlay" | "zfs"
    )
}

#[cfg(target_os = "linux")]
fn block_device_name(sys_root: &Path, major_minor: &str) -> Option<String> {
    let dev_path = sys_root.join("dev/block").join(major_minor);
    let canonical = std::fs::canonicalize(dev_path).ok()?;
    block_device_name_from_sysfs_path(&canonical)
}

#[cfg(target_os = "linux")]
fn block_device_name_from_source(sys_root: &Path, source: &str) -> Option<String> {
    if !source.starts_with("/dev/") {
        return None;
    }
    let source = strip_mount_source_subpath(source);
    let meta = std::fs::metadata(source).ok()?;
    let rdev = std::os::unix::fs::MetadataExt::rdev(&meta);
    if rdev == 0 {
        return None;
    }
    let major = (((rdev >> 32) & 0xffff_f000) | ((rdev >> 8) & 0x0000_0fff)) as u32;
    let minor = (((rdev >> 12) & 0xffff_ff00) | (rdev & 0x0000_00ff)) as u32;
    block_device_name(sys_root, &format!("{major}:{minor}"))
}

#[cfg(target_os = "linux")]
fn strip_mount_source_subpath(source: &str) -> &str {
    source.split_once('[').map(|(s, _)| s).unwrap_or(source)
}

#[cfg(target_os = "linux")]
fn block_device_name_from_sysfs_path(path: &Path) -> Option<String> {
    let parts: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    for window in parts.windows(2) {
        if window[0] == "block" {
            return Some(window[1].clone());
        }
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(target_os = "linux")]
fn profile_from_block_device(sys_root: &Path, device: &str) -> StorageProfile {
    if device.starts_with("nvme") {
        return StorageProfile::Nvme;
    }
    let rotational = sys_root.join("block").join(device).join("queue/rotational");
    match std::fs::read_to_string(rotational)
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("0") => StorageProfile::Ssd,
        Some("1") => StorageProfile::Hdd,
        _ => StorageProfile::Unknown,
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_parser_decodes_paths_and_finds_longest_prefix() {
        let input = "\
24 0 8:1 / / rw,relatime - ext4 /dev/sda1 rw
25 24 8:2 / /mnt/media rw,relatime - xfs /dev/sdb1 rw
26 24 0:42 / /mnt/media/remote\\040share rw,relatime - nfs server:/share rw
";
        let mounts = parse_mountinfo(input).unwrap();
        let local = find_best_mount(Path::new("/mnt/media/movie.bin"), &mounts).unwrap();
        assert_eq!(local.major_minor, "8:2");
        assert_eq!(local.fs_type, "xfs");

        let remote =
            find_best_mount(Path::new("/mnt/media/remote share/file.bin"), &mounts).unwrap();
        assert_eq!(remote.major_minor, "0:42");
        assert_eq!(remote.fs_type, "nfs");
        assert!(is_network_fs(&remote.fs_type));
    }

    #[test]
    fn mountinfo_read_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mountinfo");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_MOUNTINFO_BYTES + 1)
            .unwrap();

        assert!(read_mountinfo(&path).is_none());
    }

    #[test]
    fn sysfs_block_device_name_prefers_parent_block_device() {
        let path = Path::new(
            "/sys/devices/pci0000:00/nvme/nvme0/nvme0n1/nvme0n1p1/block/nvme0n1/nvme0n1p1",
        );
        assert_eq!(
            block_device_name_from_sysfs_path(path).as_deref(),
            Some("nvme0n1")
        );
    }

    #[test]
    fn mount_source_subpath_suffix_is_ignored() {
        assert_eq!(
            strip_mount_source_subpath("/dev/nvme0n1p2[/@home]"),
            "/dev/nvme0n1p2"
        );
        assert_eq!(
            strip_mount_source_subpath("/dev/mapper/data"),
            "/dev/mapper/data"
        );
    }

    #[test]
    fn block_device_profile_uses_name_and_rotational_flag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("block/sda/queue")).unwrap();
        std::fs::create_dir_all(dir.path().join("block/sdb/queue")).unwrap();
        std::fs::write(dir.path().join("block/sda/queue/rotational"), "1\n").unwrap();
        std::fs::write(dir.path().join("block/sdb/queue/rotational"), "0\n").unwrap();

        assert_eq!(
            profile_from_block_device(dir.path(), "sda"),
            StorageProfile::Hdd
        );
        assert_eq!(
            profile_from_block_device(dir.path(), "sdb"),
            StorageProfile::Ssd
        );
        assert_eq!(
            profile_from_block_device(dir.path(), "nvme0n1"),
            StorageProfile::Nvme
        );
    }

    #[test]
    fn topology_reports_fs_profile_and_cow() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("root/media")).unwrap();
        std::fs::create_dir_all(dir.path().join("sys/dev/block")).unwrap();
        std::fs::create_dir_all(dir.path().join("sys/block/sda/queue")).unwrap();
        std::fs::write(dir.path().join("sys/block/sda/queue/rotational"), "1\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../../block/sda", dir.path().join("sys/dev/block/8:1"))
            .unwrap();

        let mountinfo = format!(
            "24 0 8:1 / {} rw,relatime - btrfs /dev/sda1 rw\n",
            dir.path().join("root").display()
        );
        let mountinfo_path = dir.path().join("mountinfo");
        std::fs::write(&mountinfo_path, mountinfo).unwrap();

        let topology = detect_storage_topology_from_roots(
            &dir.path().join("root/media/movie.bin"),
            &mountinfo_path,
            &dir.path().join("sys"),
        )
        .unwrap();
        assert_eq!(topology.device_id, Some(DeviceId("sda".to_owned())));
        assert_eq!(topology.profile, StorageProfile::Hdd);
        assert_eq!(topology.fs_type.as_deref(), Some("btrfs"));
        assert!(topology.cow);
    }

    #[test]
    fn network_topology_uses_mount_source_as_device_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("root/remote")).unwrap();
        let mountinfo = format!(
            "24 0 0:42 / {} rw,relatime - nfs server:/share rw\n",
            dir.path().join("root/remote").display()
        );
        let mountinfo_path = dir.path().join("mountinfo");
        std::fs::write(&mountinfo_path, mountinfo).unwrap();

        let topology = detect_storage_topology_from_roots(
            &dir.path().join("root/remote/movie.bin"),
            &mountinfo_path,
            &dir.path().join("sys"),
        )
        .unwrap();
        assert_eq!(
            topology.device_id,
            Some(DeviceId("nfs:server:/share".to_owned()))
        );
        assert_eq!(topology.profile, StorageProfile::Network);
    }

    #[test]
    fn cow_fs_detection_covers_common_filesystems() {
        for fs_type in [
            "btrfs",
            "zfs",
            "bcachefs",
            "overlay",
            "fuse-overlayfs",
            "apfs",
        ] {
            assert!(is_cow_fs(fs_type), "{fs_type} should be treated as CoW");
        }
        for fs_type in ["ext4", "xfs", "nfs", "tmpfs"] {
            assert!(
                !is_cow_fs(fs_type),
                "{fs_type} should not be treated as CoW"
            );
        }
    }
}
