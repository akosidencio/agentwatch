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
use std::time::Duration;

use agentwatch_adapter_claude::{derived_transcript_path, read_token_usage, transcript_root};
use agentwatch_adapter_codex::{find_rollouts, read_rollout, rollout_root};
use agentwatch_storage::{PendingSession, Store, StoreError};
use agentwatch_types::{RepositoryResolver, Timestamp};

/// Sessions examined per sweep.
///
/// A bound rather than "everything": a first run against months of history
/// should not hold the write connection for minutes.
const SWEEP_LIMIT: u32 = 200;

/// How long a transcript must sit untouched before its session is finished.
///
/// Only an observed `SessionEnd` proves a session is over. For everything else
/// — imported history, sessions that began before the daemon did — the file's
/// own stillness is the available evidence. An hour is far longer than any gap
/// within a live session and far shorter than the age of imported history, so
/// it retires the backlog without freezing a session that is still running.
const TRANSCRIPT_IDLE: Duration = Duration::from_secs(3_600);

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
    /// Sessions that will never be read again.
    pub finished: u64,
    /// Codex rollout files examined.
    pub codex_rollouts: u64,
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

    let now = Timestamp::now().as_micros();

    // Recorded even when there is nothing to read, so a session whose
    // transcript is gone moves to the back of the queue instead of being
    // re-examined ahead of everything else on every sweep.
    let Some(path) = locate_transcript(session, root) else {
        report.missing_transcripts = 1;
        store.mark_reconcile_attempted(&session.session_id, now)?;
        return Ok(report);
    };

    let Ok((events, summary)) = read_token_usage(&path) else {
        report.missing_transcripts = 1;
        store.mark_reconcile_attempted(&session.session_id, now)?;
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

    if is_finished(session, &path) {
        report.finished = 1;
        store.mark_reconciled(&session.session_id, now)?;
    } else {
        store.mark_reconcile_attempted(&session.session_id, now)?;
    }

    Ok(report)
}

/// Whether a session's transcript has stopped growing.
///
/// An observed end settles it. Otherwise the file's own modification time is
/// the evidence: a session nobody watched start cannot be declared over on the
/// strength of its database row, and marking it done would freeze its totals
/// mid-flight — but leaving it pending forever means its transcript is re-read
/// on every sweep for the life of the installation.
fn is_finished(session: &PendingSession, transcript: &Path) -> bool {
    if session.status == "ended" {
        return true;
    }

    std::fs::metadata(transcript)
        .and_then(|metadata| metadata.modified())
        .and_then(|modified| {
            std::time::SystemTime::now()
                .duration_since(modified)
                .map_err(|_| std::io::Error::other("transcript is modified in the future"))
        })
        .is_ok_and(|idle| idle >= TRANSCRIPT_IDLE)
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
    sweep_with_local_history(store, true)
}

/// Reconciles durable sources, optionally excluding agent-home discovery.
///
/// Explicitly configured daemon instances use `false` to remain isolated from
/// the host user's history. The normal CLI daemon always uses `true`.
pub(crate) fn sweep_with_local_history(
    store: &mut Store,
    include_agent_homes: bool,
) -> Result<ReconcileReport, StoreError> {
    let mut report = ReconcileReport::default();
    if let Ok(root) = transcript_root() {
        let pending = store.sessions_awaiting_reconcile(SWEEP_LIMIT)?;
        for session in &pending {
            let one = reconcile_session(store, session, &root)?;
            report.sessions += one.sessions;
            report.missing_transcripts += one.missing_transcripts;
            report.responses += one.responses;
            report.written += one.written;
            report.finished += one.finished;
        }
    } else {
        tracing::warn!("cannot locate the Claude transcript directory; skipping Claude reconcile");
    }

    // Codex has no hook yet, so its durable rollout is both fast and repairing
    // path. Re-reading is safe: every normalized event id and usage key is
    // deterministic, including for the rollout currently being appended.
    if include_agent_homes && let Ok(root) = rollout_root() {
        for path in find_rollouts(&root) {
            report.codex_rollouts += 1;
            match read_rollout(&path) {
                Ok((events, summary)) => {
                    report.responses += summary.responses;
                    report.written += store.insert_events(&events)? as u64;
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), ?error, "Codex rollout could not be read")
                }
            }
        }
    }

    // Same cadence as the rest of the sweep: cheap, and a change made while we
    // were not running is still caught at the next start.
    crate::config_watch::sweep(store);

    // Resolution touches the filesystem, so it runs here rather than on the
    // write path. Cheap to repeat: only newly seen directories are examined.
    match store.backfill_repositories(&mut RepositoryResolver::new()) {
        Ok(backfill) if backfill.projects > 0 || backfill.linked_rows > 0 => tracing::info!(
            directories = backfill.projects,
            repositories = backfill.repositories,
            unresolved = backfill.unresolved,
            linked = backfill.linked_rows,
            "resolved directories to repositories"
        ),
        Ok(_) => {}
        Err(error) => tracing::error!(?error, "repository backfill failed"),
    }

    if report.sessions > 0 || report.codex_rollouts > 0 {
        tracing::info!(
            sessions = report.sessions,
            codex_rollouts = report.codex_rollouts,
            responses = report.responses,
            written = report.written,
            missing = report.missing_transcripts,
            finished = report.finished,
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

    /// A session whose start was never observed, with a transcript on disk.
    ///
    /// The shape every row imported from history has: status `unknown`, so
    /// nothing about the row itself says whether it is still running.
    fn unwatched_fixture(age: Duration) -> (Store, tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("projects");
        let project = root.join("-work");
        std::fs::create_dir_all(&project).expect("create");

        let transcript = project.join("s-1.jsonl");
        std::fs::write(&transcript, TRANSCRIPT).expect("write");
        let modified = std::time::SystemTime::now() - age;
        std::fs::File::options()
            .write(true)
            .open(&transcript)
            .expect("open")
            .set_modified(modified)
            .expect("backdate");

        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[AgentEvent::observed(
                AgentId::CLAUDE_CODE,
                EvidenceSource::Transcript,
                Event::SessionStarted(SessionStarted::default()),
            )
            .with_session(ExternalSessionId::from("s-1".to_owned()))
            .with_project_path("/work".to_owned())])
            .expect("insert");
        store
            .connection_for_test()
            .execute("UPDATE sessions SET status = 'unknown'", [])
            .expect("demote");

        (store, directory, root)
    }

    #[test]
    fn an_unwatched_session_with_a_still_growing_transcript_stays_pending() {
        let (mut store, _guard, root) = unwatched_fixture(Duration::from_secs(0));
        let pending = store.sessions_awaiting_reconcile(10).expect("pending");

        let report = reconcile_session(&mut store, &pending[0], &root).expect("reconcile");
        assert_eq!(report.finished, 0, "it may still be running");
        assert_eq!(
            store
                .sessions_awaiting_reconcile(10)
                .expect("pending")
                .len(),
            1,
            "its totals must not be frozen mid-flight"
        );
    }

    #[test]
    fn an_unwatched_session_whose_transcript_went_quiet_is_retired() {
        let (mut store, _guard, root) = unwatched_fixture(TRANSCRIPT_IDLE * 2);
        let pending = store.sessions_awaiting_reconcile(10).expect("pending");

        let report = reconcile_session(&mut store, &pending[0], &root).expect("reconcile");
        assert_eq!(report.finished, 1);
        assert!(
            store
                .sessions_awaiting_reconcile(10)
                .expect("pending")
                .is_empty(),
            "imported history should not be re-read forever"
        );
    }

    /// The starvation regression: a bounded sweep ordered newest-first re-read
    /// the same head of the queue on every pass and never reached the tail.
    #[test]
    fn a_bounded_sweep_works_through_every_pending_session() {
        let mut store = Store::open_in_memory().expect("schema");
        for index in 0..5 {
            store
                .insert_events(&[AgentEvent::observed(
                    AgentId::CLAUDE_CODE,
                    EvidenceSource::Transcript,
                    Event::SessionStarted(SessionStarted::default()),
                )
                .with_session(ExternalSessionId::from(format!("s-{index}")))
                .at(agentwatch_types::Timestamp::from_micros(
                    1_000_000 * (index + 1),
                ))])
                .expect("insert");
        }
        store
            .connection_for_test()
            .execute("UPDATE sessions SET status = 'unknown'", [])
            .expect("demote");

        // Two sessions per sweep, with no transcripts anywhere to read.
        let elsewhere = PathBuf::from("/nonexistent/projects");
        let mut seen = std::collections::HashSet::new();
        for _ in 0..3 {
            let batch = store.sessions_awaiting_reconcile(2).expect("pending");
            for session in &batch {
                seen.insert(session.session_id.clone());
                reconcile_session(&mut store, session, &elsewhere).expect("reconcile");
            }
        }

        assert_eq!(seen.len(), 5, "every session should have had a turn");
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
