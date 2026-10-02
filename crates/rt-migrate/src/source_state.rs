//! Was the source client shut down cleanly?
//!
//! Importing a client's resume state and trusting it is only sound if that
//! state was written by a client that finished its shutdown. A source that
//! crashed (or lost power) may have saved a bitfield claiming pieces whose
//! data never reached disk. Where a client leaves a lock or pid file behind,
//! its presence after the process is gone is a reliable "unclean" signal.
//!
//! What is known for each source, stated plainly:
//!
//! * **rTorrent** keeps `rtorrent.lock` in its session directory, containing
//!   `hostname:+pid`, and removes it on exit. Its location is fixed, so a
//!   missing lock means a clean exit.
//! * **qBittorrent** (a Qt application) keeps a `lockfile` written by
//!   `QLockFile`: pid, application name and host name on separate lines.
//!   **Deluge** keeps `deluged.pid`. Where they sit relative to the state
//!   directory you pass depends on the install (profile directory, `--from`
//!   pointing at `BT_backup` or at the config directory), so TorrentNG looks in
//!   the state directory and its two parents. These names and formats come from
//!   the clients' documentation and behavior as generally reported, **not from
//!   running the clients here**. A missing file is therefore *unknown*, never
//!   "clean": only a lock that is found and stale changes anything.
//! * **Any other source** (Transmission, uTorrent, BiglyBT, Tixati, generic) has
//!   no lock TorrentNG can rely on. Pass `--source-lock <FILE>` to point at
//!   whatever lock or pid file that client keeps (for example
//!   `transmission-daemon --pid-file`); the same parser and liveness check
//!   apply, and a missing explicit file means the client exited cleanly.
//!
//! A lock file is classified by looking at its process: gone means the last run
//! did not finish; alive and named like the client means the client is running
//! now and its state is mid-write. Process inspection works on Linux
//! (`/proc`), macOS and FreeBSD (`kill(2)` plus `ps`), and Windows
//! (`OpenProcess`); elsewhere a lock is treated as stale.

use std::path::{Path, PathBuf};

use crate::MigrationSource;

/// Longest lock file we will read.
const MAX_LOCK_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceShutdown {
    /// No lock was left behind: the source exited cleanly.
    Clean,
    /// The source appears to be running right now. Its state is mid-write and
    /// must not be imported.
    Running { pid: u32 },
    /// A lock remains but its process is gone: the last run did not finish.
    Crashed { reason: String },
    /// TorrentNG has no signal for this source.
    Unknown,
}

impl SourceShutdown {
    /// Whether the resume state may reflect data that never reached disk.
    pub fn is_suspect(&self) -> bool {
        matches!(self, SourceShutdown::Crashed { .. })
    }
}

/// What is known about a process id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessState {
    NotRunning,
    /// Alive. `name` is the executable name when it could be read.
    Running {
        name: Option<String>,
    },
    /// This platform cannot inspect processes.
    Unknown,
}

/// Contents of a lock or pid file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockInfo {
    pub pid: u32,
    pub host: Option<String>,
    pub app: Option<String>,
}

/// Where a client keeps its lock, and how to recognize its process.
struct LockSpec {
    /// File names to look for.
    names: &'static [&'static str],
    /// How many directory levels above the state directory to search (0 = the
    /// state directory only).
    levels_up: usize,
    /// Lowercase substrings of the client's process name.
    process_names: &'static [&'static str],
    /// The lock's location is fixed, so its absence proves a clean exit.
    absence_is_clean: bool,
}

fn lock_spec(source: MigrationSource) -> Option<LockSpec> {
    match source {
        MigrationSource::RTorrent => Some(LockSpec {
            names: &["rtorrent.lock"],
            levels_up: 0,
            process_names: &["rtorrent"],
            absence_is_clean: true,
        }),
        MigrationSource::QBittorrent => Some(LockSpec {
            names: &["lockfile"],
            levels_up: 2,
            process_names: &["qbittorrent"],
            absence_is_clean: false,
        }),
        MigrationSource::Deluge => Some(LockSpec {
            names: &["deluged.pid"],
            levels_up: 1,
            process_names: &["deluged", "deluge"],
            absence_is_clean: false,
        }),
        _ => None,
    }
}

/// Inspect `root` (the source state directory passed as `--from`).
pub fn detect_source_shutdown(source: MigrationSource, root: &Path) -> SourceShutdown {
    detect_source_shutdown_with(source, root, None)
}

/// Like [`detect_source_shutdown`], with an explicit lock or pid file. An
/// explicit path overrides the built-in search for every source; because the
/// operator named the file, its absence means the client exited cleanly.
pub fn detect_source_shutdown_with(
    source: MigrationSource,
    root: &Path,
    explicit_lock: Option<&Path>,
) -> SourceShutdown {
    let spec = lock_spec(source);
    let process_names: &[&str] = spec.as_ref().map_or(&[], |spec| spec.process_names);

    if let Some(path) = explicit_lock {
        return match read_lock(path) {
            LockRead::Absent => SourceShutdown::Clean,
            other => classify_read(other, process_names),
        };
    }
    let Some(spec) = spec else {
        return SourceShutdown::Unknown;
    };
    for dir in search_dirs(root, spec.levels_up) {
        for name in spec.names {
            let candidate = dir.join(name);
            match read_lock(&candidate) {
                LockRead::Absent => continue,
                other => return classify_read(other, spec.process_names),
            }
        }
    }
    if spec.absence_is_clean {
        SourceShutdown::Clean
    } else {
        SourceShutdown::Unknown
    }
}

/// `root` and up to `levels_up` ancestors, nearest first.
fn search_dirs(root: &Path, levels_up: usize) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    let mut current = root;
    for _ in 0..levels_up {
        match current.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                dirs.push(parent.to_path_buf());
                current = parent;
            }
            _ => break,
        }
    }
    dirs
}

enum LockRead {
    Absent,
    Text(String),
    /// Present but unreadable or oversized.
    Unreadable(String),
}

fn read_lock(path: &Path) -> LockRead {
    match std::fs::metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LockRead::Absent,
        Err(error) => {
            return LockRead::Unreadable(format!("cannot read {}: {error}", path.display()))
        }
    }
    match read_bounded(path) {
        Some(text) => LockRead::Text(text),
        None => LockRead::Unreadable(format!(
            "{} exists but could not be read as a small text file",
            path.display()
        )),
    }
}

fn classify_read(read: LockRead, process_names: &[&str]) -> SourceShutdown {
    match read {
        LockRead::Absent => SourceShutdown::Clean,
        LockRead::Unreadable(reason) => SourceShutdown::Crashed { reason },
        LockRead::Text(text) => classify_lock(
            &text,
            local_hostname().as_deref(),
            process_names,
            process_state,
        ),
    }
}

/// Parse a lock or pid file. Understands:
///
/// * rTorrent: one line, `hostname:+pid`;
/// * Qt `QLockFile` (qBittorrent): pid, application name and host name on
///   separate lines;
/// * plain pid files, optionally followed by `;` or whitespace and more text
///   (Deluge writes `pid;port`).
pub fn parse_lock(text: &str) -> Option<LockInfo> {
    let text = text.trim();
    let first_line = text.lines().next()?.trim();
    if first_line.is_empty() {
        return None;
    }
    // rTorrent: `host:+pid` on a single line whose head is not a number.
    if let Some((host, pid)) = first_line.rsplit_once(':') {
        let digits = pid.trim().trim_start_matches('+');
        if !host.trim().is_empty()
            && host.trim().parse::<u64>().is_err()
            && !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
        {
            let pid = digits.parse::<u32>().ok().filter(|pid| *pid != 0)?;
            return Some(LockInfo {
                pid,
                host: Some(host.trim().to_owned()),
                app: None,
            });
        }
    }
    // pid first: `1234`, `1234;8112`, `1234 rest`.
    let end = first_line
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(first_line.len());
    let pid = first_line[..end]
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid != 0)?;
    let mut rest = text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty());
    let app = rest.next().map(str::to_owned);
    let host = rest.next().map(str::to_owned);
    Some(LockInfo { pid, host, app })
}

/// Interpreters that legitimately host a client written in a scripting
/// language: a live pid with one of these names cannot be told apart from the
/// client, so it counts as running.
const INTERPRETERS: &[&str] = &["python", "java", "node", "ruby", "perl"];

fn classify_lock(
    text: &str,
    local_host: Option<&str>,
    process_names: &[&str],
    state_of: impl Fn(u32) -> ProcessState,
) -> SourceShutdown {
    let Some(info) = parse_lock(text) else {
        return SourceShutdown::Crashed {
            reason: "the lock file exists but is not in a recognized `host:+pid` or pid form"
                .to_owned(),
        };
    };
    if let (Some(host), Some(local)) = (info.host.as_deref(), local_host) {
        if !host.eq_ignore_ascii_case(local) {
            return SourceShutdown::Crashed {
                reason: format!(
                    "the lock belongs to host `{host}`, not this host; it cannot be checked and is treated as stale"
                ),
            };
        }
    }
    let pid = info.pid;
    match state_of(pid) {
        ProcessState::Running { name } => match name {
            Some(name) => {
                let lower = name.to_ascii_lowercase();
                let matches_client =
                    process_names.is_empty() || process_names.iter().any(|n| lower.contains(n));
                let ambiguous = INTERPRETERS.iter().any(|i| lower.contains(i));
                if matches_client || ambiguous {
                    SourceShutdown::Running { pid }
                } else {
                    SourceShutdown::Crashed {
                        reason: format!(
                            "the lock names pid {pid}, which is now `{name}`, not the source client"
                        ),
                    }
                }
            }
            // Alive but the name could not be read: assume it is the client,
            // because importing mid-write state is the worse mistake.
            None => SourceShutdown::Running { pid },
        },
        ProcessState::NotRunning => SourceShutdown::Crashed {
            reason: format!("the lock names pid {pid}, which is no longer running"),
        },
        ProcessState::Unknown => SourceShutdown::Crashed {
            reason: format!(
                "the lock names pid {pid}; process liveness cannot be checked on this platform, so the lock is treated as stale"
            ),
        },
    }
}

fn read_bounded(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_LOCK_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_LOCK_BYTES)
        .then(|| String::from_utf8(bytes).ok())
        .flatten()
}

/// The name of this host, as the clients record it.
fn local_hostname() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buffer = [0u8; 256];
        // SAFETY: `buffer` is a live byte buffer of the stated length.
        let rc = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
        if rc != 0 {
            return None;
        }
        let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
        let name = String::from_utf8_lossy(&buffer[..end]).trim().to_owned();
        (!name.is_empty()).then_some(name)
    }
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// Inspect a process by id.
pub fn process_state(pid: u32) -> ProcessState {
    #[cfg(target_os = "linux")]
    {
        let proc = Path::new("/proc").join(pid.to_string());
        if !proc.exists() {
            return ProcessState::NotRunning;
        }
        let name = std::fs::read_to_string(proc.join("comm"))
            .ok()
            .map(|comm| comm.trim().to_owned())
            .filter(|comm| !comm.is_empty());
        ProcessState::Running { name }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let Ok(raw) = i32::try_from(pid) else {
            return ProcessState::NotRunning;
        };
        // SAFETY: signal 0 only checks that the process exists.
        let rc = unsafe { libc::kill(raw, 0) };
        let exists = rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !exists {
            return ProcessState::NotRunning;
        }
        let name = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|text| {
                let text = text.trim();
                // `ps` may print a full path; keep the executable name.
                text.rsplit('/').next().unwrap_or(text).to_owned()
            })
            .filter(|name| !name.is_empty());
        ProcessState::Running { name }
    }
    #[cfg(windows)]
    {
        windows_process::state(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        ProcessState::Unknown
    }
}

#[cfg(windows)]
mod windows_process {
    use super::ProcessState;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;

    type Handle = *mut c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
        fn QueryFullProcessImageNameW(
            process: Handle,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    pub(super) fn state(pid: u32) -> ProcessState {
        // SAFETY: OpenProcess returns null on failure; the handle is closed below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            // No such process (ERROR_INVALID_PARAMETER) or no access. Either way
            // there is nothing we can show is running.
            return ProcessState::NotRunning;
        }
        let mut code = 0u32;
        // SAFETY: `handle` is a live process handle and `code` a live u32.
        let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
        if ok == 0 || code != STILL_ACTIVE {
            // SAFETY: closing the handle opened above.
            unsafe { CloseHandle(handle) };
            return ProcessState::NotRunning;
        }
        let mut buffer = vec![0u16; 1024];
        let mut size = buffer.len() as u32;
        // SAFETY: `buffer` has `size` UTF-16 units of capacity.
        let named =
            unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
        // SAFETY: closing the handle opened above.
        unsafe { CloseHandle(handle) };
        let name = (named != 0).then(|| {
            let full = std::ffi::OsString::from_wide(&buffer[..size as usize])
                .to_string_lossy()
                .into_owned();
            full.rsplit(['\\', '/']).next().unwrap_or(&full).to_owned()
        });
        ProcessState::Running { name }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive(name: &str) -> impl Fn(u32) -> ProcessState + '_ {
        move |_| ProcessState::Running {
            name: Some(name.to_owned()),
        }
    }

    /// A pid no platform will have allocated: past Linux `pid_max`, not a
    /// multiple of four (Windows), and within `i32`.
    const DEAD_PID: u32 = 2_000_000_003;

    // ---- parsing ---------------------------------------------------------

    #[test]
    fn rtorrent_locks_parse_with_and_without_the_plus() {
        assert_eq!(
            parse_lock("seedbox:+4242\n"),
            Some(LockInfo {
                pid: 4242,
                host: Some("seedbox".to_owned()),
                app: None
            })
        );
        assert_eq!(parse_lock("host.example.com:99").map(|l| l.pid), Some(99));
        assert_eq!(parse_lock("host:+0"), None);
        assert_eq!(parse_lock("host:notanumber"), None);
    }

    #[test]
    fn qt_lock_files_parse_pid_app_and_host_lines() {
        let info = parse_lock("12345\nqbittorrent\nmediabox\n").unwrap();
        assert_eq!(info.pid, 12345);
        assert_eq!(info.app.as_deref(), Some("qbittorrent"));
        assert_eq!(info.host.as_deref(), Some("mediabox"));
    }

    #[test]
    fn plain_pid_files_parse_including_deluge_pid_and_port() {
        assert_eq!(parse_lock("777\n").map(|l| l.pid), Some(777));
        assert_eq!(parse_lock("8080;58846").map(|l| l.pid), Some(8080));
        assert_eq!(parse_lock("31337 extra text").map(|l| l.pid), Some(31337));
    }

    #[test]
    fn a_pid_only_head_is_not_mistaken_for_a_host_pair() {
        // `1234:5678` has a numeric head, so it is a pid line, not host:pid.
        assert_eq!(parse_lock("1234:5678").map(|l| l.pid), Some(1234));
    }

    #[test]
    fn garbage_and_empty_locks_do_not_parse() {
        for text in ["", "   \n", "not a lock", "0", "-5", "abc:def"] {
            assert_eq!(parse_lock(text), None, "{text:?}");
        }
    }

    // ---- classification ---------------------------------------------------

    #[test]
    fn a_live_process_named_like_the_client_means_running() {
        let verdict = classify_lock("box:+10", Some("box"), &["rtorrent"], alive("rtorrent"));
        assert_eq!(verdict, SourceShutdown::Running { pid: 10 });
        assert!(!verdict.is_suspect());
        let qt = classify_lock(
            "10\nqbittorrent\nbox\n",
            Some("box"),
            &["qbittorrent"],
            alive("qbittorrent-nox"),
        );
        assert_eq!(qt, SourceShutdown::Running { pid: 10 });
    }

    #[test]
    fn a_reused_pid_owned_by_another_program_is_a_crash() {
        let verdict = classify_lock("10", None, &["qbittorrent"], alive("systemd"));
        match verdict {
            SourceShutdown::Crashed { reason } => assert!(reason.contains("systemd")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn interpreters_are_treated_as_possibly_the_client() {
        // deluged may show up as `python3`; refusing is safer than importing.
        let verdict = classify_lock("10", None, &["deluged"], alive("python3"));
        assert_eq!(verdict, SourceShutdown::Running { pid: 10 });
    }

    #[test]
    fn a_live_process_whose_name_cannot_be_read_counts_as_running() {
        let verdict = classify_lock("10", None, &["rtorrent"], |_| ProcessState::Running {
            name: None,
        });
        assert_eq!(verdict, SourceShutdown::Running { pid: 10 });
    }

    #[test]
    fn an_empty_name_list_accepts_any_live_process() {
        // `--source-lock` for a client TorrentNG has no process names for.
        let verdict = classify_lock("10", None, &[], alive("transmission-da"));
        assert_eq!(verdict, SourceShutdown::Running { pid: 10 });
    }

    #[test]
    fn a_gone_process_or_unknown_platform_is_a_crash() {
        for state in [ProcessState::NotRunning, ProcessState::Unknown] {
            let verdict = classify_lock("box:+10", Some("box"), &["rtorrent"], |_| state.clone());
            assert!(
                matches!(verdict, SourceShutdown::Crashed { .. }),
                "{state:?}"
            );
            assert!(verdict.is_suspect());
        }
    }

    #[test]
    fn a_lock_from_another_host_cannot_be_checked_and_is_stale() {
        let verdict = classify_lock("other:+10", Some("box"), &["rtorrent"], alive("rtorrent"));
        match verdict {
            SourceShutdown::Crashed { reason } => assert!(reason.contains("other")),
            other => panic!("unexpected {other:?}"),
        }
        // Host names compare case-insensitively.
        let same = classify_lock("BOX:+10", Some("box"), &["rtorrent"], alive("rtorrent"));
        assert_eq!(same, SourceShutdown::Running { pid: 10 });
    }

    #[test]
    fn an_unrecognized_lock_is_a_crash() {
        let verdict = classify_lock("garbage", Some("box"), &["rtorrent"], alive("rtorrent"));
        assert!(matches!(verdict, SourceShutdown::Crashed { .. }));
    }

    // ---- real process inspection -----------------------------------------

    #[test]
    fn this_process_is_seen_running_with_its_own_name() {
        match process_state(std::process::id()) {
            ProcessState::Running { name } => {
                if let Some(name) = name {
                    assert!(
                        name.to_ascii_lowercase().contains("rt_migrate"),
                        "unexpected process name {name}"
                    );
                }
            }
            ProcessState::Unknown => {} // a platform without process inspection
            ProcessState::NotRunning => panic!("the current process must be running"),
        }
    }

    #[test]
    fn a_pid_nobody_holds_is_not_running() {
        match process_state(DEAD_PID) {
            ProcessState::NotRunning | ProcessState::Unknown => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    // ---- detection on disk -------------------------------------------------

    #[test]
    fn rtorrent_absence_is_clean_and_other_sources_are_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_source_shutdown(MigrationSource::RTorrent, dir.path()),
            SourceShutdown::Clean
        );
        for source in [
            MigrationSource::QBittorrent,
            MigrationSource::Deluge,
            MigrationSource::Transmission,
            MigrationSource::UTorrent,
            MigrationSource::Generic,
        ] {
            assert_eq!(
                detect_source_shutdown(source, dir.path()),
                SourceShutdown::Unknown,
                "{source:?}: absence is not evidence of a clean exit"
            );
        }
    }

    #[test]
    fn a_stale_rtorrent_lock_is_a_crash_and_garbage_is_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rtorrent.lock"),
            format!("some-other-host:+{DEAD_PID}"),
        )
        .unwrap();
        assert!(detect_source_shutdown(MigrationSource::RTorrent, dir.path()).is_suspect());

        std::fs::write(dir.path().join("rtorrent.lock"), vec![b'x'; 4096]).unwrap();
        assert!(detect_source_shutdown(MigrationSource::RTorrent, dir.path()).is_suspect());
    }

    #[test]
    fn a_stale_qbittorrent_lockfile_is_found_in_a_parent_directory() {
        // --from often points at .../qBittorrent/BT_backup while the lock sits
        // beside it or one level higher.
        let root = tempfile::tempdir().unwrap();
        let backup = root.path().join("qBittorrent").join("BT_backup");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(
            root.path().join("qBittorrent").join("lockfile"),
            format!("{DEAD_PID}\nqbittorrent\nsome-other-host\n"),
        )
        .unwrap();
        let verdict = detect_source_shutdown(MigrationSource::QBittorrent, &backup);
        assert!(verdict.is_suspect(), "{verdict:?}");

        // Too far away to be found: nothing to say.
        let far = root.path().join("a").join("b").join("c").join("d");
        std::fs::create_dir_all(&far).unwrap();
        assert_eq!(
            detect_source_shutdown(MigrationSource::QBittorrent, &far),
            SourceShutdown::Unknown
        );
    }

    #[test]
    fn a_stale_deluge_pid_file_is_a_crash() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.path().join("deluged.pid"), format!("{DEAD_PID};58846")).unwrap();
        assert!(detect_source_shutdown(MigrationSource::Deluge, &state).is_suspect());
    }

    #[test]
    fn an_explicit_lock_works_for_any_source_and_absence_means_clean() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("transmission.pid");
        // Named but not there: the operator says this is where it lives, so the
        // client exited cleanly.
        assert_eq!(
            detect_source_shutdown_with(MigrationSource::Transmission, dir.path(), Some(&lock)),
            SourceShutdown::Clean
        );
        std::fs::write(&lock, DEAD_PID.to_string()).unwrap();
        assert!(detect_source_shutdown_with(
            MigrationSource::Transmission,
            dir.path(),
            Some(&lock)
        )
        .is_suspect());
    }

    #[test]
    fn an_explicit_lock_held_by_a_live_process_means_running() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("client.pid");
        std::fs::write(&lock, std::process::id().to_string()).unwrap();
        // Unknown source: no name list, so any live process counts.
        match detect_source_shutdown_with(MigrationSource::Generic, dir.path(), Some(&lock)) {
            SourceShutdown::Running { pid } => assert_eq!(pid, std::process::id()),
            // A platform that cannot inspect processes degrades to stale.
            SourceShutdown::Crashed { .. } if matches!(process_state(1), ProcessState::Unknown) => {
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn search_dirs_stops_at_the_filesystem_root() {
        let dirs = search_dirs(Path::new("/a/b"), 5);
        assert_eq!(dirs.first().map(|p| p.as_path()), Some(Path::new("/a/b")));
        assert!(dirs.len() <= 6);
        assert_eq!(search_dirs(Path::new("/a/b"), 0).len(), 1);
    }
}
