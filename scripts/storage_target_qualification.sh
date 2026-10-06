#!/usr/bin/env bash

storage_target_is_hardware_qualified() {
  local root_block="$1"
  local fstype
  fstype="$(printf '%s' "${2:-}" | tr '[:upper:]' '[:lower:]')"
  local device_type="${3:-unknown}"
  [[ -n "$root_block" ]] || return 1
  case "$device_type" in
    disk|part) ;;
    *) return 1 ;;
  esac
  case "$fstype" in
    ""|unknown|tmpfs|ramfs|overlay|aufs|proc|sysfs|devtmpfs|cgroup|cgroup2|nfs|nfs4|cifs|smb3|9p|fuse.sshfs|ceph|glusterfs)
      return 1
      ;;
    *)
      return 0
      ;;
  esac
}
