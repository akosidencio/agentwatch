//! Reading history out of transcripts, and checking that it stayed right.

use agentwatch_adapter_claude::{find_transcripts, read_token_usage, transcript_root};
use agentwatch_events::Event;
use agentwatch_storage::Store;
use anyhow::{Context as _, Result};

/// What an import pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ImportReport {
    /// Transcript files read.
    pub(crate) files: u64,
    /// Files that could not be read at all.
    pub(crate) unreadable: u64,
    /// Assistant records seen.
    pub(crate) records: u64,
    /// Distinct model responses those records represent.
    pub(crate) responses: u64,
    /// Rows actually written, after deduplication against what was stored.
    pub(crate) written: u64,
}

impl ImportReport {
    /// How much a naive per-record count would have over-reported.
    pub(crate) fn record_inflation(&self) -> f64 {
        if self.responses == 0 {
            return 1.0;
        }
        self.records as f64 / self.responses as f64
    }
}

/// Reads every transcript on disk into the database.
///
/// Safe to run repeatedly: transcript events carry deterministic ids and one
/// unique key per model response, so a second import writes nothing new.
///
/// # Errors
///
/// Returns an error if the transcript directory cannot be located or the
/// database cannot be written. An individual unreadable file is counted, not
/// raised — one corrupt transcript should not block importing the rest.
pub(crate) fn import(store: &mut Store, limit: Option<usize>) -> Result<ImportReport> {
    let root = transcript_root().context("locating the transcript directory")?;
    let mut files = find_transcripts(&root);
    if let Some(limit) = limit {
        files.truncate(limit);
    }

    let mut report = ImportReport::default();
    for file in &files {
        report.files += 1;

        let Ok((events, summary)) = read_token_usage(file) else {
            report.unreadable += 1;
            continue;
        };

        report.records += summary.usage_records;
        report.responses += summary.responses;
        report.written += store
            .insert_events(&events)
            .with_context(|| format!("storing events from {}", file.display()))?
            as u64;
    }

    Ok(report)
}

/// A disagreement between what was stored and what the transcripts say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Drift {
    /// Responses the transcripts contain.
    pub(crate) transcript_responses: i64,
    /// Responses the database holds.
    pub(crate) stored_responses: i64,
    /// Total tokens the transcripts account for.
    pub(crate) transcript_tokens: i64,
    /// Total tokens the database accounts for.
    pub(crate) stored_tokens: i64,
}

impl Drift {
    /// Whether the two agree exactly.
    pub(crate) const fn is_clean(&self) -> bool {
        self.transcript_responses == self.stored_responses
            && self.transcript_tokens == self.stored_tokens
    }
}

/// Re-reads every transcript and compares the result against storage.
///
/// This is the phase 2 exit criterion made executable. It re-derives the answer
/// from the source of truth instead of trusting the pipeline that produced it,
/// which is the only way to notice that ingestion has quietly started dropping
/// or double-counting something.
///
/// # Errors
///
/// Returns an error if the transcripts or the database cannot be read.
pub(crate) fn verify(store: &Store) -> Result<Drift> {
    let root = transcript_root().context("locating the transcript directory")?;

    let mut transcript_responses = 0_i64;
    let mut transcript_tokens = 0_i64;

    for file in find_transcripts(&root) {
        let Ok((events, _)) = read_token_usage(&file) else {
            continue;
        };
        for event in events {
            if let Event::TokenUsage(usage) = event.event {
                transcript_responses += 1;
                transcript_tokens += i64::try_from(usage.total()).unwrap_or(i64::MAX);
            }
        }
    }

    let stored = store
        .token_totals(0, i64::MAX)
        .context("reading stored totals")?;

    Ok(Drift {
        transcript_responses,
        stored_responses: stored.responses,
        transcript_tokens,
        stored_tokens: stored.total(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_drift_is_reported_as_clean() {
        let drift = Drift {
            transcript_responses: 10,
            stored_responses: 10,
            transcript_tokens: 500,
            stored_tokens: 500,
        };
        assert!(drift.is_clean());
    }

    #[test]
    fn a_token_mismatch_is_not_clean() {
        let drift = Drift {
            transcript_responses: 10,
            stored_responses: 10,
            transcript_tokens: 500,
            stored_tokens: 499,
        };
        assert!(!drift.is_clean());
    }

    #[test]
    fn inflation_is_one_when_nothing_was_read() {
        assert_eq!(ImportReport::default().record_inflation(), 1.0);
    }

    #[test]
    fn inflation_reports_records_per_response() {
        let report = ImportReport {
            records: 300,
            responses: 100,
            ..ImportReport::default()
        };
        assert_eq!(report.record_inflation(), 3.0);
    }
}
