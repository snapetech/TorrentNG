//! End-to-end crash detection against the real `torrentngd` binary.
//!
//! The engine tests simulate an unclean previous run by editing the run marker.
//! These start the actual daemon process, kill it the hard way, restart it and
//! read the verdict back through the real HTTP API, which is the path an
//! operator (and the WebUI) actually sees. Unix only: they rely on `SIGKILL`
//! and `SIGTERM`.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Daemon {
    dir: tempfile::TempDir,
    api_port: u16,
    child: Option<Child>,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Daemon {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let api_port = free_port();
        let listen_port = free_port();
        let session = dir.path().join("session");
        let config = format!(
            r#"
[daemon]
api_bind = "127.0.0.1:{api_port}"
session_dir = "{session}"
log_level = "warn"
shutdown_timeout_secs = 5

[network]
listen_port = {listen_port}

[storage]
download_dir = "{downloads}"

[dht]
enabled = false

[db]
path = "{db}"
"#,
            session = session.display(),
            downloads = dir.path().join("downloads").display(),
            db = dir.path().join("state.db").display(),
        );
        std::fs::write(dir.path().join("config.toml"), config).unwrap();
        Daemon {
            dir,
            api_port,
            child: None,
        }
    }

    fn marker_path(&self) -> PathBuf {
        self.dir.path().join("session").join("run_marker.json")
    }

    fn start(&mut self) {
        assert!(self.child.is_none(), "daemon already running");
        let log = std::fs::File::create(self.dir.path().join("daemon.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_torrentngd"))
            .env("TORRENTNGD_CONFIG", self.dir.path().join("config.toml"))
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn torrentngd");
        self.child = Some(child);
        self.wait_ready();
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some((200, _)) = http_get(self.api_port, "/health") {
                return;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("daemon exited early ({status}):\n{}", self.log());
            }
            assert!(
                Instant::now() < deadline,
                "daemon never became ready:\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("daemon.log")).unwrap_or_default()
    }

    /// `SIGKILL`: no shutdown code runs, exactly like a crash.
    fn kill_hard(&mut self) {
        let mut child = self.child.take().expect("running");
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
    }

    /// `SIGTERM` and wait for the orderly shutdown to finish.
    fn stop_gracefully(&mut self) {
        let mut child = self.child.take().expect("running");
        // SAFETY: signalling a child process we own.
        let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        assert_eq!(rc, 0);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "unclean exit {status}:\n{}", self.log());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not stop:\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn crash_safety(&self) -> Value {
        let (status, body) =
            http_get(self.api_port, "/api/v1/settings/crash-safety").expect("api reachable");
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    fn verdict(&self) -> String {
        self.crash_safety()["report"]["previous_run"]["verdict"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn event_kinds(&self) -> Vec<String> {
        let (_, body) = http_get(self.api_port, "/api/v1/logs?limit=200").expect("api reachable");
        let value: Value = serde_json::from_str(&body).unwrap();
        value["logs"]
            .as_array()
            .map(|logs| {
                logs.iter()
                    .filter_map(|log| log["kind"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A minimal HTTP/1.1 GET over a plain socket (`Connection: close`).
fn http_get(port: u16, path: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_secs(2),
    )
    .ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let (head, body) = raw.split_once("\r\n\r\n")?;
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_owned()
    };
    Some((status, body))
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, tail)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size_line.trim(), 16) else {
            break;
        };
        if size == 0 || tail.len() < size {
            break;
        }
        out.push_str(&tail[..size]);
        rest = tail[size..].trim_start_matches("\r\n");
    }
    out
}

/// Whether this platform can tell a process crash from a host crash.
fn has_boot_identity() -> bool {
    cfg!(target_os = "linux")
}

fn read_marker(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("marker exists")).unwrap()
}

#[test]
fn a_hard_killed_daemon_is_recognized_as_a_crash_on_the_next_start() {
    let mut daemon = Daemon::new();

    // First start: nothing to compare with.
    daemon.start();
    assert_eq!(daemon.verdict(), "no_record");
    assert_eq!(daemon.crash_safety()["report"]["detection_active"], true);
    assert_eq!(read_marker(&daemon.marker_path())["graceful"], false);

    // Killed with no chance to clean up: the marker is still open.
    daemon.kill_hard();
    assert_eq!(read_marker(&daemon.marker_path())["graceful"], false);

    // Restart. Same boot, so a process crash on Linux; other platforms cannot
    // rule out a power loss.
    daemon.start();
    let expected = if has_boot_identity() {
        "process_crash"
    } else {
        "unclean_unknown_cause"
    };
    assert_eq!(daemon.verdict(), expected, "{}", daemon.log());
    let view = daemon.crash_safety();
    assert!(view["report"]["previous_run"]["crash_reference_unix"].is_u64());
    assert!(
        daemon.event_kinds().contains(&"crash_recovery".to_owned()),
        "an unclean previous run must be logged for operators"
    );
}

#[test]
fn an_orderly_shutdown_is_clean_and_leaves_a_closed_marker() {
    let mut daemon = Daemon::new();
    daemon.start();
    daemon.stop_gracefully();
    assert_eq!(read_marker(&daemon.marker_path())["graceful"], true);

    daemon.start();
    assert_eq!(daemon.verdict(), "clean", "{}", daemon.log());
    assert!(!daemon.event_kinds().contains(&"crash_recovery".to_owned()));

    // A clean start reopens the marker for the new run.
    assert_eq!(read_marker(&daemon.marker_path())["graceful"], false);
}

#[test]
fn a_marker_left_open_by_a_different_boot_is_a_host_crash() {
    if !has_boot_identity() {
        eprintln!("no boot identity on this platform; nothing to distinguish");
        return;
    }
    let mut daemon = Daemon::new();
    daemon.start();
    daemon.stop_gracefully();

    // Power loss: the previous run never closed its marker, and the machine has
    // rebooted since (a different boot identity).
    let mut marker = read_marker(&daemon.marker_path());
    marker["graceful"] = Value::Bool(false);
    marker["boot_id"] = Value::String("boot-before-the-power-cut".to_owned());
    std::fs::write(daemon.marker_path(), serde_json::to_vec(&marker).unwrap()).unwrap();

    daemon.start();
    assert_eq!(daemon.verdict(), "host_crash", "{}", daemon.log());
    let view = daemon.crash_safety();
    assert_eq!(
        view["report"]["previous_run"]["previous_boot_id"],
        "boot-before-the-power-cut"
    );
    assert!(daemon.event_kinds().contains(&"crash_recovery".to_owned()));
}

#[test]
fn a_corrupt_marker_is_treated_as_an_unclean_shutdown_not_a_startup_failure() {
    let mut daemon = Daemon::new();
    daemon.start();
    daemon.stop_gracefully();
    // A torn write, as a crash during a marker update would leave.
    std::fs::write(daemon.marker_path(), b"{\"version\":1,\"boot_id\":").unwrap();

    daemon.start();
    assert_eq!(
        daemon.verdict(),
        "unclean_unknown_cause",
        "{}",
        daemon.log()
    );
}
