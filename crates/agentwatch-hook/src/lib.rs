//! The hook shim.
//!
//! Claude Code spawns this on every hooked tool call, so it sits in the
//! critical path of the user's own work. It therefore does as close to nothing
//! as possible: read stdin, wrap it, write one frame, exit.
//!
//! A library rather than its own binary since 0.2: everything ships as one
//! `agentwatch` executable, and this is what `agentwatch hook` runs. The rule
//! below is unchanged by that and unchanged by anything else linked into the
//! same executable — nothing here touches the database, the runtime, or the
//! terminal, and nothing here is allowed to start doing so.
//!
//! # The rule that outranks every other consideration
//!
//! **This process exits 0 on every path.** A monitoring tool that can fail a
//! tool call is worse than no monitoring tool. If the daemon is down, the
//! socket is missing, the payload is malformed, or the write times out, the
//! event is dropped silently and the agent carries on.
//!
//! Nothing is written to stderr by default either, because an agent may surface
//! hook output to the user. Set `AGENTWATCH_HOOK_DEBUG=1` to see failures.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use agentwatch_events::{HookEnvelopeRef, MAX_FRAME_BYTES, PROTOCOL_VERSION, encode_frame};
use agentwatch_types::{Paths, Timestamp};
use serde_json::value::RawValue;

/// This binary's version, reported in every envelope.
const HOOK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default source name, overridable so the same shim can serve other agents.
const DEFAULT_SOURCE: &str = "claude-code";

/// Environment variable that names the source.
const SOURCE_ENV: &str = "AGENTWATCH_SOURCE";

/// Environment variable that turns on stderr diagnostics.
const DEBUG_ENV: &str = "AGENTWATCH_HOOK_DEBUG";

/// How long to wait on the socket before giving up.
///
/// Generous for a local socket and still an order of magnitude below the point
/// where a user would notice. If the daemon is wedged, we drop the event.
const SOCKET_TIMEOUT: Duration = Duration::from_millis(250);

/// Hard ceiling on the whole forward, whatever it is stuck on.
///
/// [`SOCKET_TIMEOUT`] covers the write, but `connect` has no timeout of its own
/// and blocks if the daemon has stopped accepting with a full backlog. Running
/// the work on a thread and abandoning it at the deadline is what makes the
/// bound total rather than per-syscall — without it the one failure mode the
/// module promises to survive is the one that stalls a tool call.
const TOTAL_BUDGET: Duration = Duration::from_millis(500);

/// Bytes reserved for the envelope wrapped around the payload.
///
/// The frame carries the envelope, not the payload, so the payload limit has to
/// leave room for the fields around it. Generously over the ~120 bytes it
/// actually takes, so an oversized payload is refused here with a message that
/// says so rather than failing later as an opaque write error.
const ENVELOPE_HEADROOM: usize = 4096;

/// Forwards one hook payload from stdin to the daemon, then returns.
///
/// Returns rather than exiting, so the caller keeps ownership of the exit code
/// — which, per the module docs, must be 0 on every path.
pub fn run() {
    // The return value is deliberately discarded: see the module docs.
    //
    // The work runs on a thread so a blocked syscall cannot outlive the budget.
    // Returning from `main` exits the process and takes the thread with it,
    // which is the point: the agent is waiting on this process, not on us
    // finishing tidily.
    let (finished, outcome) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = finished.send(forward());
    });

    match outcome.recv_timeout(TOTAL_BUDGET) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => debug(&error),
        Err(_) => debug("timed out; dropping the event"),
    }
}

/// Reads one payload from stdin and forwards it to the daemon.
fn forward() -> Result<(), String> {
    let payload = read_stdin()?;
    let raw = RawValue::from_string(payload).map_err(|error| format!("invalid json: {error}"))?;

    let envelope = HookEnvelopeRef {
        v: PROTOCOL_VERSION,
        source: &source_name(),
        sent_at: Timestamp::now(),
        hook_version: HOOK_VERSION,
        payload: &raw,
    };

    let body = serde_json::to_vec(&envelope).map_err(|error| format!("encode: {error}"))?;

    let paths = Paths::from_env().map_err(|error| error.to_string())?;
    let mut socket =
        UnixStream::connect(paths.socket()).map_err(|error| format!("connect: {error}"))?;
    socket
        .set_write_timeout(Some(SOCKET_TIMEOUT))
        .map_err(|error| format!("timeout: {error}"))?;

    encode_frame(&mut socket, &body).map_err(|error| format!("write: {error}"))?;
    socket.flush().map_err(|error| format!("flush: {error}"))?;

    Ok(())
}

/// Reads the payload, refusing anything that could not be a real hook payload.
fn read_stdin() -> Result<String, String> {
    // The frame carries the envelope, so the payload gets the frame limit minus
    // the room the envelope needs around it. Checking against the frame limit
    // itself accepts payloads the encoder then rejects.
    let max_payload = MAX_FRAME_BYTES - ENVELOPE_HEADROOM;

    // Read one byte past the limit so an oversized payload is detected rather
    // than silently truncated into invalid JSON.
    let limit = u64::try_from(max_payload).unwrap_or(u64::MAX);
    let mut buffer = String::new();
    std::io::stdin()
        .lock()
        .take(limit + 1)
        .read_to_string(&mut buffer)
        .map_err(|error| format!("read stdin: {error}"))?;

    if buffer.len() > max_payload {
        return Err(format!(
            "payload of {} bytes exceeds the {max_payload} byte limit",
            buffer.len()
        ));
    }
    if buffer.trim().is_empty() {
        return Err("empty payload".to_owned());
    }

    Ok(buffer)
}

/// The source name to stamp on the envelope.
fn source_name() -> String {
    std::env::var(SOURCE_ENV).unwrap_or_else(|_| DEFAULT_SOURCE.to_owned())
}

/// Reports a failure, but only when explicitly asked to.
fn debug(message: &str) {
    if std::env::var_os(DEBUG_ENV).is_some() {
        let _ = writeln!(std::io::stderr(), "agentwatch hook: {message}");
    }
}
