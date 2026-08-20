//! Repairing what the hooks missed.
//!
//! Hooks are the fast path: they arrive as the action happens and they carry
//! structure no other source has. They are also lossy — the daemon restarts, a
//! session began before it was running, and no hook reports token counts at
//! all. The transcript on disk is the durable record, so it is the correcting
//! path.
//!
//! Both paths write through the same idempotent insert. Events read from a
//! transcript carry deterministic ids and a unique key per model response, so a
//! session can be reconciled any number of times without a total moving.

use std::path::{Path, PathBuf};

use agentwatch_adapter_claude::{derived_transcript_path, read_token_usage, transcript_root};
use agentwatch_storage::{PendingSession, Store, StoreError};
use agentwatch_types::Timestamp;

/// Sessions examined per sweep.
///
/// A bound rather than "everything": a first run against months of history
/// should not hold the write connection for minutes.
const SWEEP_LIMIT: u32 = 200;

/// What one reconcile pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Sessions examined.
    pub sessions: u64,
    /// Sessions whose transcript could not be found.
    pub missing_transcripts: u64,
    /// Distinct model responses read.
    pub responses: u64,
    /// Rows actually written, after deduplication against what was stored.
    pub written: u64,
}

/// Reads one session's transcript and stores whatever it reports.
///
/// # Errors
///
/// Returns an error if the database rejects the write. A missing or unreadable
/// transcript is not an error — it is the normal state for a session whose file
/// has been cleaned up, and is counted rather than raised.
pub fn reconcile_session(
    store: &mut Store,
    session: &PendingSession,
    root: &Path,
) -> Result<ReconcileReport, StoreError> {
    let mut report = ReconcileReport {
        sessions: 1,
        ..ReconcileReport::default()
    };

    let Some(path) = locate_transcript(session, root) else {
        report.missing_transcripts = 1;
        return Ok(report);
    };

    let Ok((events, summary)) = read_token_usage(&path) else {
        report.missing_transcripts = 1;
        return Ok(report);
    };

    report.responses = summary.responses;
    report.written = store.insert_events(&events)? as u64;

    if let Some(inflation) = summary.record_inflation()
        && inflation > 1.0
    {
        tracing::debug!(
            session = %session.external_session_id,
            records = summary.usage_records,
            responses = summary.responses,
            inflation = format!("{inflation:.2}x"),
            "collapsed duplicate transcript records"
        );
    }

    store.mark_reconciled(&session.session_id, Timestamp::now().as_micros())?;
    Ok(report)
}

/// Finds a session's transcript.
///
/// Prefers the path the agent reported at `SessionStart`. The derived path is
/// the fallback for sessions whose start was never seen, and depends on an
/// undocumented naming rule, so it is only used when the reported one is absent
/// or gone.
fn locate_transcript(session: &PendingSession, root: &Path) -> Option<PathBuf> {
    if let Some(reported) = session.transcript_path.as_deref() {
        let path = PathBuf::from(reported);
        if path.is_file() {
            return Some(path);
        }
    }

    let project = session.project_path.as_deref()?;
    let derived = derived_transcript_path(root, project, &session.external_session_id);
    derived.is_file().then_some(derived)
}

/// Reconciles every session that has not been read yet.
///
/// # Errors
///
/// Returns an error if the database cannot be queried or written.
pub fn sweep(store: &mut Store) -> Result<ReconcileReport, StoreError> {
    let Ok(root) = transcript_root() else {
        tracing::warn!("cannot locate the transcript directory; skipping reconcile");
        return Ok(ReconcileReport::default());
    };

    let pending = store.sessions_awaiting_reconcile(SWEEP_LIMIT)?;
    let mut report = ReconcileReport::default();

    for session in &pending {
        let one = reconcile_session(store, session, &root)?;
        report.sessions += one.sessions;
        report.missing_transcripts += one.missing_transcripts;
        report.responses += one.responses;
        report.written += one.written;
    }

    if report.sessions > 0 {
        tracing::info!(
            sessions = report.sessions,
            responses = report.responses,
            written = report.written,
            missing = report.missing_transcripts,
            "reconciled sessions against their transcripts"
        );
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{AgentEvent, Event, EvidenceSource, SessionEnded, SessionStarted};
    use agentwatch_types::{AgentId, ExternalSessionId};

    use super::*;

    const TRANSCRIPT: &str = r#"
{"type":"assistant","requestId":"req_1","timestamp":"2026-08-20T17:22:02.051Z","sessionId":"s-1","cwd":"/work","message":{"id":"msg_1","model":"claude-opus-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":200,"output_tokens":50}}}
{"type":"assistant","requestId":"req_1","timestamp":"2026-08-20T17:22:02.051Z","sessionId":"s-1","cwd":"/work","message":{"id":"msg_1","model":"claude-opus-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":200,"output_tokens":50}}}
{"type":"assistant","requestId":"req_2","timestamp":"2026-08-20T17:23:02.051Z","sessionId":"s-1","cwd":"/work","message":{"id":"msg_2","model":"claude-opus-5","usage":{"input_tokens":1,"cache_creation_input_tokens":10,"cache_read_input_tokens":20,"output_tokens":5}}}
"#;

    /// Builds a store holding one ended session, plus the transcript on disk.
    fn fixture() -> (Store, tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("projects");
        let project = root.join("-work");
        std::fs::create_dir_all(&project).expect("create");
        std::fs::write(project.join("s-1.jsonl"), TRANSCRIPT).expect("write");

        let mut store = Store::open_in_memory().expect("schema");
        let external = ExternalSessionId::from("s-1".to_owned());
        store
            .insert_events(&[
                AgentEvent::observed(
                    AgentId::CLAUDE_CODE,
                    EvidenceSource::Hook,
                    Event::SessionStarted(SessionStarted::default()),
                )
                .with_session(external.clone())
                .with_project_path("/work".to_owned()),
                AgentEvent::observed(
                    AgentId::CLAUDE_CODE,
                    EvidenceSource::Hook,
                    Event::SessionEnded(SessionEnded::default()),
                )
                .with_session(external)
                .with_project_path("/work".to_owned()),
            ])
            .expect("insert");

        (store, directory, root)
    }

    fn stored_output(store: &Store) -> i64 {
        store.token_totals(0, i64::MAX).expect("totals").output
    }

    #[test]
    fn a_sweep_reads_token_usage_out_of_the_transcript() {
        let (mut store, _guard, root) = fixture();
        let pending = store.sessions_awaiting_reconcile(10).expect("pending");
        let report = reconcile_session(&mut store, &pending[0], &root).expect("reconcile");

        assert_eq!(report.responses, 2, "three records are two responses");
        assert_eq!(report.written, 2);
        assert_eq!(stored_output(&store), 55);
    }

    #[test]
    fn reconciling_twice_does_not_move_the_totals() {
        let (mut store, _guard, root) = fixture();
        let pending = store.sessions_awaiting_reconcile(10).expect("pending");

        reconcile_session(&mut store, &pending[0], &root).expect("first");
        let before = stored_output(&store);

        // Force it back into the queue the way a crash mid-write would.
        store
            .connection_for_test()
            .execute("UPDATE sessions SET reconciled_at_us = NULL", [])
            .expect("reset");
        let pending = store.sessions_awaiting_reconcile(10).expect("pending");
        let second = reconcile_session(&mut store, &pending[0], &root).expect("second");

        assert_eq!(second.written, 0, "nothing new should be written");
        assert_eq!(stored_output(&store), before);
    }

    #[test]
    fn an_ended_session_stops_being_pending_once_reconciled() {
        let (mut store, _guard, root) = fixture();
        sweep_with_root(&mut store, &root);
        assert!(
            store
                .sessions_awaiting_reconcile(10)
                .expect("pending")
                .is_empty()
        );
    }

    #[test]
    fn a_missing_transcript_is_counted_not_raised() {
        let (mut store, _guard, _root) = fixture();
        let pending = store.sessions_awaiting_reconcile(10).expect("pending");
        let elsewhere = PathBuf::from("/nonexistent/projects");

        let report = reconcile_session(&mut store, &pending[0], &elsewhere).expect("no error");
        assert_eq!(report.missing_transcripts, 1);
        assert_eq!(report.responses, 0);
    }

    /// Test-only stand-in for [`sweep`], which reads the real home directory.
    fn sweep_with_root(store: &mut Store, root: &Path) {
        let pending = store
            .sessions_awaiting_reconcile(SWEEP_LIMIT)
            .expect("pending");
        for session in &pending {
            reconcile_session(store, session, root).expect("reconcile");
        }
    }
}
