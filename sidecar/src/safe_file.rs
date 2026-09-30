use std::{fs::File, io, path::Path};

#[cfg(any(unix, windows))]
use std::fs::OpenOptions;

/// Open a regular file for reading, following normal path aliases.
///
/// Unix callers use this for operator-configured paths where symlinks are
/// expected to work. `O_NONBLOCK` ensures a FIFO cannot pin a synchronous
/// reader before the opened handle can be checked with `fstat`-equivalent
/// metadata.
pub(crate) fn open_regular_read(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;

        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?
    };

    #[cfg(not(unix))]
    let file = File::open(path)?;

    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    Ok(file)
}

/// Open an existing regular file without following its final symlink or
/// Windows reparse point.
pub(crate) fn open_regular_read_no_follow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;

        OpenOptions::new()
            .read(true)
            // O_NOFOLLOW prevents a final-component symlink from being
            // followed. O_NONBLOCK also matters here: opening a FIFO for
            // reading otherwise waits for a writer before we can inspect
            // metadata and reject the non-regular file.
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?
    };

    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file is a reparse point",
            ));
        }
        file
    };

    #[cfg(not(any(unix, windows)))]
    let file = File::open(path)?;

    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::{open_regular_read, open_regular_read_no_follow};

    #[cfg(unix)]
    #[test]
    fn regular_read_follows_symlinks_to_regular_files() {
        use std::{io::Read, os::unix::fs::symlink};

        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("target");
        let alias = directory.path().join("alias");
        std::fs::write(&target, b"contents").expect("write target");
        symlink(&target, &alias).expect("create symlink");

        let mut file = open_regular_read(&alias).expect("open regular symlink target");
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).expect("read target");
        assert_eq!(contents, b"contents");
    }

    #[cfg(unix)]
    #[test]
    fn regular_read_rejects_fifo_without_waiting_for_a_writer() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc, thread, time::Duration};

        let directory = tempfile::tempdir().expect("tempdir");
        let fifo = directory.path().join("not-a-regular-file");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `fifo_c` is a valid NUL-terminated path and remains alive
        // for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

        let (result_tx, result_rx) = mpsc::channel();
        let worker_path = fifo.clone();
        let worker = thread::spawn(move || {
            let _ = result_tx.send(open_regular_read(&worker_path));
        });

        let result = match result_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Unblock a regression to the old blocking open so the test
                // can join the worker cleanly before failing.
                let writer = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&fifo)
                    .expect("unblock a blocking FIFO reader");
                drop(writer);
                let result = result_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("FIFO reader did not return after a writer connected");
                worker.join().expect("FIFO open worker panicked");
                assert!(
                    result.is_err(),
                    "FIFO must not be accepted as a regular file"
                );
                panic!("opening a FIFO blocked while waiting for a writer");
            }
            Err(error) => panic!("FIFO open worker failed: {error}"),
        };
        worker.join().expect("FIFO open worker panicked");
        assert!(
            result.is_err(),
            "FIFO must not be accepted as a regular file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_fifo_without_waiting_for_a_writer() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc, thread, time::Duration};

        let directory = tempfile::tempdir().expect("tempdir");
        let fifo = directory.path().join("not-a-regular-file");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `fifo_c` is a valid NUL-terminated path and remains alive
        // for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

        let (result_tx, result_rx) = mpsc::channel();
        let worker_path = fifo.clone();
        let worker = thread::spawn(move || {
            let _ = result_tx.send(open_regular_read_no_follow(&worker_path));
        });

        let result = match result_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Unblock a regression to the old blocking open so the test
                // can join the worker cleanly before failing.
                let writer = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&fifo)
                    .expect("unblock a blocking FIFO reader");
                drop(writer);
                let result = result_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("FIFO reader did not return after a writer connected");
                worker.join().expect("FIFO open worker panicked");
                assert!(
                    result.is_err(),
                    "FIFO must not be accepted as a regular file"
                );
                panic!("opening a FIFO blocked while waiting for a writer");
            }
            Err(error) => panic!("FIFO open worker failed: {error}"),
        };
        worker.join().expect("FIFO open worker panicked");
        assert!(
            result.is_err(),
            "FIFO must not be accepted as a regular file"
        );
    }
}
