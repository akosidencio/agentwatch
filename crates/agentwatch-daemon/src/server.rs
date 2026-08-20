//! The Unix socket listener.

use std::sync::Arc;

use agentwatch_events::{AgentEvent, Event, HookEnvelope, MAX_FRAME_BYTES, decode_frame_len};
use tokio::io::AsyncReadExt as _;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::sync::mpsc::Sender;

use crate::registry::AdapterRegistry;

/// Reads framed envelopes from one connection until it closes.
///
/// A connection carrying a bad frame is dropped rather than reset, on the
/// assumption that a confused hook should not be able to take down ingestion.
pub(crate) async fn serve(
    listener: UnixListener,
    registry: Arc<AdapterRegistry>,
    events: Sender<AgentEvent>,
    session_ended: Arc<Notify>,
) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _address)) => stream,
            Err(error) => {
                tracing::error!(?error, "accept failed");
                continue;
            }
        };

        let registry = Arc::clone(&registry);
        let events = events.clone();
        let session_ended = Arc::clone(&session_ended);
        tokio::spawn(async move {
            if let Err(error) = handle(stream, &registry, &events, &session_ended).await {
                tracing::debug!(?error, "connection ended");
            }
        });
    }
}

/// Reads every frame on a connection.
async fn handle(
    mut stream: UnixStream,
    registry: &AdapterRegistry,
    events: &Sender<AgentEvent>,
    session_ended: &Notify,
) -> std::io::Result<()> {
    let mut body = Vec::with_capacity(4096);

    loop {
        let mut header = [0_u8; 4];
        match stream.read_exact(&mut header).await {
            Ok(_) => {}
            // A clean EOF is the normal end of a hook connection.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        }

        let length = decode_frame_len(header)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        debug_assert!(length <= MAX_FRAME_BYTES);

        body.clear();
        body.resize(length, 0);
        stream.read_exact(&mut body).await?;

        ingest(&body, registry, events, session_ended).await;
    }
}

/// Parses, normalizes, and queues one frame.
///
/// Failures here are logged and dropped: one malformed frame must not cost the
/// connection or the events that follow it.
async fn ingest(
    body: &[u8],
    registry: &AdapterRegistry,
    events: &Sender<AgentEvent>,
    session_ended: &Notify,
) {
    let envelope: HookEnvelope = match serde_json::from_slice(body) {
        Ok(envelope) => envelope,
        Err(error) => {
            tracing::warn!(
                ?error,
                bytes = body.len(),
                "discarding unparseable envelope"
            );
            return;
        }
    };

    let source = envelope.source.clone();
    let event = match registry.normalize(&envelope) {
        Ok(event) => event,
        Err(error) => {
            tracing::warn!(?error, %source, "discarding envelope the adapter rejected");
            return;
        }
    };

    tracing::debug!(kind = event.kind(), %source, "ingested event");

    // A finished session has a finished transcript. Wake the reconciler rather
    // than making it wait for the next scheduled sweep.
    if matches!(event.event, Event::SessionEnded(_)) {
        session_ended.notify_one();
    }

    // Blocks once the pipeline is saturated. Safe here: the hook already exited
    // after writing, so nothing the agent does is waiting on this send.
    if events.send(event).await.is_err() {
        tracing::error!("pipeline closed; dropping event");
    }
}
