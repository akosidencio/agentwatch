//! The read side.
//!
//! Deliberately thin for phase 1: enough to prove events landed and to let the
//! CLI show them. Real analytics queries arrive in phase 3, on typed tables.

use rusqlite::{Row, params};

use crate::store::{Store, StoreError};

/// One event as displayed by the CLI.
///
/// A plain row, constructable by callers so their rendering can be tested
/// without a database.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    /// When it happened, microseconds since the Unix epoch.
    pub timestamp_us: i64,
    /// Which agent.
    pub agent_id: String,
    /// The event kind, matching `Event::kind`.
    pub kind: String,
    /// How it was observed.
    pub evidence: String,
    /// Project path, when the event had one.
    pub project_path: Option<String>,
    /// The event body as stored JSON.
    pub payload: String,
}

impl EventRow {
    /// Reads a row from a joined `events`/`projects` query.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            timestamp_us: row.get(0)?,
            agent_id: row.get(1)?,
            kind: row.get(2)?,
            evidence: row.get(3)?,
            project_path: row.get(4)?,
            payload: row.get(5)?,
        })
    }
}

/// Headline counts for `agentwatch status`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Totals {
    /// Events stored, all time.
    pub events: i64,
    /// Sessions seen, all time.
    pub sessions: i64,
    /// Sessions not yet ended.
    pub active_sessions: i64,
    /// Distinct projects seen.
    pub projects: i64,
    /// Events the adapter could not classify.
    ///
    /// Reported because it is the one counter that says AgentWatch has stopped
    /// understanding its own input. An adapter that silently files new payloads
    /// under `unknown` keeps collecting and keeps looking healthy while the
    /// detail it was installed to capture is discarded — which is exactly what
    /// happened when hook names began arriving in a different case.
    pub unknown_events: i64,
}

impl Store {
    /// Reads the headline counts.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn totals(&self) -> Result<Totals, StoreError> {
        let totals = self.connection().query_row(
            "SELECT
                (SELECT COUNT(*) FROM events),
                (SELECT COUNT(*) FROM sessions),
                (SELECT COUNT(*) FROM sessions WHERE status = 'active'),
                (SELECT COUNT(*) FROM projects),
                (SELECT COUNT(*) FROM events WHERE kind = 'unknown')",
            [],
            |row| {
                Ok(Totals {
                    events: row.get(0)?,
                    sessions: row.get(1)?,
                    active_sessions: row.get(2)?,
                    projects: row.get(3)?,
                    unknown_events: row.get(4)?,
                })
            },
        )?;
        Ok(totals)
    }

    /// Names what the adapter could not classify, most frequent first.
    ///
    /// A bare count tells you something is wrong; the labels tell you what to
    /// fix, and they are the only surviving evidence of the payload — the rest
    /// of it was dropped at normalization. Rows whose payload carries no label
    /// are grouped under `(no label)` rather than silently vanishing from a
    /// report whose entire job is to show what went unrecognised.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn unknown_event_labels(&self, limit: usize) -> Result<Vec<(String, i64)>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT COALESCE(json_extract(payload, '$.label'), '(no label)'), COUNT(*)
               FROM events
              WHERE kind = 'unknown'
              GROUP BY 1
              ORDER BY 2 DESC, 1 ASC
              LIMIT ?1",
        )?;
        let rows = statement
            .query_map([limit], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Reads the most recent events, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn recent_events(&self, limit: u32) -> Result<Vec<EventRow>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT e.timestamp_us, e.agent_id, e.kind, e.evidence, p.path, e.payload
               FROM events e
               LEFT JOIN projects p ON p.id = e.project_id
              ORDER BY e.timestamp_us DESC, e.id DESC
              LIMIT ?1",
        )?;

        let rows = statement.query_map([limit], EventRow::from_row)?;
        let mut events = Vec::with_capacity(limit as usize);
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }
}

/// The result of an ad-hoc query.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryResult {
    /// Column names, in order.
    pub columns: Vec<String>,
    /// Rows, each as one rendered value per column. `None` is SQL NULL.
    pub rows: Vec<Vec<Option<String>>>,
}

impl Store {
    /// Runs a caller-supplied read-only query.
    ///
    /// # Why an escape hatch exists at all
    ///
    /// Every purpose-built command here answers a question someone already
    /// thought of. The database holds far more than those commands expose —
    /// the whole point of keeping a provider's unrecognised counters, or a
    /// record type this version does not model, is that the answer is present
    /// before anyone writes the report that needs it. Without a way to ask,
    /// that data is only theoretically available.
    ///
    /// # What makes it safe
    ///
    /// The connection this runs on is opened with `query_only` set, so a write
    /// is refused by SQLite itself rather than by inspecting the statement.
    /// Parsing SQL to decide whether it looks dangerous is a game that is lost
    /// by default; letting the engine enforce it is not.
    ///
    /// # Errors
    ///
    /// Returns an error if the statement is invalid or attempts a write.
    pub fn query(&self, sql: &str) -> Result<QueryResult, StoreError> {
        let mut statement = self.connection().prepare(sql)?;
        let columns: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        let width = columns.len();

        let mut rows = Vec::new();
        let mut cursor = statement.query([])?;
        while let Some(row) = cursor.next()? {
            let mut values = Vec::with_capacity(width);
            for index in 0..width {
                values.push(render_value(row.get_ref(index)?));
            }
            rows.push(values);
        }

        Ok(QueryResult { columns, rows })
    }
}

/// Renders one SQLite value for display.
///
/// A blob is described rather than printed: this output goes to a terminal, and
/// nothing in this database stores a blob worth reading as text.
fn render_value(value: rusqlite::types::ValueRef<'_>) -> Option<String> {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(number) => Some(number.to_string()),
        ValueRef::Real(number) => Some(number.to_string()),
        ValueRef::Text(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => Some(format!("<{} bytes>", bytes.len())),
    }
}

/// Signals that collection itself is degrading.
///
/// Distinct from the headline counts, which measure work observed. These
/// measure work *missed*, or about to be: a collector that has stopped hearing
/// anything looks identical to an idle machine unless something says otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Health {
    /// When the last event was stored, not when it happened.
    ///
    /// Storage time is the staleness signal: an event timestamped an hour ago
    /// and written a second ago means collection is working.
    pub last_write_us: Option<i64>,
    /// Sessions whose transcript has never been read to completion.
    pub unreconciled_sessions: i64,
}

impl Store {
    /// Reads the signals that say whether collection is still working.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn health(&self) -> Result<Health, StoreError> {
        let health = self.connection().query_row(
            "SELECT (SELECT MAX(created_at_us) FROM events),
                    (SELECT COUNT(*) FROM sessions WHERE reconciled_at_us IS NULL)",
            [],
            |row| {
                Ok(Health {
                    last_write_us: row.get(0)?,
                    unreconciled_sessions: row.get(1)?,
                })
            },
        )?;
        Ok(health)
    }
}

/// How much one file was rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChurn {
    /// Absolute path, as the agent reported it.
    pub path: String,
    /// Times it was written.
    pub writes: i64,
    /// Times it was read.
    ///
    /// Only ever non-zero for an agent with a distinct read tool. Codex has
    /// none — every read goes through the shell — so its files show zero reads
    /// here rather than no reads having happened.
    pub reads: i64,
    /// How many distinct sessions touched it.
    pub sessions: i64,
}

impl Store {
    /// Lists the files rewritten most often, busiest first.
    ///
    /// # What a high count means
    ///
    /// Not that the file is important — that the agent kept coming back to it.
    /// A file written thirty times in one session is the signature of work that
    /// went in circles, and it is invisible in any per-session summary because
    /// each individual write looks ordinary.
    ///
    /// Reads are reported beside writes because the ratio distinguishes two
    /// very different shapes: a file re-read before each edit is being worked
    /// carefully, one written repeatedly without being read is being guessed at.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn file_churn(
        &self,
        from_us: i64,
        to_us: i64,
        limit: usize,
    ) -> Result<Vec<FileChurn>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT path,
                    SUM(operation = 'write') AS writes,
                    SUM(operation = 'read')  AS reads,
                    COUNT(DISTINCT session_id) AS sessions
               FROM file_events
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2
              GROUP BY path
             HAVING writes > 0
              ORDER BY writes DESC, path ASC
              LIMIT ?3",
        )?;

        let rows = statement.query_map(params![from_us, to_us, limit], |row| {
            Ok(FileChurn {
                path: row.get(0)?,
                writes: row.get(1)?,
                reads: row.get(2)?,
                sessions: row.get(3)?,
            })
        })?;

        let mut churn = Vec::new();
        for row in rows {
            churn.push(row?);
        }
        Ok(churn)
    }
}

/// How one tool has behaved over a range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolReliability {
    /// Which agent's tool this is.
    ///
    /// Part of the row's identity, not decoration. Agents name their tools
    /// differently for the same job — Claude's `Bash` and Codex's `exec` both
    /// run a shell — so a bare tool ranking silently compares two products
    /// while looking like it compares two tools.
    pub agent_id: String,
    /// The tool's name, as the agent reported it.
    pub tool: String,
    /// Calls that were matched to a result.
    pub calls: i64,
    /// How many of them the agent reported as failed.
    pub failures: i64,
    /// Median duration, over the calls whose duration is known.
    pub p50_ms: Option<i64>,
    /// 95th percentile duration.
    ///
    /// The number worth looking at: a tool that is usually instant and
    /// occasionally minutes has a median that says nothing about the wait.
    pub p95_ms: Option<i64>,
    /// The slowest single call.
    ///
    /// Wall-clock between the call record and the result record, which is not
    /// the same as time spent running. A tool awaiting the user's approval, or
    /// a session left open overnight and resumed, both land here as an
    /// enormous duration for a command that took a second. Measured on the
    /// development corpus this affects roughly one call in two hundred, so the
    /// percentiles are unaffected and this figure is not to be quoted alone.
    pub max_ms: Option<i64>,
}

impl ToolReliability {
    /// The share of calls that failed, as a percentage.
    #[must_use]
    pub fn failure_rate(&self) -> f64 {
        if self.calls == 0 {
            return 0.0;
        }
        self.failures as f64 * 100.0 / self.calls as f64
    }
}

impl Store {
    /// Reports how each tool has behaved, busiest first.
    ///
    /// Percentiles are computed here rather than in SQL: the durations have to
    /// be sorted per tool either way, and doing it in Rust keeps the query one
    /// plain scan instead of a window function per statistic.
    ///
    /// Calls with no recorded duration still count towards `calls` and
    /// `failures` — whether a call failed is known even when how long it took
    /// is not — but are left out of the percentiles rather than counted as
    /// zero.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn tool_reliability(
        &self,
        from_us: i64,
        to_us: i64,
    ) -> Result<Vec<ToolReliability>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT agent_id, tool, duration_ms, failed
               FROM tool_outcomes
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2",
        )?;

        let rows = statement.query_map([from_us, to_us], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;

        let mut collected: std::collections::HashMap<(String, String), (i64, i64, Vec<i64>)> =
            std::collections::HashMap::new();
        for row in rows {
            let (agent_id, tool, duration, failed) = row?;
            let entry = collected
                .entry((agent_id, tool))
                .or_insert((0, 0, Vec::new()));
            entry.0 += 1;
            entry.1 += i64::from(failed != 0);
            if let Some(duration) = duration {
                entry.2.push(duration);
            }
        }

        let mut report: Vec<ToolReliability> = collected
            .into_iter()
            .map(|((agent_id, tool), (calls, failures, mut durations))| {
                durations.sort_unstable();
                ToolReliability {
                    agent_id,
                    tool,
                    calls,
                    failures,
                    p50_ms: percentile(&durations, 50),
                    p95_ms: percentile(&durations, 95),
                    max_ms: durations.last().copied(),
                }
            })
            .collect();

        report.sort_by(|a, b| {
            b.calls
                .cmp(&a.calls)
                .then_with(|| a.agent_id.cmp(&b.agent_id))
                .then_with(|| a.tool.cmp(&b.tool))
        });
        Ok(report)
    }
}

/// Nearest-rank percentile over a sorted slice.
///
/// Nearest-rank rather than interpolated: every value it returns is a duration
/// that actually happened, which is the right property for a figure someone
/// will quote back as "the 95th percentile call took this long".
fn percentile(sorted: &[i64], percent: usize) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percent * sorted.len()).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{AgentEvent, CommandEvent, Event, EvidenceSource};
    use agentwatch_types::{AgentId, ExternalSessionId, Timestamp};

    use super::*;

    fn event_at(micros: i64, command: &str) -> AgentEvent {
        AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::Command(CommandEvent {
                command: command.to_owned(),
                description: None,
            }),
        )
        .with_session(ExternalSessionId::from("s-1".to_owned()))
        .with_project_path("/Users/dev/projects/acme".to_owned())
        .at(Timestamp::from_micros(micros))
    }

    #[test]
    fn totals_count_events_sessions_and_projects() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[event_at(1_000, "ls"), event_at(2_000, "pwd")])
            .expect("insert");

        let totals = store.totals().expect("totals");
        assert_eq!(totals.events, 2);
        assert_eq!(totals.sessions, 1);
        assert_eq!(totals.projects, 1);
        assert_eq!(
            totals.active_sessions, 0,
            "these events never reported a session starting, so nothing is known to be running"
        );
    }

    #[test]
    fn an_ad_hoc_query_returns_named_columns_and_rows() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[event_at(1_000, "ls"), event_at(2_000, "pwd")])
            .expect("insert");

        let result = store
            .query("SELECT kind, COUNT(*) AS n FROM events GROUP BY kind")
            .expect("query");

        assert_eq!(result.columns, vec!["kind", "n"]);
        assert_eq!(
            result.rows,
            vec![vec![Some("command".to_owned()), Some("2".to_owned())]]
        );
    }

    #[test]
    fn a_null_is_distinguishable_from_an_empty_string() {
        let store = Store::open_in_memory().expect("schema");
        let result = store.query("SELECT NULL AS a, '' AS b").expect("query");
        assert_eq!(result.rows, vec![vec![None, Some(String::new())]]);
    }

    #[test]
    fn a_write_is_refused_by_the_engine_on_a_read_only_handle() {
        // The guarantee is SQLite's `query_only`, not a check on the statement
        // text. This asserts the mechanism rather than a blocklist, because a
        // blocklist is the version of this that gets defeated.
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("aw.db");
        {
            let mut store = Store::open(&path).expect("create");
            store.insert_events(&[event_at(1_000, "ls")]).expect("seed");
        }

        let reader = Store::open_read_only(&path).expect("open");
        assert!(reader.query("SELECT COUNT(*) FROM events").is_ok());
        let refused = reader.query("DELETE FROM events");
        assert!(refused.is_err(), "a write must not succeed on this handle");
    }

    #[test]
    fn unclassified_events_are_counted_and_named() {
        use agentwatch_events::UnknownEvent;

        let unknown = |label: &str, micros: i64| {
            AgentEvent::observed(
                AgentId::CLAUDE_CODE,
                EvidenceSource::Hook,
                Event::Unknown(UnknownEvent {
                    label: label.to_owned(),
                }),
            )
            .at(Timestamp::from_micros(micros))
        };

        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[
                event_at(1_000, "ls"),
                unknown("postToolUse", 2_000),
                unknown("postToolUse", 3_000),
                unknown("beforeSubmitPrompt", 4_000),
            ])
            .expect("insert");

        let totals = store.totals().expect("totals");
        assert_eq!(totals.events, 4);
        assert_eq!(
            totals.unknown_events, 3,
            "only unclassified events count, not every event"
        );

        let labels = store.unknown_event_labels(10).expect("labels");
        assert_eq!(
            labels,
            vec![
                ("postToolUse".to_owned(), 2),
                ("beforeSubmitPrompt".to_owned(), 1),
            ],
            "most frequent first, so the biggest loss is named first"
        );
    }

    #[test]
    fn a_session_counts_as_active_only_once_its_start_is_observed() {
        use agentwatch_events::SessionStarted;

        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[AgentEvent::observed(
                AgentId::CLAUDE_CODE,
                EvidenceSource::Hook,
                Event::SessionStarted(SessionStarted::default()),
            )
            .with_session(ExternalSessionId::from("s-1".to_owned()))])
            .expect("insert");

        assert_eq!(store.totals().expect("totals").active_sessions, 1);
    }

    #[test]
    fn recent_events_are_newest_first_and_carry_the_project_path() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[event_at(1_000, "first"), event_at(2_000, "second")])
            .expect("insert");

        let rows = store.recent_events(10).expect("query");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].timestamp_us, 2_000);
        assert_eq!(
            rows[0].project_path.as_deref(),
            Some("/Users/dev/projects/acme")
        );
        assert!(rows[0].payload.contains("second"));
    }

    #[test]
    fn recent_events_respects_the_limit() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[
                event_at(1_000, "a"),
                event_at(2_000, "b"),
                event_at(3_000, "c"),
            ])
            .expect("insert");

        assert_eq!(store.recent_events(2).expect("query").len(), 2);
    }
}
