//! Token analytics and the reconcile bookkeeping that feeds it.

use rusqlite::params;

use crate::store::{Store, StoreError};

/// Token counts over some slice of history.
///
/// The four categories stay apart all the way to the surface. Providers bill
/// them differently, so a single "cached" number cannot be un-merged later.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTotals {
    /// Uncached input tokens.
    pub input: i64,
    /// Input tokens written into the prompt cache.
    pub cache_creation: i64,
    /// Input tokens served from the prompt cache.
    pub cache_read: i64,
    /// Tokens generated.
    pub output: i64,
    /// Distinct model responses counted.
    pub responses: i64,
}

impl TokenTotals {
    /// Every token in every category.
    #[must_use]
    pub const fn total(&self) -> i64 {
        self.input + self.cache_creation + self.cache_read + self.output
    }
}

/// A session that has not had its transcript read yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSession {
    /// AgentWatch's session id.
    pub session_id: String,
    /// The agent's own session id, which names the transcript file.
    pub external_session_id: String,
    /// The agent that ran it.
    pub agent_id: String,
    /// Working directory, used to derive the transcript path.
    pub project_path: Option<String>,
    /// Transcript path the agent reported, when it did.
    pub transcript_path: Option<String>,
}

/// One row of a per-group token breakdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenGroup {
    /// What this row groups by — a project path, a model, a date.
    pub label: String,
    /// The counts for it.
    pub totals: TokenTotals,
}

impl Store {
    /// Sums token usage over a half-open time range.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn token_totals(&self, from_us: i64, to_us: i64) -> Result<TokenTotals, StoreError> {
        let totals = self.connection().query_row(
            "SELECT COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_creation_input_tokens), 0),
                    COALESCE(SUM(cache_read_input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COUNT(*)
               FROM token_usage
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2",
            params![from_us, to_us],
            |row| {
                Ok(TokenTotals {
                    input: row.get(0)?,
                    cache_creation: row.get(1)?,
                    cache_read: row.get(2)?,
                    output: row.get(3)?,
                    responses: row.get(4)?,
                })
            },
        )?;
        Ok(totals)
    }

    /// Breaks token usage down by project, largest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn tokens_by_project(
        &self,
        from_us: i64,
        to_us: i64,
    ) -> Result<Vec<TokenGroup>, StoreError> {
        self.token_groups(
            "SELECT COALESCE(p.path, '(unknown)'),
                    COALESCE(SUM(t.input_tokens), 0),
                    COALESCE(SUM(t.cache_creation_input_tokens), 0),
                    COALESCE(SUM(t.cache_read_input_tokens), 0),
                    COALESCE(SUM(t.output_tokens), 0),
                    COUNT(*)
               FROM token_usage t
               LEFT JOIN projects p ON p.id = t.project_id
              WHERE t.timestamp_us >= ?1 AND t.timestamp_us < ?2
              GROUP BY t.project_id
              ORDER BY SUM(t.input_tokens) + SUM(t.cache_creation_input_tokens)
                     + SUM(t.cache_read_input_tokens) + SUM(t.output_tokens) DESC",
            from_us,
            to_us,
        )
    }

    /// Breaks token usage down by model, largest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn tokens_by_model(&self, from_us: i64, to_us: i64) -> Result<Vec<TokenGroup>, StoreError> {
        self.token_groups(
            "SELECT COALESCE(model, '(unknown)'),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_creation_input_tokens), 0),
                    COALESCE(SUM(cache_read_input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COUNT(*)
               FROM token_usage
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2
              GROUP BY model
              ORDER BY SUM(input_tokens) + SUM(cache_creation_input_tokens)
                     + SUM(cache_read_input_tokens) + SUM(output_tokens) DESC",
            from_us,
            to_us,
        )
    }

    /// Breaks token usage down by day, in the given timezone offset.
    ///
    /// `offset_seconds` shifts UTC into the user's zone before the date is
    /// taken, so a day boundary lands where the user thinks it does rather than
    /// wherever UTC happens to put it.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn tokens_by_day(
        &self,
        from_us: i64,
        to_us: i64,
        offset_seconds: i64,
    ) -> Result<Vec<TokenGroup>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT date((timestamp_us / 1000000) + ?3, 'unixepoch'),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_creation_input_tokens), 0),
                    COALESCE(SUM(cache_read_input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COUNT(*)
               FROM token_usage
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2
              GROUP BY 1
              ORDER BY 1",
        )?;

        let rows = statement.query_map(params![from_us, to_us, offset_seconds], read_group)?;
        let mut groups = Vec::new();
        for row in rows {
            groups.push(row?);
        }
        Ok(groups)
    }

    /// Shared machinery for the grouped breakdowns.
    fn token_groups(
        &self,
        sql: &str,
        from_us: i64,
        to_us: i64,
    ) -> Result<Vec<TokenGroup>, StoreError> {
        let mut statement = self.connection().prepare(sql)?;
        let rows = statement.query_map(params![from_us, to_us], read_group)?;

        let mut groups = Vec::new();
        for row in rows {
            groups.push(row?);
        }
        Ok(groups)
    }

    /// Lists sessions whose transcript has not been read yet.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn sessions_awaiting_reconcile(
        &self,
        limit: u32,
    ) -> Result<Vec<PendingSession>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT s.id, s.external_session_id, s.agent_id, p.path, s.transcript_path
               FROM sessions s
               LEFT JOIN projects p ON p.id = s.project_id
              WHERE s.reconciled_at_us IS NULL
                AND s.external_session_id IS NOT NULL
              ORDER BY s.started_at_us DESC
              LIMIT ?1",
        )?;

        let rows = statement.query_map([limit], |row| {
            Ok(PendingSession {
                session_id: row.get(0)?,
                external_session_id: row.get(1)?,
                agent_id: row.get(2)?,
                project_path: row.get(3)?,
                transcript_path: row.get(4)?,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Records that a session's transcript has been read to completion.
    ///
    /// Only meaningful for ended sessions: an active session's transcript is
    /// still growing, so marking it done would freeze its totals mid-flight.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    pub fn mark_reconciled(&self, session_id: &str, at_us: i64) -> Result<(), StoreError> {
        self.connection().execute(
            "UPDATE sessions SET reconciled_at_us = ?2 WHERE id = ?1 AND status = 'ended'",
            params![session_id, at_us],
        )?;
        Ok(())
    }
}

/// Reads one grouped row.
fn read_group(row: &rusqlite::Row<'_>) -> rusqlite::Result<TokenGroup> {
    Ok(TokenGroup {
        label: row.get(0)?,
        totals: TokenTotals {
            input: row.get(1)?,
            cache_creation: row.get(2)?,
            cache_read: row.get(3)?,
            output: row.get(4)?,
            responses: row.get(5)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{AgentEvent, Event, EvidenceSource, TokenUsageEvent};
    use agentwatch_types::{AgentId, EventId, ExternalSessionId, Timestamp};

    use super::*;

    fn usage(request_id: &str, micros: i64, output: u64, project: &str) -> AgentEvent {
        AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Transcript,
            Event::TokenUsage(TokenUsageEvent {
                provider: "anthropic".into(),
                model: Some("claude-opus-5".into()),
                request_id: Some(request_id.to_owned()),
                input_tokens: 10,
                cache_creation_input_tokens: 20,
                cache_read_input_tokens: 30,
                output_tokens: output,
                is_subagent: false,
                provider_usage: serde_json::Map::new(),
            }),
        )
        .with_id(EventId::from_key(&AgentId::CLAUDE_CODE, request_id))
        .with_session(ExternalSessionId::from("s-1".to_owned()))
        .with_project_path(project.to_owned())
        .at(Timestamp::from_micros(micros))
    }

    fn seeded() -> Store {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[
                usage("m1", 1_000_000, 100, "/work/acme"),
                usage("m2", 2_000_000, 200, "/work/acme"),
                usage("m3", 3_000_000, 300, "/work/storefront"),
            ])
            .expect("insert");
        store
    }

    #[test]
    fn totals_sum_every_category_over_a_range() {
        let totals = seeded().token_totals(0, 10_000_000).expect("totals");

        assert_eq!(totals.responses, 3);
        assert_eq!(totals.input, 30);
        assert_eq!(totals.cache_creation, 60);
        assert_eq!(totals.cache_read, 90);
        assert_eq!(totals.output, 600);
        assert_eq!(totals.total(), 780);
    }

    #[test]
    fn the_range_is_half_open() {
        let store = seeded();
        let totals = store.token_totals(1_000_000, 3_000_000).expect("totals");
        assert_eq!(totals.responses, 2, "the upper bound must be excluded");
    }

    #[test]
    fn project_breakdown_is_largest_first() {
        let groups = seeded().tokens_by_project(0, 10_000_000).expect("groups");

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].label, "/work/acme");
        assert_eq!(groups[0].totals.output, 300);
        assert_eq!(groups[1].label, "/work/storefront");
    }

    #[test]
    fn project_breakdown_orders_by_total_not_by_output_alone() {
        let mut store = Store::open_in_memory().expect("schema");
        // `quiet` generates more output but `busy` moves far more tokens overall.
        store
            .insert_events(&[
                usage("m1", 1_000_000, 1, "/work/busy"),
                usage("m2", 2_000_000, 1, "/work/busy"),
                usage("m3", 3_000_000, 1, "/work/busy"),
                usage("m4", 4_000_000, 100, "/work/quiet"),
            ])
            .expect("insert");

        let groups = store.tokens_by_project(0, 10_000_000).expect("groups");
        assert_eq!(
            groups[0].label, "/work/busy",
            "ordering must use the summed total"
        );
    }

    #[test]
    fn model_breakdown_groups_by_exact_identifier() {
        let groups = seeded().tokens_by_model(0, 10_000_000).expect("groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "claude-opus-5");
        assert_eq!(groups[0].totals.responses, 3);
    }

    #[test]
    fn daily_buckets_follow_the_supplied_timezone_offset() {
        let mut store = Store::open_in_memory().expect("schema");
        // 2026-08-20T16:00:00Z — still the 20th in UTC, already the 21st at +09.
        let at = 1_755_705_600_000_000;
        store
            .insert_events(&[usage("m1", at, 5, "/work")])
            .expect("insert");

        let utc = store.tokens_by_day(0, i64::MAX, 0).expect("utc");
        let manila = store.tokens_by_day(0, i64::MAX, 8 * 3600).expect("manila");

        assert_ne!(
            utc[0].label, manila[0].label,
            "the offset must move the day boundary"
        );
    }

    #[test]
    fn a_session_awaits_reconcile_until_it_is_marked() {
        let store = seeded();
        assert_eq!(
            store
                .sessions_awaiting_reconcile(10)
                .expect("pending")
                .len(),
            1
        );
    }

    #[test]
    fn an_active_session_cannot_be_marked_reconciled() {
        let store = seeded();
        let pending = store.sessions_awaiting_reconcile(10).expect("pending");
        store
            .mark_reconciled(&pending[0].session_id, 1)
            .expect("mark");

        assert_eq!(
            store
                .sessions_awaiting_reconcile(10)
                .expect("pending")
                .len(),
            1,
            "an active session's transcript is still growing"
        );
    }
}
