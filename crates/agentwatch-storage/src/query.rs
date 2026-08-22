//! The read side.
//!
//! Deliberately thin for phase 1: enough to prove events landed and to let the
//! CLI show them. Real analytics queries arrive in phase 3, on typed tables.

use rusqlite::Row;

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
