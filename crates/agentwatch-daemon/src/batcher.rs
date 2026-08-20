//! Batching and the write thread.
//!
//! SQLite is fast at transactions and slow at doing one per row. Events are
//! accumulated until the batch is full or the flush interval elapses, then
//! handed to a dedicated OS thread that owns the connection.

use std::time::Duration;

use agentwatch_events::AgentEvent;
use agentwatch_storage::Store;
use agentwatch_types::RepositoryResolver;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::time::Instant;

/// Events per transaction.
pub(crate) const BATCH_SIZE: usize = 256;

/// Longest an event waits before being written.
///
/// Sets the ceiling on how stale a live view can be. 200ms is imperceptible in
/// a menu bar and still batches heavily during a busy agent session.
pub(crate) const FLUSH_INTERVAL: Duration = Duration::from_millis(200);

/// Batches queued to the writer thread before the batcher blocks.
const WRITE_QUEUE_DEPTH: usize = 16;

/// Consumes normalized events, batches them, and writes them.
///
/// Returns when `events` closes and the final partial batch has been written.
pub(crate) async fn run(mut events: Receiver<AgentEvent>, store: Store) {
    let (batches, writer) = spawn_writer(store);
    let mut buffer: Vec<AgentEvent> = Vec::with_capacity(BATCH_SIZE);
    let mut deadline: Option<Instant> = None;

    loop {
        // With no buffered events there is nothing to flush, so the timer arm
        // never becomes ready and the task parks entirely. That is what keeps
        // idle CPU at zero rather than at one wakeup per interval.
        // Copied, not borrowed: the select arm below needs `deadline` mutably
        // while this future is still alive.
        let pending_deadline = deadline;
        let timer = async move {
            match pending_deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            biased;

            received = events.recv() => match received {
                Some(event) => {
                    if buffer.is_empty() {
                        deadline = Some(Instant::now() + FLUSH_INTERVAL);
                    }
                    buffer.push(event);

                    if buffer.len() >= BATCH_SIZE {
                        flush(&mut buffer, &batches, &mut deadline).await;
                    }
                }
                None => {
                    flush(&mut buffer, &batches, &mut deadline).await;
                    break;
                }
            },

            () = timer => flush(&mut buffer, &batches, &mut deadline).await,
        }
    }

    drop(batches);
    if let Err(error) = writer.await {
        tracing::error!(?error, "writer thread did not shut down cleanly");
    }
}

/// Sends the buffered events to the writer and resets the deadline.
async fn flush(
    buffer: &mut Vec<AgentEvent>,
    batches: &Sender<Vec<AgentEvent>>,
    deadline: &mut Option<Instant>,
) {
    *deadline = None;
    if buffer.is_empty() {
        return;
    }

    let batch = std::mem::replace(buffer, Vec::with_capacity(BATCH_SIZE));
    let count = batch.len();
    if batches.send(batch).await.is_err() {
        tracing::error!(count, "writer thread is gone; dropping batch");
    }
}

/// Starts the thread that owns the database connection.
fn spawn_writer(mut store: Store) -> (Sender<Vec<AgentEvent>>, tokio::task::JoinHandle<()>) {
    let (sender, mut receiver) = channel::<Vec<AgentEvent>>(WRITE_QUEUE_DEPTH);

    // Lives with the writer thread so its cache survives between batches.
    let mut resolver = RepositoryResolver::new();

    let handle = tokio::task::spawn_blocking(move || {
        while let Some(batch) = receiver.blocking_recv() {
            match store.insert_events(&batch) {
                Ok(written) => {
                    tracing::debug!(written, queued = batch.len(), "wrote batch");

                    // Almost always a no-op: it returns before opening a
                    // transaction unless a directory has been seen for the
                    // first time.
                    if let Err(error) = store.backfill_repositories(&mut resolver) {
                        tracing::error!(?error, "repository backfill failed");
                    }
                }
                Err(error) => {
                    // A failed batch is dropped rather than retried: retrying a
                    // batch that SQLite rejected would loop forever, and losing
                    // analytics events is preferable to stalling ingestion.
                    tracing::error!(?error, count = batch.len(), "dropping unwritable batch");
                }
            }
        }
    });

    (sender, handle)
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{CommandEvent, Event, EvidenceSource};
    use agentwatch_types::{AgentId, ExternalSessionId};

    use super::*;

    fn event(command: &str) -> AgentEvent {
        AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::Command(CommandEvent {
                command: command.to_owned(),
                description: None,
            }),
        )
        .with_session(ExternalSessionId::from("s-1".to_owned()))
    }

    #[tokio::test]
    async fn writes_a_partial_batch_when_the_channel_closes() {
        let directory = tempfile::tempdir().expect("temp dir");
        let database = directory.path().join("events.db");

        let (sender, receiver) = channel(64);
        let store = Store::open(&database).expect("open");
        let task = tokio::spawn(run(receiver, store));

        sender.send(event("cargo test")).await.expect("queued");
        drop(sender);
        task.await.expect("batcher finished");

        let reader = Store::open_read_only(&database).expect("reopen");
        assert_eq!(reader.totals().expect("totals").events, 1);
    }

    #[tokio::test]
    async fn flushes_on_the_interval_without_waiting_for_a_full_batch() {
        let directory = tempfile::tempdir().expect("temp dir");
        let database = directory.path().join("events.db");

        let (sender, receiver) = channel(64);
        let store = Store::open(&database).expect("open");
        let task = tokio::spawn(run(receiver, store));

        // The sender is deliberately still open: nothing but the flush interval
        // can cause this single event to be written.
        sender.send(event("ls")).await.expect("queued");

        // Real time rather than a paused clock: with time paused the sleep
        // returns instantly while the write is still in flight on a blocking
        // thread, which makes the assertion a race rather than a test.
        let deadline = tokio::time::Instant::now() + FLUSH_INTERVAL * 25;
        let written = loop {
            let count = Store::open_read_only(&database)
                .ok()
                .and_then(|store| store.totals().ok())
                .map_or(0, |totals| totals.events);

            if count > 0 || tokio::time::Instant::now() >= deadline {
                break count;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        assert_eq!(
            written, 1,
            "the interval should have flushed a partial batch"
        );

        drop(sender);
        task.await.expect("batcher finished");
    }

    #[tokio::test]
    async fn writes_more_events_than_fit_in_one_batch() {
        let directory = tempfile::tempdir().expect("temp dir");
        let database = directory.path().join("events.db");

        let (sender, receiver) = channel(BATCH_SIZE * 4);
        let store = Store::open(&database).expect("open");
        let task = tokio::spawn(run(receiver, store));

        let total = BATCH_SIZE * 2 + 7;
        for index in 0..total {
            sender
                .send(event(&format!("command-{index}")))
                .await
                .expect("queued");
        }
        drop(sender);
        task.await.expect("batcher finished");

        let reader = Store::open_read_only(&database).expect("reopen");
        assert_eq!(reader.totals().expect("totals").events, total as i64);
    }
}
