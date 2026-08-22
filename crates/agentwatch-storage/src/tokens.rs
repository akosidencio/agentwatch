//! Token analytics and the reconcile bookkeeping that feeds it.

use std::collections::BTreeMap;

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
    /// `active`, `ended`, or `unknown`.
    ///
    /// Carried so the caller can decide whether a pass is the final one. Only
    /// an `ended` session is finished for certain; everything else needs the
    /// transcript itself to say so.
    pub status: String,
}

/// One row of a per-group token breakdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenGroup {
    /// What this row groups by — a project path, a model, a date.
    pub label: String,
    /// The counts for it.
    pub totals: TokenTotals,
}

/// Counters the provider reported that the four headline figures do not cover.
///
/// # Why these were invisible
///
/// Ingestion keeps everything a provider sends: whatever is not one of the four
/// known counters is preserved verbatim in `provider_usage`. That was the right
/// call and it worked — but nothing ever read it back, so counters worth
/// millions of tokens sat in the database with no way to see them short of
/// writing SQL by hand. Reading them needs no new collection and no re-import;
/// the rows already say all of this.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TokenDetail {
    /// Tokens spent reasoning before answering.
    ///
    /// Normalized across providers on purpose. Anthropic reports it as
    /// `output_tokens_details.thinking_tokens` and OpenAI as
    /// `reasoning_output_tokens`; they are the same quantity under two names,
    /// they are both a subset of output tokens, and unlike the headline
    /// counters they *are* comparable — so this is one of the few figures that
    /// can honestly be added across providers.
    pub reasoning: i64,
    /// Output tokens over the same rows, so the reasoning share is computable.
    pub output: i64,
    /// Cache writes on the five-minute TTL.
    ///
    /// Kept apart from the one-hour tier for the same reason cache creation is
    /// kept apart from cache read: the two are priced differently, and a total
    /// that blends them cannot be un-blended afterwards.
    pub cache_write_5m: i64,
    /// Cache writes on the one-hour TTL.
    pub cache_write_1h: i64,
    /// Searches the provider ran server-side, which are metered separately.
    pub web_search_requests: i64,
    /// Fetches the provider ran server-side.
    pub web_fetch_requests: i64,
}

impl TokenDetail {
    /// What fraction of output was spent reasoning, as a percentage.
    ///
    /// Returns `None` when there was no output to divide by.
    #[must_use]
    pub fn reasoning_share(&self) -> Option<f64> {
        (self.output > 0).then(|| self.reasoning as f64 * 100.0 / self.output as f64)
    }

    /// Every cache write, whichever tier it landed on.
    #[must_use]
    pub const fn cache_write(&self) -> i64 {
        self.cache_write_5m + self.cache_write_1h
    }

    /// Whether the provider reported any of this at all.
    ///
    /// Used to skip the section rather than print a block of zeroes for a
    /// provider that simply does not report these categories.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.reasoning == 0
            && self.cache_write_5m == 0
            && self.cache_write_1h == 0
            && self.web_search_requests == 0
            && self.web_fetch_requests == 0
    }
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

    /// Breaks token usage down by repository, largest first.
    ///
    /// Rows that never resolved to a repository fall back to their working
    /// directory rather than collapsing into one "(unknown)" bucket, so work
    /// done outside a repository stays visible and attributed.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn tokens_by_repository(
        &self,
        from_us: i64,
        to_us: i64,
    ) -> Result<Vec<TokenGroup>, StoreError> {
        self.token_groups(
            "SELECT COALESCE(r.root, p.path, '(unknown)'),
                    COALESCE(SUM(t.input_tokens), 0),
                    COALESCE(SUM(t.cache_creation_input_tokens), 0),
                    COALESCE(SUM(t.cache_read_input_tokens), 0),
                    COALESCE(SUM(t.output_tokens), 0),
                    COUNT(*)
               FROM token_usage t
               LEFT JOIN repositories r ON r.id = t.repository_id
               LEFT JOIN projects p     ON p.id = t.project_id
              WHERE t.timestamp_us >= ?1 AND t.timestamp_us < ?2
              GROUP BY COALESCE(t.repository_id, t.project_id)
              ORDER BY SUM(t.input_tokens) + SUM(t.cache_creation_input_tokens)
                     + SUM(t.cache_read_input_tokens) + SUM(t.output_tokens) DESC",
            from_us,
            to_us,
        )
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
    /// Labels are qualified with the provider that served them. A bare model
    /// list reads as one uniform ranking, and it is not one: the counters
    /// underneath `claude-opus-5` and `gpt-5.6-sol` are produced by different
    /// tokenizers and priced on different terms, so the provider is part of
    /// the model's identity rather than decoration on it.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn tokens_by_model(&self, from_us: i64, to_us: i64) -> Result<Vec<TokenGroup>, StoreError> {
        self.token_groups(
            "SELECT provider || '/' || COALESCE(model, '(unknown)'),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_creation_input_tokens), 0),
                    COALESCE(SUM(cache_read_input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COUNT(*)
               FROM token_usage
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2
              GROUP BY provider, model
              ORDER BY SUM(input_tokens) + SUM(cache_creation_input_tokens)
                     + SUM(cache_read_input_tokens) + SUM(output_tokens) DESC",
            from_us,
            to_us,
        )
    }

    /// Breaks token usage down by provider, largest first.
    ///
    /// # Why this is not just another grouping
    ///
    /// The four counters are comparable *within* a provider and not across
    /// one. Anthropic reports almost all of its input as cache reads while
    /// OpenAI reports none of it as cache writes at all, so a summed "cache
    /// write" figure silently means "Anthropic only", and a summed "input"
    /// figure adds two differently-defined quantities measured by two
    /// different tokenizers.
    ///
    /// This is the same argument that keeps cache creation apart from cache
    /// read, one level up: a merge that cannot be undone downstream has to not
    /// happen at ingestion or at the surface.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn tokens_by_provider(
        &self,
        from_us: i64,
        to_us: i64,
    ) -> Result<Vec<TokenGroup>, StoreError> {
        self.token_groups(
            "SELECT provider,
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_creation_input_tokens), 0),
                    COALESCE(SUM(cache_read_input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COUNT(*)
               FROM token_usage
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2
              GROUP BY provider
              ORDER BY SUM(input_tokens) + SUM(cache_creation_input_tokens)
                     + SUM(cache_read_input_tokens) + SUM(output_tokens) DESC",
            from_us,
            to_us,
        )
    }

    /// Reads the promoted `provider_usage` counters, per provider.
    ///
    /// Nothing here is recomputed from scratch: every value is already stored
    /// on the row, and this only reaches into the JSON to add it up. Existing
    /// history therefore answers these questions immediately, with no import.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn token_detail_by_provider(
        &self,
        from_us: i64,
        to_us: i64,
    ) -> Result<Vec<(String, TokenDetail)>, StoreError> {
        let mut statement = self.connection().prepare(
            // COALESCE across the two spellings of reasoning tokens: whichever
            // the provider used, at most one is present on a given row.
            "SELECT provider,
                    COALESCE(SUM(COALESCE(
                        json_extract(provider_usage, '$.output_tokens_details.thinking_tokens'),
                        json_extract(provider_usage, '$.reasoning_output_tokens'),
                        0)), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(COALESCE(
                        json_extract(provider_usage, '$.cache_creation.ephemeral_5m_input_tokens'),
                        0)), 0),
                    COALESCE(SUM(COALESCE(
                        json_extract(provider_usage, '$.cache_creation.ephemeral_1h_input_tokens'),
                        0)), 0),
                    COALESCE(SUM(COALESCE(
                        json_extract(provider_usage, '$.server_tool_use.web_search_requests'),
                        0)), 0),
                    COALESCE(SUM(COALESCE(
                        json_extract(provider_usage, '$.server_tool_use.web_fetch_requests'),
                        0)), 0)
               FROM token_usage
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2
              GROUP BY provider
              ORDER BY provider",
        )?;

        let rows = statement.query_map(params![from_us, to_us], |row| {
            Ok((
                row.get::<_, String>(0)?,
                TokenDetail {
                    reasoning: row.get(1)?,
                    output: row.get(2)?,
                    cache_write_5m: row.get(3)?,
                    cache_write_1h: row.get(4)?,
                    web_search_requests: row.get(5)?,
                    web_fetch_requests: row.get(6)?,
                },
            ))
        })?;

        let mut detail = Vec::new();
        for row in rows {
            detail.push(row?);
        }
        Ok(detail)
    }

    /// Breaks token usage down by coding agent, largest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn tokens_by_agent(&self, from_us: i64, to_us: i64) -> Result<Vec<TokenGroup>, StoreError> {
        self.token_groups(
            "SELECT agent_id,
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_creation_input_tokens), 0),
                    COALESCE(SUM(cache_read_input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COUNT(*)
               FROM token_usage
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2
              GROUP BY agent_id
              ORDER BY SUM(input_tokens) + SUM(cache_creation_input_tokens)
                     + SUM(cache_read_input_tokens) + SUM(output_tokens) DESC",
            from_us,
            to_us,
        )
    }

    /// Sums token usage for one agent over a half-open range.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn token_totals_for_agent(
        &self,
        agent: &str,
        from_us: i64,
        to_us: i64,
    ) -> Result<TokenTotals, StoreError> {
        let totals = self.connection().query_row(
            "SELECT COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_creation_input_tokens), 0),
                    COALESCE(SUM(cache_read_input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0), COUNT(*)
               FROM token_usage
              WHERE agent_id = ?1 AND timestamp_us >= ?2 AND timestamp_us < ?3",
            params![agent, from_us, to_us],
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

    /// Breaks token usage down by day using the caller's timezone resolver.
    ///
    /// The callback receives each row's UTC timestamp. Resolving per row rather
    /// than applying one fixed offset is what keeps historical data correct in
    /// zones whose UTC offset changes for daylight saving time.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn tokens_by_day(
        &self,
        from_us: i64,
        to_us: i64,
        mut day_for: impl FnMut(i64) -> String,
    ) -> Result<Vec<TokenGroup>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT timestamp_us, input_tokens, cache_creation_input_tokens,
                    cache_read_input_tokens, output_tokens
               FROM token_usage
              WHERE timestamp_us >= ?1 AND timestamp_us < ?2
              ORDER BY timestamp_us",
        )?;

        let rows = statement.query_map(params![from_us, to_us], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                TokenTotals {
                    input: row.get(1)?,
                    cache_creation: row.get(2)?,
                    cache_read: row.get(3)?,
                    output: row.get(4)?,
                    responses: 1,
                },
            ))
        })?;
        let mut by_day: BTreeMap<String, TokenTotals> = BTreeMap::new();
        for row in rows {
            let (timestamp, totals) = row?;
            let group = by_day.entry(day_for(timestamp)).or_default();
            group.input += totals.input;
            group.cache_creation += totals.cache_creation;
            group.cache_read += totals.cache_read;
            group.output += totals.output;
            group.responses += totals.responses;
        }
        Ok(by_day
            .into_iter()
            .map(|(label, totals)| TokenGroup { label, totals })
            .collect())
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

    /// Lists sessions whose transcript has not been read to completion.
    ///
    /// Ordered least-recently-attempted first, not newest first. A sweep is
    /// bounded, and ordering by start time meant the same newest sessions were
    /// re-read on every pass while everything behind them was never reached at
    /// all — which is the normal state after importing a history, since a
    /// session nobody watched start cannot be declared finished on sight.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn sessions_awaiting_reconcile(
        &self,
        limit: u32,
    ) -> Result<Vec<PendingSession>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT s.id, s.external_session_id, s.agent_id, p.path, s.transcript_path, s.status
               FROM sessions s
               LEFT JOIN projects p ON p.id = s.project_id
              WHERE s.reconciled_at_us IS NULL
                AND s.agent_id = 'claude-code'
                AND s.external_session_id IS NOT NULL
              ORDER BY COALESCE(s.reconcile_attempted_at_us, 0) ASC,
                       s.started_at_us DESC
              LIMIT ?1",
        )?;

        let rows = statement.query_map([limit], |row| {
            Ok(PendingSession {
                session_id: row.get(0)?,
                external_session_id: row.get(1)?,
                agent_id: row.get(2)?,
                project_path: row.get(3)?,
                transcript_path: row.get(4)?,
                status: row.get(5)?,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Records that a session's transcript will never need reading again.
    ///
    /// Final: the session drops out of the reconcile queue permanently, so the
    /// caller must be sure the transcript has stopped growing. Whether that is
    /// true is a question about the transcript, not about the row, so the
    /// decision belongs to the caller holding the file.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    pub fn mark_reconciled(&self, session_id: &str, at_us: i64) -> Result<(), StoreError> {
        self.connection().execute(
            "UPDATE sessions
                SET reconciled_at_us = ?2, reconcile_attempted_at_us = ?2
              WHERE id = ?1",
            params![session_id, at_us],
        )?;
        Ok(())
    }

    /// Records that a session was examined without being finished.
    ///
    /// What moves a session to the back of the queue, so a bounded sweep works
    /// its way through every pending session rather than circling the newest.
    ///
    /// # Errors
    ///
    /// Returns an error if the update fails.
    pub fn mark_reconcile_attempted(&self, session_id: &str, at_us: i64) -> Result<(), StoreError> {
        self.connection().execute(
            "UPDATE sessions SET reconcile_attempted_at_us = ?2 WHERE id = ?1",
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
    fn repository_breakdown_merges_directories_under_one_root() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("acme");
        let nested = root.join("packages").join("core");
        std::fs::create_dir_all(&nested).expect("create");
        std::fs::create_dir_all(root.join(".git")).expect("marker");

        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[
                usage("m1", 1_000_000, 10, &root.to_string_lossy()),
                usage("m2", 2_000_000, 20, &nested.to_string_lossy()),
            ])
            .expect("insert");
        store
            .backfill_repositories(&mut agentwatch_types::RepositoryResolver::new())
            .expect("backfill");

        let by_directory = store.tokens_by_project(0, i64::MAX).expect("directories");
        let by_repository = store
            .tokens_by_repository(0, i64::MAX)
            .expect("repositories");

        assert_eq!(by_directory.len(), 2, "two directories");
        assert_eq!(by_repository.len(), 1, "one repository");
        assert_eq!(by_repository[0].totals.output, 30);
    }

    #[test]
    fn work_outside_a_repository_falls_back_to_its_directory() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[usage("m1", 1_000, 5, "/loose/place")])
            .expect("insert");
        store
            .backfill_repositories(&mut agentwatch_types::RepositoryResolver::new())
            .expect("backfill");

        let groups = store.tokens_by_repository(0, i64::MAX).expect("groups");
        assert_eq!(groups[0].label, "/loose/place");
    }

    #[test]
    fn model_breakdown_groups_by_exact_identifier_and_names_its_provider() {
        let groups = seeded().tokens_by_model(0, 10_000_000).expect("groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].label, "anthropic/claude-opus-5",
            "a model ranking without its provider reads as one uniform list \
             when it is not"
        );
        assert_eq!(groups[0].totals.responses, 3);
    }

    #[test]
    fn the_provider_usage_remainder_is_readable_without_reimporting() {
        // Both spellings of reasoning tokens, and the TTL split, exactly as the
        // two providers write them.
        let with_usage = |agent: &AgentId, provider: &str, request_id: &str, extra: &str| {
            let provider_usage: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(extra).expect("valid usage remainder");
            AgentEvent::observed(
                agent.clone(),
                EvidenceSource::Transcript,
                Event::TokenUsage(TokenUsageEvent {
                    provider: provider.to_owned(),
                    model: Some("m".into()),
                    request_id: Some(request_id.to_owned()),
                    input_tokens: 1,
                    cache_creation_input_tokens: 300,
                    cache_read_input_tokens: 0,
                    output_tokens: 1_000,
                    is_subagent: false,
                    provider_usage,
                }),
            )
            .with_id(EventId::from_key(agent, request_id))
            .with_session(ExternalSessionId::from("s-1".to_owned()))
            .at(Timestamp::from_micros(1_000))
        };

        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[
                with_usage(
                    &AgentId::CLAUDE_CODE,
                    "anthropic",
                    "a1",
                    r#"{"output_tokens_details":{"thinking_tokens":250},
                        "cache_creation":{"ephemeral_5m_input_tokens":100,
                                          "ephemeral_1h_input_tokens":200},
                        "server_tool_use":{"web_search_requests":3,"web_fetch_requests":1}}"#,
                ),
                with_usage(
                    &AgentId::CODEX,
                    "openai",
                    "o1",
                    r#"{"reasoning_output_tokens":400}"#,
                ),
            ])
            .expect("insert");

        let detail = store
            .token_detail_by_provider(0, i64::MAX)
            .expect("detail");
        let of = |name: &str| {
            detail
                .iter()
                .find(|(provider, _)| provider == name)
                .map(|(_, found)| *found)
                .expect("provider present")
        };

        let anthropic = of("anthropic");
        assert_eq!(anthropic.reasoning, 250);
        assert_eq!(anthropic.cache_write_5m, 100);
        assert_eq!(anthropic.cache_write_1h, 200);
        assert_eq!(anthropic.cache_write(), 300);
        assert_eq!(anthropic.web_search_requests, 3);
        assert_eq!(anthropic.web_fetch_requests, 1);
        assert_eq!(anthropic.reasoning_share(), Some(25.0));

        // The other spelling of the same quantity has to land in the same field,
        // or reasoning cannot be compared or added across providers.
        let openai = of("openai");
        assert_eq!(openai.reasoning, 400);
        assert_eq!(openai.reasoning_share(), Some(40.0));
        assert!(
            openai.is_empty() || openai.cache_write() == 0,
            "this provider reports no cache tiers"
        );
    }

    #[test]
    fn a_provider_reporting_no_extras_is_reported_as_empty() {
        // `usage()` carries an empty remainder, which must not render a block
        // of zeroes at the surface.
        let detail = seeded()
            .token_detail_by_provider(0, i64::MAX)
            .expect("detail");
        assert!(detail[0].1.is_empty());
        assert_eq!(detail[0].1.reasoning_share(), Some(0.0));
    }

    #[test]
    fn providers_are_broken_out_and_never_merged() {
        // The counters mean different things per provider: this fixture mirrors
        // the real shape, where one provider reports cache writes and the other
        // structurally reports none.
        let openai = |request_id: &str, micros: i64| {
            AgentEvent::observed(
                AgentId::CODEX,
                EvidenceSource::Transcript,
                Event::TokenUsage(TokenUsageEvent {
                    provider: "openai".into(),
                    model: Some("gpt-5.6-sol".into()),
                    request_id: Some(request_id.to_owned()),
                    input_tokens: 500,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 100,
                    output_tokens: 7,
                    is_subagent: false,
                    provider_usage: serde_json::Map::new(),
                }),
            )
            .with_id(EventId::from_key(&AgentId::CODEX, request_id))
            .with_session(ExternalSessionId::from("s-2".to_owned()))
            .at(Timestamp::from_micros(micros))
        };

        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[
                usage("m1", 1_000, 5, "/work"),
                openai("o1", 2_000),
                openai("o2", 3_000),
            ])
            .expect("insert");

        let providers = store.tokens_by_provider(0, i64::MAX).expect("providers");
        assert_eq!(providers.len(), 2, "one row per provider");

        let openai_row = providers
            .iter()
            .find(|group| group.label == "openai")
            .expect("openai present");
        assert_eq!(openai_row.totals.responses, 2);
        assert_eq!(
            openai_row.totals.cache_creation, 0,
            "this provider reports no cache writes; summing it with one that \
             does would attribute the whole figure to the wrong place"
        );

        let anthropic_row = providers
            .iter()
            .find(|group| group.label == "anthropic")
            .expect("anthropic present");
        assert_eq!(anthropic_row.totals.cache_creation, 20);

        // Models stay separable too, which is the point of qualifying them.
        let models = store.tokens_by_model(0, i64::MAX).expect("models");
        let labels: Vec<&str> = models.iter().map(|g| g.label.as_str()).collect();
        assert!(labels.contains(&"openai/gpt-5.6-sol"), "{labels:?}");
        assert!(labels.contains(&"anthropic/claude-opus-5"), "{labels:?}");
    }

    #[test]
    fn daily_buckets_follow_the_supplied_timezone_offset() {
        let mut store = Store::open_in_memory().expect("schema");
        // 2026-08-20T16:00:00Z — still the 20th in UTC, already the 21st at +09.
        let at = 1_755_705_600_000_000;
        store
            .insert_events(&[usage("m1", at, 5, "/work")])
            .expect("insert");

        let day_at = |offset_seconds: i64, timestamp_us: i64| {
            ((timestamp_us / 1_000_000 + offset_seconds) / 86_400).to_string()
        };
        let utc = store
            .tokens_by_day(0, i64::MAX, |timestamp| day_at(0, timestamp))
            .expect("utc");
        let manila = store
            .tokens_by_day(0, i64::MAX, |timestamp| day_at(8 * 3600, timestamp))
            .expect("manila");

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
    fn marking_a_session_reconciled_retires_it() {
        let store = seeded();
        let pending = store.sessions_awaiting_reconcile(10).expect("pending");
        store
            .mark_reconciled(&pending[0].session_id, 1)
            .expect("mark");

        assert!(
            store
                .sessions_awaiting_reconcile(10)
                .expect("pending")
                .is_empty()
        );
    }

    #[test]
    fn recording_an_attempt_keeps_a_session_pending() {
        // The counterpart to `mark_reconciled`: whether a transcript has
        // stopped growing is a question about the file, so the reconciler
        // decides and this only moves the session down the queue.
        let store = seeded();
        let pending = store.sessions_awaiting_reconcile(10).expect("pending");
        store
            .mark_reconcile_attempted(&pending[0].session_id, 1)
            .expect("mark");

        assert_eq!(
            store
                .sessions_awaiting_reconcile(10)
                .expect("pending")
                .len(),
            1,
            "an examined session is not necessarily a finished one"
        );
    }

    #[test]
    fn the_queue_serves_the_least_recently_attempted_first() {
        let store = seeded();
        let mut store = store;
        store
            .insert_events(&[usage("m4", 4_000_000, 400, "/work/other")
                .with_session(ExternalSessionId::from("s-2".to_owned()))])
            .expect("insert");

        let first = store.sessions_awaiting_reconcile(1).expect("pending");
        store
            .mark_reconcile_attempted(&first[0].session_id, 10)
            .expect("mark");

        let second = store.sessions_awaiting_reconcile(1).expect("pending");
        assert_ne!(
            second[0].session_id, first[0].session_id,
            "a bounded sweep must not circle the same head forever"
        );
    }
}
