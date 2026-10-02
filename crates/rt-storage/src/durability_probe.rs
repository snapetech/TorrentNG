//! Mount durability classification.
//!
//! The durability barrier only helps if `fsync` on the mount actually reaches
//! stable storage. Some mounts cannot promise that: volatile filesystems,
//! filesystems mounted with write barriers disabled, and layers (network,
//! FUSE, overlay) whose flush semantics depend on configuration TorrentNG
//! cannot see.
//!
//! This module classifies a path's mount into [`DurabilityTrust`] from the
//! filesystem type and mount options. It answers a narrow question: *does the
//! filesystem layer honor fsync?* It cannot see the disk's own volatile write
//! cache, RAID controller batteries, or a ZFS dataset's `sync=disabled`
//! property, so `Strong` means "no known reason to distrust", not a guarantee.
//! Operators can override the result per path prefix in configuration.
//!
//! Detection reads `/proc/self/mountinfo` on Linux (type and mount options),
//! `statfs(2)` on macOS and FreeBSD (type only), and the volume information
//! and drive type on Windows (a remote drive is `network`). Platforms where
//! nothing can be read report `Unknown`, which the recovery policy treats
//! conservatively.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::device::detect_mount_details;

/// How far a mount's `fsync` can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityTrust {
    /// A local block filesystem with no known reason to distrust fsync.
    Strong,
    /// Semantics depend on configuration TorrentNG cannot inspect (network,
    /// FUSE, overlay), or the platform cannot be probed.
    Unknown,
    /// Known not to provide durable fsync: volatile storage, or barriers /
    /// server flushes disabled.
    Weak,
}

impl DurabilityTrust {
    pub fn as_str(self) -> &'static str {
        match self {
            DurabilityTrust::Strong => "strong",
            DurabilityTrust::Unknown => "unknown",
            DurabilityTrust::Weak => "weak",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountDurability {
    pub trust: DurabilityTrust,
    pub fs_type: Option<String>,
    /// Human-readable reasons for the classification, shown in the UI.
    pub reasons: Vec<String>,
}

const STRONG_FS: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "jfs", "bcachefs", "reiserfs", "ntfs",
    "ntfs3", "exfat", "vfat", "fat", "fat32", "msdos", "apfs", "hfs", "hfsplus", "ufs", "refs",
];
const VOLATILE_FS: &[&str] = &["tmpfs", "ramfs", "devtmpfs", "rootfs"];
const NETWORK_FS: &[&str] = &[
    "network",
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "afpfs",
    "webdav",
    "9p",
    "virtiofs",
    "ceph",
    "glusterfs",
    "lustre",
    "afs",
];

/// Mount options that disable flush/barrier behavior.
const WEAK_OPTIONS: &[(&str, &str)] = &[
    ("nobarrier", "write barriers are disabled"),
    ("barrier=0", "write barriers are disabled"),
    ("nostrictsync", "fsync is not forwarded to the server"),
    ("cache=loose", "client caching ignores fsync"),
    ("cache=mmap", "client caching ignores fsync"),
    ("nobh", "buffer-head flushing is relaxed"),
];

/// Classify a filesystem type and its mount options.
pub fn classify_mount(fs_type: &str, options: &[String]) -> MountDurability {
    let fs = fs_type.to_ascii_lowercase();
    let mut reasons = Vec::new();

    for (needle, why) in WEAK_OPTIONS {
        if options.iter().any(|option| option == needle) {
            reasons.push(format!("mount option `{needle}`: {why}"));
        }
    }
    if VOLATILE_FS.contains(&fs.as_str()) {
        reasons.push(format!(
            "`{fs}` is volatile; contents do not survive a reboot"
        ));
    }
    if !reasons.is_empty() {
        return MountDurability {
            trust: DurabilityTrust::Weak,
            fs_type: Some(fs_type.to_owned()),
            reasons,
        };
    }

    if NETWORK_FS.contains(&fs.as_str()) {
        return unknown(
            fs_type,
            "network filesystem: durability depends on the server's export and cache settings",
        );
    }
    if matches!(
        fs.as_str(),
        "overlay" | "fuse-overlayfs" | "ecryptfs" | "nullfs" | "unionfs"
    ) {
        return unknown(
            fs_type,
            "stacked filesystem: durability of the underlying layer is not inspected",
        );
    }
    if fs.starts_with("fuse") {
        return unknown(
            fs_type,
            "FUSE filesystem: fsync propagation depends on the daemon and its cache mode",
        );
    }
    if STRONG_FS.contains(&fs.as_str()) {
        return MountDurability {
            trust: DurabilityTrust::Strong,
            fs_type: Some(fs_type.to_owned()),
            reasons: vec![
                "local block filesystem; the disk's own write cache is not inspected".to_owned(),
            ],
        };
    }
    unknown(fs_type, "filesystem type is not in the known-durable list")
}

fn unknown(fs_type: &str, reason: &str) -> MountDurability {
    MountDurability {
        trust: DurabilityTrust::Unknown,
        fs_type: Some(fs_type.to_owned()),
        reasons: vec![reason.to_owned()],
    }
}

/// Classify the mount that holds `path`.
pub fn probe_mount_durability(path: &Path) -> MountDurability {
    match detect_mount_details(path) {
        Some(details) => classify_mount(&details.fs_type, &details.options),
        None => MountDurability {
            trust: DurabilityTrust::Unknown,
            fs_type: None,
            reasons: vec![
                "mount information is unavailable on this platform or could not be read".to_owned(),
            ],
        },
    }
}

/// Whether mount probing is implemented on this platform.
pub const fn mount_probe_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    ))
}

/// Apply operator overrides: the longest matching prefix wins, and an exact
/// tie favors the weaker classification.
pub fn apply_overrides(
    probed: MountDurability,
    path: &Path,
    weak_paths: &[std::path::PathBuf],
    strong_paths: &[std::path::PathBuf],
) -> MountDurability {
    let weak = longest_prefix(path, weak_paths);
    let strong = longest_prefix(path, strong_paths);
    match (weak, strong) {
        (Some(w), Some(s)) if s > w => override_to(probed, DurabilityTrust::Strong),
        (Some(_), _) => override_to(probed, DurabilityTrust::Weak),
        (None, Some(_)) => override_to(probed, DurabilityTrust::Strong),
        (None, None) => probed,
    }
}

fn longest_prefix(path: &Path, prefixes: &[std::path::PathBuf]) -> Option<usize> {
    prefixes
        .iter()
        .filter(|prefix| path.starts_with(prefix))
        .map(|prefix| prefix.components().count())
        .max()
}

fn override_to(mut probed: MountDurability, trust: DurabilityTrust) -> MountDurability {
    probed.reasons.insert(
        0,
        format!(
            "operator override: classified {} by configuration",
            trust.as_str()
        ),
    );
    probed.trust = trust;
    probed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn opts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn local_block_filesystems_are_strong() {
        for fs in ["ext4", "xfs", "btrfs", "zfs", "f2fs", "apfs", "ntfs3"] {
            let result = classify_mount(fs, &opts(&["rw", "relatime"]));
            assert_eq!(result.trust, DurabilityTrust::Strong, "{fs}");
        }
    }

    #[test]
    fn other_platform_filesystem_names_are_classified() {
        // macOS / FreeBSD `f_fstypename` and Windows volume names, lowercased.
        for fs in [
            "apfs", "hfs", "ufs", "zfs", "ntfs", "refs", "exfat", "fat32", "msdos",
        ] {
            assert_eq!(
                classify_mount(fs, &[]).trust,
                DurabilityTrust::Strong,
                "{fs}"
            );
        }
        for fs in [
            "network", "smbfs", "afpfs", "webdav", "nullfs", "unionfs", "fusefs", "macfuse",
        ] {
            assert_eq!(
                classify_mount(fs, &[]).trust,
                DurabilityTrust::Unknown,
                "{fs}"
            );
        }
        assert_eq!(classify_mount("ramfs", &[]).trust, DurabilityTrust::Weak);
    }

    #[test]
    fn volatile_filesystems_are_weak() {
        for fs in ["tmpfs", "ramfs"] {
            let result = classify_mount(fs, &opts(&["rw"]));
            assert_eq!(result.trust, DurabilityTrust::Weak, "{fs}");
            assert!(result.reasons[0].contains("volatile"));
        }
    }

    #[test]
    fn disabled_barriers_downgrade_an_otherwise_strong_filesystem() {
        for option in ["nobarrier", "barrier=0"] {
            let result = classify_mount("ext4", &opts(&["rw", option]));
            assert_eq!(result.trust, DurabilityTrust::Weak, "{option}");
            assert!(result.reasons.iter().any(|r| r.contains(option)));
        }
        let cifs = classify_mount("cifs", &opts(&["rw", "nostrictsync"]));
        assert_eq!(cifs.trust, DurabilityTrust::Weak);
    }

    #[test]
    fn network_stacked_and_fuse_mounts_are_unknown_not_weak() {
        for fs in [
            "nfs",
            "nfs4",
            "cifs",
            "9p",
            "overlay",
            "fuse.mergerfs",
            "fuse.sshfs",
            "fuseblk",
        ] {
            let result = classify_mount(fs, &opts(&["rw"]));
            assert_eq!(result.trust, DurabilityTrust::Unknown, "{fs}");
            assert!(!result.reasons.is_empty());
        }
    }

    #[test]
    fn unrecognized_filesystems_are_unknown() {
        assert_eq!(
            classify_mount("madeupfs", &[]).trust,
            DurabilityTrust::Unknown
        );
    }

    #[test]
    fn trust_ordering_puts_weak_last_for_escalation_logic() {
        assert!(DurabilityTrust::Strong < DurabilityTrust::Unknown);
        assert!(DurabilityTrust::Unknown < DurabilityTrust::Weak);
    }

    #[test]
    fn overrides_use_the_longest_matching_prefix() {
        let probed = classify_mount("ext4", &[]);
        let weak = vec![PathBuf::from("/mnt/pool")];
        let strong = vec![PathBuf::from("/mnt/pool/local")];

        let outer = apply_overrides(
            probed.clone(),
            Path::new("/mnt/pool/x/file.bin"),
            &weak,
            &strong,
        );
        assert_eq!(outer.trust, DurabilityTrust::Weak);
        assert!(outer.reasons[0].contains("operator override"));

        let inner = apply_overrides(
            probed.clone(),
            Path::new("/mnt/pool/local/file.bin"),
            &weak,
            &strong,
        );
        assert_eq!(inner.trust, DurabilityTrust::Strong);

        let unrelated = apply_overrides(probed.clone(), Path::new("/srv/x"), &weak, &strong);
        assert_eq!(unrelated, probed);
    }

    #[test]
    fn overrides_do_not_match_on_partial_component_names() {
        let probed = classify_mount("ext4", &[]);
        let weak = vec![PathBuf::from("/mnt/pool")];
        let result = apply_overrides(probed.clone(), Path::new("/mnt/pool2/file"), &weak, &[]);
        assert_eq!(result, probed);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probing_a_real_path_never_panics_and_reports_a_fs_type() {
        let dir = tempfile::tempdir().unwrap();
        let result = probe_mount_durability(dir.path());
        assert!(result.fs_type.is_some());
        assert!(!result.reasons.is_empty());
    }
}
