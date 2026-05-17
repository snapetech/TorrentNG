use std::path::PathBuf;

use rt_storage::{
    execute_storage_plan_under_roots, plan_delete, plan_import, plan_move, DeletePlanRequest,
    ImportPlanRequest, MovePlanRequest,
};

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn bench_root() -> Option<PathBuf> {
    std::env::var_os("TNG_STORAGE_MOVE_IMPORT_ROOT").map(PathBuf::from)
}

fn write_fixture_tree(root: &std::path::Path, files: u64, mib_per_file: u64) -> u64 {
    let mut bytes = 0u64;
    for i in 0..files {
        let dir = root.join(format!("dir-{:03}", i % 16));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("file-{i:05}.bin"));
        let file = std::fs::File::create(path).unwrap();
        file.set_len(mib_per_file.saturating_mul(1024 * 1024))
            .unwrap();
        file.sync_all().unwrap();
        bytes = bytes.saturating_add(mib_per_file.saturating_mul(1024 * 1024));
    }
    bytes
}

fn read_tree_len(path: &std::path::Path) -> u64 {
    let metadata = std::fs::metadata(path).unwrap();
    if metadata.is_file() {
        return metadata.len();
    }

    let mut bytes = 0u64;
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        bytes = bytes.saturating_add(read_tree_len(&entry.path()));
    }
    bytes
}

#[test]
#[ignore = "real-root move/import certification; set TNG_STORAGE_MOVE_IMPORT_ROOT"]
fn move_import_delete_executor_runs_on_configured_storage_root() {
    let Some(root) = bench_root() else {
        eprintln!("tng_storage_move_import skipped_no_root=1");
        return;
    };
    std::fs::create_dir_all(&root).unwrap();

    let files = env_u64("TNG_STORAGE_MOVE_IMPORT_FILES", 64);
    let mib_per_file = env_u64("TNG_STORAGE_MOVE_IMPORT_MIB_PER_FILE", 1);
    let run = tempfile::Builder::new()
        .prefix("tng-move-import-")
        .tempdir_in(&root)
        .unwrap();
    let source = run.path().join("source-tree");
    let moved = run.path().join("moved-tree");
    let imported = run.path().join("imported-tree");
    std::fs::create_dir_all(&source).unwrap();

    let expected_bytes = write_fixture_tree(&source, files, mib_per_file);
    let move_plan = plan_move(&MovePlanRequest {
        source: source.clone(),
        destination: moved.clone(),
        bytes: expected_bytes,
        available_bytes: None,
        dry_run: false,
    });
    execute_storage_plan_under_roots(&move_plan, &[root.clone()]).unwrap();
    assert!(!source.exists());
    assert_eq!(read_tree_len(&moved), expected_bytes);

    let import_plan = plan_import(&ImportPlanRequest {
        source: moved.clone(),
        destination: imported.clone(),
        bytes: expected_bytes,
        available_bytes: None,
        hardlink_or_copy: true,
        dry_run: false,
    });
    execute_storage_plan_under_roots(&import_plan, &[root.clone()]).unwrap();
    assert_eq!(read_tree_len(&moved), expected_bytes);
    assert_eq!(read_tree_len(&imported), expected_bytes);

    let delete_plan = plan_delete(&DeletePlanRequest {
        target: imported.clone(),
        bytes: expected_bytes,
        dry_run: false,
        dry_run_approved: true,
    });
    execute_storage_plan_under_roots(&delete_plan, &[root.clone()]).unwrap();
    assert!(!imported.exists());

    println!(
        "tng_storage_move_import root={} files={files} mib_per_file={mib_per_file} bytes={expected_bytes} moved=1 imported=1 deleted=1 root_confined=1",
        root.display()
    );
}
