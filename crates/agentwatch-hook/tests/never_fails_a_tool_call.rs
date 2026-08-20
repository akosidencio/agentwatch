//! The invariant that outranks everything else: the hook exits 0 no matter
//! what, and does it fast enough that nobody notices it ran.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Read as _;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use agentwatch_events::decode_frame_len;

/// Path to the binary under test, supplied by Cargo.
const HOOK_BINARY: &str = env!("CARGO_BIN_EXE_agentwatch-hook");

/// The ceiling the design depends on. Above this, the collection architecture
/// has to change, so the test asserts it rather than trusting it.
const LATENCY_BUDGET: Duration = Duration::from_millis(30);

/// A representative payload.
const PAYLOAD: &str = r#"{"hook_event_name":"PostToolUse","session_id":"s-1",
    "tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;

/// Runs the hook with the given data directory and stdin, returning its status.
fn run_hook(data_dir: &std::path::Path, stdin: &str) -> std::process::ExitStatus {
    let mut child = Command::new(HOOK_BINARY)
        .env("AGENTWATCH_DIR", data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hook");

    {
        use std::io::Write as _;
        let mut pipe = child.stdin.take().expect("stdin piped");
        let _ = pipe.write_all(stdin.as_bytes());
    }

    child.wait().expect("hook exits")
}

#[test]
fn exits_zero_when_no_daemon_is_listening() {
    let directory = tempfile::tempdir().expect("temp dir");
    let status = run_hook(directory.path(), PAYLOAD);
    assert!(status.success(), "the hook must never fail a tool call");
}

#[test]
fn exits_zero_on_a_malformed_payload() {
    let directory = tempfile::tempdir().expect("temp dir");
    assert!(run_hook(directory.path(), "not json at all").success());
}

#[test]
fn exits_zero_on_an_empty_payload() {
    let directory = tempfile::tempdir().expect("temp dir");
    assert!(run_hook(directory.path(), "").success());
}

#[test]
fn exits_zero_when_the_data_directory_does_not_exist() {
    let directory = tempfile::tempdir().expect("temp dir");
    let missing = directory.path().join("nowhere").join("deeper");
    assert!(run_hook(&missing, PAYLOAD).success());
}

#[test]
fn delivers_a_frame_the_daemon_can_decode() {
    let directory = tempfile::tempdir().expect("temp dir");
    let socket = directory.path().join("agentwatch.sock");
    let listener = UnixListener::bind(&socket).expect("bind");

    let (sender, receiver) = mpsc::channel();
    let accepting = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buffer = Vec::new();
        stream.read_to_end(&mut buffer).expect("read");
        let _ = sender.send(buffer);
    });

    assert!(run_hook(directory.path(), PAYLOAD).success());
    let frame = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("frame delivered");
    accepting.join().expect("accept thread");

    let header: [u8; 4] = frame[..4].try_into().expect("header");
    let length = decode_frame_len(header).expect("valid header");
    assert_eq!(
        frame.len(),
        4 + length,
        "frame length must match its header"
    );

    let envelope: serde_json::Value = serde_json::from_slice(&frame[4..]).expect("body is json");
    assert_eq!(envelope["source"], "claude-code");
    assert_eq!(envelope["payload"]["tool_name"], "Bash");
}

#[test]
fn stays_inside_the_latency_budget_when_the_daemon_is_absent() {
    let directory = tempfile::tempdir().expect("temp dir");

    // Warm the page cache so the measurement is of the hook, not of the first
    // load of the binary off disk.
    let _ = run_hook(directory.path(), PAYLOAD);

    let mut slowest = Duration::ZERO;
    for _ in 0..10 {
        let started = Instant::now();
        assert!(run_hook(directory.path(), PAYLOAD).success());
        slowest = slowest.max(started.elapsed());
    }

    assert!(
        slowest < LATENCY_BUDGET,
        "worst hook round trip was {slowest:?}, budget is {LATENCY_BUDGET:?}"
    );
}
