//! The hook shim.
//!
//! Claude Code spawns this binary on every hooked tool call, so it sits in the
//! critical path of the user's own work. It therefore does as close to nothing
//! as possible: read stdin, wrap it, write one frame, exit.
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

fn main() {
    // The return value is deliberately discarded: see the module docs.
    if let Err(error) = forward() {
        debug(&error);
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
    // Read one byte past the limit so an oversized payload is detected rather
    // than silently truncated into invalid JSON.
    let limit = u64::try_from(MAX_FRAME_BYTES).unwrap_or(u64::MAX);
    let mut buffer = String::new();
    std::io::stdin()
        .lock()
        .take(limit + 1)
        .read_to_string(&mut buffer)
        .map_err(|error| format!("read stdin: {error}"))?;

    if buffer.len() > MAX_FRAME_BYTES {
        return Err(format!(
            "payload of {} bytes exceeds the limit",
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
        let _ = writeln!(std::io::stderr(), "agentwatch-hook: {message}");
    }
}
