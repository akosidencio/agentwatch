//! Session listings, activity timelines, and what was actually observed.

use rusqlite::{Row, params};

use crate::store::{Store, StoreError};

/// One session, with the counts that describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// AgentWatch's session id.
    pub id: String,
    /// The agent that ran it.
    pub agent_id: String,
    /// Repository worked on, falling back to the working directory.
    pub project: Option<String>,
    /// Git branch, when known.
    pub git_branch: Option<String>,
    /// Surface the agent ran in, when known — `claude-vscode`, and so on.
    ///
    /// `None` means it was never observed, not that it was a default: only
    /// transcripts carry the field, so a session seen purely through hooks has
    /// no surface until it is reconciled.
    pub surface: Option<String>,
    /// When it started.
    pub started_at_us: Option<i64>,
    /// How long it ran, if it has ended.
    pub duration_ms: Option<i64>,
    /// `active` or `ended`.
    pub status: String,
    /// Tokens across every category.
    pub tokens: i64,
    /// Model responses.
    pub responses: i64,
    /// Shell commands run.
    pub commands: i64,
    /// File reads and writes reported by tools.
    pub files: i64,
    /// MCP tool calls.
    pub mcp_calls: i64,
    /// File events and command references above `normal`.
    pub sensitive: i64,
}

/// What a session's data can and cannot answer.
///
/// The point of the product is that a missing collector reads as missing, not
/// as zero. A session with no MCP calls and a session whose MCP calls were
/// never observed look identical in a count; they must not look identical here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    /// Token usage was reconciled from the transcript.
    pub tokens: bool,
    /// The session's start was observed.
    pub session_bounds: bool,
    /// At least one tool call was seen.
    pub tools: bool,
    /// At least one command was seen.
    pub commands: bool,
    /// At least one file event was seen.
    pub files: bool,
    /// At least one MCP call was seen.
    pub mcp: bool,
    /// Never collected in this version.
    pub network: bool,
    /// Never collected in this version.
    pub processes: bool,
    /// Prompt content is disabled by design.
    pub prompt_content: bool,
}

/// Which events to include in an activity listing.
#[derive(Debug, Clone, Default)]
pub struct ActivityFilter {
    /// Restrict to one agent.
    pub agent: Option<String>,
    /// Restrict to one session.
    pub session: Option<String>,
    /// Restrict to event kinds.
    pub kinds: Vec<String>,
    /// Restrict to a repository or project path, matched as a prefix.
    pub project_prefix: Option<String>,
}

impl Store {
    /// Lists sessions in a range, most recent first.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn sessions(
        &self,
        from_us: i64,
        to_us: i64,
        limit: u32,
    ) -> Result<Vec<SessionRow>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT s.id,
                    s.agent_id,
                    COALESCE(r.root, p.path),
                    s.git_branch,
                    s.surface,
                    s.started_at_us,
                    s.duration_ms,
                    s.status,
                    COALESCE((SELECT SUM(input_tokens + cache_creation_input_tokens
                                       + cache_read_input_tokens + output_tokens)
                                FROM token_usage WHERE session_id = s.id), 0),
                    (SELECT COUNT(*) FROM token_usage    WHERE session_id = s.id),
                    (SELECT COUNT(*) FROM command_events WHERE session_id = s.id),
                    (SELECT COUNT(*) FROM file_events    WHERE session_id = s.id),
                    (SELECT COUNT(*) FROM mcp_events     WHERE session_id = s.id),
                    (SELECT COUNT(*) FROM file_events
                       WHERE session_id = s.id AND sensitivity != 'normal')
                  + (SELECT COUNT(*) FROM command_path_references
                       WHERE session_id = s.id AND sensitivity != 'normal')
               FROM sessions s
               LEFT JOIN repositories r ON r.id = s.repository_id
               LEFT JOIN projects p     ON p.id = s.project_id
              WHERE COALESCE(s.started_at_us, 0) >= ?1
                AND COALESCE(s.started_at_us, 0) < ?2
              ORDER BY s.started_at_us DESC
              LIMIT ?3",
        )?;

        let rows = statement.query_map(params![from_us, to_us, limit], |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                project: row.get(2)?,
                git_branch: row.get(3)?,
                surface: row.get(4)?,
                started_at_us: row.get(5)?,
                duration_ms: row.get(6)?,
                status: row.get(7)?,
                tokens: row.get(8)?,
                responses: row.get(9)?,
                commands: row.get(10)?,
                files: row.get(11)?,
                mcp_calls: row.get(12)?,
                sensitive: row.get(13)?,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Reports what a session's data can answer.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn coverage(&self, session_id: &str) -> Result<Coverage, StoreError> {
        let coverage = self.connection().query_row(
            "SELECT (SELECT COUNT(*) FROM token_usage WHERE session_id = ?1) > 0,
                    (SELECT COUNT(*) FROM events
                       WHERE session_id = ?1 AND kind = 'session.started') > 0,
                    (SELECT COUNT(*) FROM events
                       WHERE session_id = ?1 AND kind IN ('tool.call','file.read','file.write','command','mcp.call')) > 0,
                    (SELECT COUNT(*) FROM command_events WHERE session_id = ?1) > 0,
                    (SELECT COUNT(*) FROM file_events    WHERE session_id = ?1) > 0,
                    (SELECT COUNT(*) FROM mcp_events     WHERE session_id = ?1) > 0",
            params![session_id],
            |row| {
                Ok(Coverage {
                    tokens: row.get(0)?,
                    session_bounds: row.get(1)?,
                    tools: row.get(2)?,
                    commands: row.get(3)?,
                    files: row.get(4)?,
                    mcp: row.get(5)?,
                    // Not collected in this version, and said so rather than
                    // reported as an absence of activity.
                    network: false,
                    processes: false,
                    prompt_content: false,
                })
            },
        )?;
        Ok(coverage)
    }

    /// Lists events in a range, oldest first, subject to filters.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn activity(
        &self,
        from_us: i64,
        to_us: i64,
        filter: &ActivityFilter,
        limit: u32,
    ) -> Result<Vec<crate::EventRow>, StoreError> {
        let mut sql = String::from(
            "SELECT e.timestamp_us, e.agent_id, e.kind, e.evidence,
                    COALESCE(r.root, p.path), e.payload
               FROM events e
               LEFT JOIN repositories r ON r.id = e.repository_id
               LEFT JOIN projects p     ON p.id = e.project_id
              WHERE e.timestamp_us >= ?1 AND e.timestamp_us < ?2",
        );

        let mut bindings: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(from_us), Box::new(to_us)];

        if let Some(agent) = &filter.agent {
            bindings.push(Box::new(agent.clone()));
            sql.push_str(&format!(" AND e.agent_id = ?{}", bindings.len()));
        }
        if let Some(session) = &filter.session {
            bindings.push(Box::new(session.clone()));
            sql.push_str(&format!(" AND e.session_id = ?{}", bindings.len()));
        }
        if let Some(prefix) = &filter.project_prefix {
            bindings.push(Box::new(format!("{prefix}%")));
            let index = bindings.len();
            sql.push_str(&format!(
                " AND (COALESCE(r.root, p.path) LIKE ?{index} OR p.path LIKE ?{index})"
            ));
        }
        if !filter.kinds.is_empty() {
            let placeholders: Vec<String> = filter
                .kinds
                .iter()
                .map(|kind| {
                    bindings.push(Box::new(kind.clone()));
                    format!("?{}", bindings.len())
                })
                .collect();
            sql.push_str(&format!(" AND e.kind IN ({})", placeholders.join(", ")));
        }

        bindings.push(Box::new(limit));
        sql.push_str(&format!(
            " ORDER BY e.timestamp_us DESC, e.id DESC LIMIT ?{}",
            bindings.len()
        ));

        let mut statement = self.connection().prepare(&sql)?;
        let parameters: Vec<&dyn rusqlite::ToSql> =
            bindings.iter().map(std::convert::AsRef::as_ref).collect();

        let rows = statement.query_map(parameters.as_slice(), read_event_row)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        events.reverse();
        Ok(events)
    }
}

/// Reads a joined event row.
fn read_event_row(row: &Row<'_>) -> rusqlite::Result<crate::EventRow> {
    Ok(crate::EventRow {
        timestamp_us: row.get(0)?,
        agent_id: row.get(1)?,
        kind: row.get(2)?,
        evidence: row.get(3)?,
        project_path: row.get(4)?,
        payload: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{
        AgentEvent, CommandEvent, Event, EvidenceSource, FileEvent, SessionEnded, SessionStarted,
    };
    use agentwatch_types::{AgentId, ExternalSessionId, Timestamp};

    use super::*;

    fn seeded() -> Store {
        let mut store = Store::open_in_memory().expect("schema");
        let external = ExternalSessionId::from("s-1".to_owned());

        let make = |event: Event, micros: i64| {
            AgentEvent::observed(AgentId::CLAUDE_CODE, EvidenceSource::Hook, event)
                .with_session(external.clone())
                .with_project_path("/work/acme".to_owned())
                .at(Timestamp::from_micros(micros))
        };

        store
            .insert_events(&[
                make(Event::SessionStarted(SessionStarted::default()), 1_000_000),
                make(
                    Event::Command(CommandEvent {
                        command: "cat .env".into(),
                        description: None,
                    }),
                    2_000_000,
                ),
                make(
                    Event::FileRead(FileEvent {
                        path: "/work/acme/src/main.rs".into(),
                        tool: "Read".into(),
                    }),
                    3_000_000,
                ),
                make(Event::SessionEnded(SessionEnded::default()), 4_000_000),
            ])
            .expect("insert");
        store
    }

    #[test]
    fn a_session_carries_its_counts() {
        let sessions = seeded().sessions(0, i64::MAX, 10).expect("sessions");

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].commands, 1);
        assert_eq!(sessions[0].files, 1);
        assert_eq!(sessions[0].status, "ended");
        assert_eq!(sessions[0].duration_ms, Some(3_000));
    }

    #[test]
    fn a_session_counts_a_sensitive_reference_from_a_command() {
        let sessions = seeded().sessions(0, i64::MAX, 10).expect("sessions");
        assert_eq!(sessions[0].sensitive, 1, "`cat .env` should register");
    }

    #[test]
    fn coverage_distinguishes_absent_from_never_collected() {
        let store = seeded();
        let sessions = store.sessions(0, i64::MAX, 10).expect("sessions");
        let coverage = store.coverage(&sessions[0].id).expect("coverage");

        assert!(coverage.commands, "commands were observed");
        assert!(coverage.files, "files were observed");
        assert!(!coverage.mcp, "no MCP calls were seen");
        assert!(!coverage.tokens, "this session was never reconciled");
        assert!(
            !coverage.network,
            "network is never collected in this version"
        );
    }

    #[test]
    fn activity_is_oldest_first() {
        let events = seeded()
            .activity(0, i64::MAX, &ActivityFilter::default(), 100)
            .expect("activity");

        assert_eq!(events.len(), 4);
        assert_eq!(events[0].kind, "session.started");
        assert_eq!(events[3].kind, "session.ended");
    }

    #[test]
    fn activity_filters_by_kind() {
        let filter = ActivityFilter {
            kinds: vec!["command".to_owned()],
            ..Default::default()
        };
        let events = seeded()
            .activity(0, i64::MAX, &filter, 100)
            .expect("activity");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "command");
    }

    #[test]
    fn activity_filters_by_agent() {
        let filter = ActivityFilter {
            agent: Some("nonesuch".to_owned()),
            ..Default::default()
        };
        assert!(
            seeded()
                .activity(0, i64::MAX, &filter, 100)
                .expect("activity")
                .is_empty()
        );
    }

    #[test]
    fn activity_filters_by_project_prefix() {
        let filter = ActivityFilter {
            project_prefix: Some("/work".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            seeded()
                .activity(0, i64::MAX, &filter, 100)
                .expect("activity")
                .len(),
            4
        );

        let filter = ActivityFilter {
            project_prefix: Some("/other".to_owned()),
            ..Default::default()
        };
        assert!(
            seeded()
                .activity(0, i64::MAX, &filter, 100)
                .expect("activity")
                .is_empty()
        );
    }

    #[test]
    fn activity_returns_the_most_recent_events_when_limited() {
        let filter = ActivityFilter::default();
        let events = seeded()
            .activity(0, i64::MAX, &filter, 2)
            .expect("activity");

        assert_eq!(events.len(), 2);
        assert_eq!(
            events[1].kind, "session.ended",
            "a limit should keep the newest, still shown oldest first"
        );
    }
}
