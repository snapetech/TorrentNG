use std::path::{Path, PathBuf};

use rt_path::StorageProfile;

#[derive(Debug, Clone, PartialEq, Eq)]
struct MountInfo {
    mount_point: PathBuf,
    major_minor: String,
    fs_type: String,
}

/// Best-effort storage profile detection for a path.
///
/// Linux uses `/proc/self/mountinfo` and `/sys/dev/block` so operators do not
/// have to manually pick HDD/SSD defaults. Unsupported platforms return
/// `Unknown` and keep the conservative HDD-shaped scheduler defaults.
pub fn detect_storage_profile(path: &Path) -> StorageProfile {
    detect_storage_profile_inner(path).unwrap_or(StorageProfile::Unknown)
}

#[cfg(target_os = "linux")]
fn detect_storage_profile_inner(path: &Path) -> Option<StorageProfile> {
    let profile = detect_storage_profile_from_roots(
        path,
        Path::new("/proc/self/mountinfo"),
        Path::new("/sys"),
    )?;
    Some(profile)
}

#[cfg(not(target_os = "linux"))]
fn detect_storage_profile_inner(_path: &Path) -> Option<StorageProfile> {
    None
}

#[cfg(target_os = "linux")]
fn detect_storage_profile_from_roots(
    path: &Path,
    mountinfo_path: &Path,
    sys_root: &Path,
) -> Option<StorageProfile> {
    let target = canonical_existing_path(path)?;
    let mountinfo = std::fs::read_to_string(mountinfo_path).ok()?;
    let mounts = parse_mountinfo(&mountinfo)?;
    let mount = find_best_mount(&target, &mounts)?;
    if is_network_fs(&mount.fs_type) {
        return Some(StorageProfile::Network);
    }

    let device = block_device_name(sys_root, &mount.major_minor)?;
    Some(profile_from_block_device(sys_root, &device))
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
fn block_device_name(sys_root: &Path, major_minor: &str) -> Option<String> {
    let dev_path = sys_root.join("dev/block").join(major_minor);
    let canonical = std::fs::canonicalize(dev_path).ok()?;
    block_device_name_from_sysfs_path(&canonical)
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
}
