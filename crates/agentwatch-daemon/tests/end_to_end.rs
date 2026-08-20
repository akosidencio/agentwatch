//! The phase 1 exit criterion: a hook payload arrives on the socket and a row
//! lands in SQLite.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use agentwatch_daemon::{Daemon, DaemonConfig};
use agentwatch_events::{PROTOCOL_VERSION, encode_frame};
use agentwatch_storage::Store;
use agentwatch_types::{Paths, Timestamp};

/// Longest a test waits for the pipeline to drain before failing.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Builds a hook envelope around a raw Claude payload.
fn envelope(payload: &str) -> Vec<u8> {
    let raw = serde_json::value::RawValue::from_string(payload.to_owned()).expect("valid json");
    let envelope = serde_json::json!({
        "v": PROTOCOL_VERSION,
        "source": "claude-code",
        "sent_at": Timestamp::now().as_micros(),
        "hook_version": "test",
        "payload": raw,
    });
    serde_json::to_vec(&envelope).expect("serializable")
}

/// Connects to the daemon socket, retrying while it starts up.
async fn connect(paths: &Paths) -> tokio::net::UnixStream {
    let deadline = tokio::time::Instant::now() + SETTLE_TIMEOUT;
    loop {
        match tokio::net::UnixStream::connect(paths.socket()).await {
            Ok(stream) => return stream,
            Err(error) if tokio::time::Instant::now() >= deadline => {
                panic!("daemon never accepted connections: {error}")
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

/// Polls the database until it holds `expected` events, or the deadline passes.
async fn wait_for_events(paths: &Paths, expected: i64) -> i64 {
    let deadline = tokio::time::Instant::now() + SETTLE_TIMEOUT;
    loop {
        let count = Store::open_read_only(paths.database())
            .ok()
            .and_then(|store| store.totals().ok())
            .map_or(0, |totals| totals.events);

        if count >= expected || tokio::time::Instant::now() >= deadline {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn a_hook_payload_becomes_a_stored_event() {
    let directory = tempfile::tempdir().expect("temp dir");
    let paths = Paths::with_root(directory.path());

    let daemon = tokio::spawn(Daemon::new(DaemonConfig::new(paths.clone())).run());

    {
        let mut stream = connect(&paths).await;
        let body = envelope(
            r#"{"hook_event_name":"PostToolUse","session_id":"s-1","cwd":"/work/acme",
                "tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
        );

        let mut framed = Vec::new();
        encode_frame(&mut framed, &body).expect("frame");
        tokio::io::AsyncWriteExt::write_all(&mut stream, &framed)
            .await
            .expect("write");
        tokio::io::AsyncWriteExt::shutdown(&mut stream)
            .await
            .expect("shutdown");
    }

    assert_eq!(
        wait_for_events(&paths, 1).await,
        1,
        "the event never reached storage"
    );

    let store = Store::open_read_only(paths.database()).expect("open");
    let rows = store.recent_events(10).expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "command");
    assert_eq!(rows[0].evidence, "hook");
    assert_eq!(rows[0].project_path.as_deref(), Some("/work/acme"));
    assert!(rows[0].payload.contains("cargo test"));

    let totals = store.totals().expect("totals");
    assert_eq!(totals.sessions, 1, "the event should have opened a session");
    assert_eq!(totals.projects, 1);

    daemon.abort();
}

#[tokio::test]
async fn many_payloads_on_one_connection_all_land() {
    let directory = tempfile::tempdir().expect("temp dir");
    let paths = Paths::with_root(directory.path());

    let daemon = tokio::spawn(Daemon::new(DaemonConfig::new(paths.clone())).run());

    let total = 500;
    {
        let mut stream = connect(&paths).await;
        let mut framed = Vec::new();
        for index in 0..total {
            let body = envelope(&format!(
                r#"{{"hook_event_name":"PostToolUse","session_id":"s-1",
                     "tool_name":"Bash","tool_input":{{"command":"step-{index}"}}}}"#
            ));
            encode_frame(&mut framed, &body).expect("frame");
        }
        tokio::io::AsyncWriteExt::write_all(&mut stream, &framed)
            .await
            .expect("write");
        tokio::io::AsyncWriteExt::shutdown(&mut stream)
            .await
            .expect("shutdown");
    }

    assert_eq!(
        wait_for_events(&paths, total).await,
        total,
        "batching should not lose events"
    );

    daemon.abort();
}

#[tokio::test]
async fn a_malformed_frame_does_not_stop_the_frames_after_it() {
    let directory = tempfile::tempdir().expect("temp dir");
    let paths = Paths::with_root(directory.path());

    let daemon = tokio::spawn(Daemon::new(DaemonConfig::new(paths.clone())).run());

    {
        let mut stream = connect(&paths).await;
        let mut framed = Vec::new();

        encode_frame(&mut framed, b"{\"not\":\"an envelope\"}").expect("frame");
        encode_frame(
            &mut framed,
            &envelope(r#"{"hook_event_name":"SessionStart"}"#),
        )
        .expect("frame");

        tokio::io::AsyncWriteExt::write_all(&mut stream, &framed)
            .await
            .expect("write");
        tokio::io::AsyncWriteExt::shutdown(&mut stream)
            .await
            .expect("shutdown");
    }

    assert_eq!(
        wait_for_events(&paths, 1).await,
        1,
        "the good frame after the bad one should still be stored"
    );

    daemon.abort();
}

#[tokio::test]
async fn an_over_long_socket_path_fails_with_an_actionable_message() {
    let directory = tempfile::tempdir().expect("temp dir");
    let deep = directory.path().join("d".repeat(120));
    std::fs::create_dir_all(&deep).expect("create");

    let error = Daemon::new(DaemonConfig::new(Paths::with_root(&deep)))
        .run()
        .await
        .expect_err("an over-long socket path must fail");

    let message = format!("{error:#}");
    assert!(
        message.contains("AGENTWATCH_DIR"),
        "unhelpful message: {message}"
    );
}

#[tokio::test]
async fn a_second_daemon_refuses_to_steal_the_socket() {
    let directory = tempfile::tempdir().expect("temp dir");
    let paths = Paths::with_root(directory.path());

    let first = tokio::spawn(Daemon::new(DaemonConfig::new(paths.clone())).run());
    drop(connect(&paths).await);

    let second = Daemon::new(DaemonConfig::new(paths.clone())).run().await;
    assert!(
        second.is_err(),
        "a second daemon must not bind the same socket"
    );

    first.abort();
}
