//! Turning a user's idea of "today" into a UTC range.
//!
//! Events are stored in UTC. Calendar questions are asked in the user's zone.
//! Converting the *boundaries* rather than the *rows* is what keeps a day's
//! total correct without a timezone conversion per row.

use anyhow::{Context as _, Result};
use time::{Date, Duration, OffsetDateTime, UtcOffset};

/// The date format accepted on the command line.
const DATE_FORMAT: &[time::format_description::FormatItem<'_>] =
    time::macros::format_description!("[year]-[month]-[day]");

/// A half-open range of microseconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Range {
    /// Inclusive lower bound.
    pub(crate) from_us: i64,
    /// Exclusive upper bound.
    pub(crate) to_us: i64,
    /// The zone the boundaries were computed in.
    pub(crate) offset: UtcOffset,
    /// How to describe this range to the user.
    pub(crate) label: String,
}

impl Range {
    /// The zone offset in whole seconds, for SQL date bucketing.
    pub(crate) const fn offset_seconds(&self) -> i64 {
        self.offset.whole_seconds() as i64
    }
}

/// Resolves the local UTC offset, falling back to UTC.
///
/// `time` refuses to read the local zone from a multi-threaded process because
/// the underlying C call is not thread safe. The CLI is single threaded, so
/// this normally succeeds; when it does not, UTC is the honest answer and the
/// caller says so rather than silently shifting the user's days.
pub(crate) fn local_offset() -> (UtcOffset, bool) {
    UtcOffset::current_local_offset().map_or((UtcOffset::UTC, false), |offset| (offset, true))
}

/// Builds the range for the last `days` calendar days, ending today.
pub(crate) fn last_days(days: u32, offset: UtcOffset) -> Range {
    let today = OffsetDateTime::now_utc().to_offset(offset).date();
    let start = today - Duration::days(i64::from(days.saturating_sub(1)));
    let label = if days == 1 {
        "today".to_owned()
    } else {
        format!("last {days} days")
    };
    between(start, today, offset, label)
}

/// Parses an explicit `--from` / `--to` pair of local dates.
///
/// # Errors
///
/// Returns an error if either date is unparseable or the range is inverted.
pub(crate) fn explicit(from: &str, to: &str, offset: UtcOffset) -> Result<Range> {
    let start = Date::parse(from, DATE_FORMAT)
        .with_context(|| format!("`{from}` is not a date; expected YYYY-MM-DD"))?;
    let end = Date::parse(to, DATE_FORMAT)
        .with_context(|| format!("`{to}` is not a date; expected YYYY-MM-DD"))?;

    anyhow::ensure!(start <= end, "--from {from} is after --to {to}");
    Ok(between(start, end, offset, format!("{from} to {to}")))
}

/// A range covering everything ever recorded.
pub(crate) fn all_time(offset: UtcOffset) -> Range {
    Range {
        from_us: 0,
        to_us: i64::MAX,
        offset,
        label: "all time".to_owned(),
    }
}

/// Builds a range from local midnight on `start` to local midnight after `end`.
fn between(start: Date, end: Date, offset: UtcOffset, label: String) -> Range {
    let from = start.midnight().assume_offset(offset);
    let to = (end + Duration::days(1)).midnight().assume_offset(offset);

    Range {
        from_us: micros(from),
        to_us: micros(to),
        offset,
        label,
    }
}

/// Converts to microseconds since the Unix epoch, saturating.
fn micros(value: OffsetDateTime) -> i64 {
    i64::try_from(value.unix_timestamp_nanos() / 1_000).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asia/Manila, the zone this was developed in.
    fn manila() -> UtcOffset {
        UtcOffset::from_hms(8, 0, 0).expect("valid offset")
    }

    #[test]
    fn an_explicit_range_spans_whole_local_days() {
        let range = explicit("2026-08-01", "2026-08-01", manila()).expect("parses");

        // 2026-08-01T00:00+08 is 2026-07-31T16:00Z, and the range is one day long.
        assert_eq!(range.to_us - range.from_us, 86_400 * 1_000_000);
    }

    #[test]
    fn the_same_dates_differ_between_zones() {
        let manila_range = explicit("2026-08-01", "2026-08-01", manila()).expect("parses");
        let utc_range = explicit("2026-08-01", "2026-08-01", UtcOffset::UTC).expect("parses");

        assert_eq!(
            utc_range.from_us - manila_range.from_us,
            8 * 3600 * 1_000_000,
            "a Manila day starts eight hours before the UTC day of the same name"
        );
    }

    #[test]
    fn an_inverted_range_is_rejected() {
        assert!(explicit("2026-08-21", "2026-08-01", manila()).is_err());
    }

    #[test]
    fn an_unparseable_date_names_the_expected_format() {
        let error = explicit("yesterday", "2026-08-01", manila()).expect_err("rejected");
        assert!(format!("{error}").contains("YYYY-MM-DD"));
    }

    #[test]
    fn one_day_is_labelled_today() {
        assert_eq!(last_days(1, manila()).label, "today");
    }

    #[test]
    fn seven_days_spans_seven_whole_days() {
        let range = last_days(7, manila());
        assert_eq!(range.to_us - range.from_us, 7 * 86_400 * 1_000_000);
    }

    #[test]
    fn all_time_starts_at_the_epoch() {
        assert_eq!(all_time(manila()).from_us, 0);
    }
}
