//! Wiring, startup, and shutdown.

use std::sync::Arc;

use agentwatch_storage::Store;
use agentwatch_types::Paths;
use anyhow::{Context as _, Result};
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::sync::Notify;
use tokio::sync::mpsc::channel;

use crate::batcher;
use crate::reconcile;
use crate::registry::AdapterRegistry;
use crate::server;

/// Events that may be queued between ingestion and storage.
///
/// At roughly 300 bytes per event this is well under a megabyte, and it is
/// several seconds of headroom for even a very busy agent.
const INGEST_QUEUE_DEPTH: usize = 4096;

/// How often the reconciler sweeps even when nothing has nudged it.
///
/// The safety net behind the session-end nudge: it catches sessions whose end
/// we never saw, which is exactly the case a nudge cannot cover.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(300);

/// Longest to wait for queued events to be written during shutdown.
///
/// Comfortably more than a full write queue needs, and short enough that
/// stopping the service stays a quick operation whatever state it was in.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How long to wait after a session ends before reading its transcript.
///
/// The agent writes the last records around the same moment the hook fires;
/// reading immediately would race the final flush and miss the closing
/// responses until the next sweep.
const RECONCILE_DEBOUNCE: Duration = Duration::from_secs(2);

/// How the daemon should run.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DaemonConfig {
    /// Where the socket and database live.
    pub paths: Paths,
    /// Whether to reconcile durable logs found in the user's agent homes.
    pub import_local_history: bool,
}

impl DaemonConfig {
    /// Builds configuration for an explicit data directory.
    #[must_use]
    pub const fn new(paths: Paths) -> Self {
        Self {
            paths,
            import_local_history: false,
        }
    }

    /// Enables or disables discovery of durable logs in agent home directories.
    ///
    /// Explicit configurations default to disabled so an embedded or test
    /// daemon does not unexpectedly ingest the host user's history.
    #[must_use]
    pub const fn with_local_history(mut self, enabled: bool) -> Self {
        self.import_local_history = enabled;
        self
    }

    /// Reads configuration from the environment.
    ///
    /// # Errors
    ///
    /// Returns an error if the data directory cannot be resolved.
    pub fn from_env() -> Result<Self> {
        let paths = Paths::from_env().context("resolving the data directory")?;
        Ok(Self::new(paths).with_local_history(true))
    }
}

/// A configured, not yet running daemon.
#[derive(Debug)]
pub struct Daemon {
    config: DaemonConfig,
}

impl Daemon {
    /// Creates a daemon.
    #[must_use]
    pub const fn new(config: DaemonConfig) -> Self {
        Self { config }
    }

    /// Runs until interrupted, then flushes and returns.
    ///
    /// # Errors
    ///
    /// Returns an error if the data directory, database, or socket cannot be
    /// prepared. Once running, individual failures are logged rather than
    /// propagated.
    pub async fn run(self) -> Result<()> {
        let paths = &self.config.paths;
        let import_local_history = self.config.import_local_history;
        paths.ensure_root().context("creating the data directory")?;

        let store = Store::open(paths.database()).context("opening the database")?;
        let listener = bind(paths).context("binding the socket")?;

        tracing::info!(
            socket = %paths.socket().display(),
            database = %paths.database().display(),
            "daemon listening"
        );

        // Catch up before accepting anything: sessions that ended while we were
        // not running are the whole reason this pass exists.
        {
            let database = paths.database();
            let caught_up = tokio::task::spawn_blocking(move || {
                let mut store = Store::open(&database)?;
                reconcile::sweep_with_local_history(&mut store, import_local_history)
            })
            .await;
            if let Ok(Err(error)) = caught_up {
                tracing::error!(?error, "startup reconcile failed");
            }
        }

        let registry = Arc::new(AdapterRegistry::with_builtin_adapters());
        let (events, queue) = channel(INGEST_QUEUE_DEPTH);
        let session_ended = Arc::new(Notify::new());

        let batcher = tokio::spawn(batcher::run(queue, store, paths.clone()));
        let server = tokio::spawn(server::serve(
            listener,
            registry,
            events,
            Arc::clone(&session_ended),
        ));
        let reconciler = tokio::spawn(reconcile_loop(
            paths.database(),
            Arc::clone(&session_ended),
            import_local_history,
        ));

        shutdown_signal().await;
        tracing::info!("shutting down; flushing queued events");

        // Aborting the server drops its connection set and with it the last
        // `Sender`, which ends the batcher once it has drained and written
        // whatever was already queued.
        reconciler.abort();
        server.abort();

        // Bounded, because this runs between the user asking us to stop and the
        // process exiting. Draining is worth waiting for; waiting forever on a
        // stage that will not finish is not, and launchd will kill us anyway.
        match tokio::time::timeout(SHUTDOWN_GRACE, batcher).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(?error, "batcher did not shut down cleanly"),
            Err(_) => tracing::warn!(
                grace = ?SHUTDOWN_GRACE,
                "batcher did not drain in time; exiting anyway"
            ),
        }

        let _ = std::fs::remove_file(paths.socket());
        tracing::info!("stopped");
        Ok(())
    }
}

/// Reads transcripts for sessions the hooks could not fully cover.
///
/// Runs on its own database connection rather than borrowing the writer's. WAL
/// allows the second connection, and it keeps a slow scan over months of
/// history off the path that ingestion depends on.
async fn reconcile_loop(
    database: std::path::PathBuf,
    session_ended: Arc<Notify>,
    import_local_history: bool,
) {
    loop {
        let nudged = tokio::select! {
            () = session_ended.notified() => true,
            () = tokio::time::sleep(RECONCILE_INTERVAL) => false,
        };

        if nudged {
            tokio::time::sleep(RECONCILE_DEBOUNCE).await;
        }

        let database = database.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut store = Store::open(&database)?;
            reconcile::sweep_with_local_history(&mut store, import_local_history)
        })
        .await;

        match outcome {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::error!(?error, "reconcile pass failed"),
            Err(error) => tracing::error!(?error, "reconcile task panicked"),
        }
    }
}

/// Longest socket path the platform accepts.
///
/// `sockaddr_un.sun_path` is 104 bytes on macOS, and the kernel's error for
/// exceeding it names a constant most people have never heard of. Checking
/// first turns that into an answer.
const MAX_SOCKET_PATH_BYTES: usize = 104;

/// Binds the listener, clearing a stale socket left by a crash.
///
/// A socket file whose peer refuses connections belongs to a dead daemon and is
/// safe to remove. One that accepts them does not: a second daemon would
/// silently steal the first one's hooks, so binding fails instead.
fn bind(paths: &Paths) -> Result<UnixListener> {
    let socket = paths.socket();

    let length = socket.as_os_str().as_encoded_bytes().len();
    anyhow::ensure!(
        length < MAX_SOCKET_PATH_BYTES,
        "socket path is {length} bytes, but the platform allows at most {}: {}\n\
         Set AGENTWATCH_DIR to a shorter directory.",
        MAX_SOCKET_PATH_BYTES - 1,
        socket.display(),
    );

    if socket.exists() {
        match std::os::unix::net::UnixStream::connect(&socket) {
            Ok(_) => anyhow::bail!(
                "another agentwatch daemon is already listening on {}",
                socket.display()
            ),
            Err(_) => {
                tracing::warn!(path = %socket.display(), "removing stale socket");
                std::fs::remove_file(&socket).context("removing the stale socket")?;
            }
        }
    }

    UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))
}

/// Resolves on the first interrupt or termination signal.
async fn shutdown_signal() {
    let interrupt = tokio::signal::ctrl_c();

    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                tracing::error!(?error, "cannot listen for SIGTERM; interrupt only");
                let _ = interrupt.await;
                return;
            }
        };

    tokio::select! {
        _ = interrupt => tracing::info!("interrupted"),
        _ = terminate.recv() => tracing::info!("terminated"),
    }
}
