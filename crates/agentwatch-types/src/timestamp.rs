//! Wall-clock timestamps.

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Microseconds in a second.
const MICROS_PER_SECOND: i128 = 1_000_000;

/// A UTC instant stored as microseconds since the Unix epoch.
///
/// Integer microseconds rather than a formatted string: it sorts correctly as a
/// SQLite `INTEGER`, ranges cheaply, and leaves calendar bucketing to the query
/// layer where the user's timezone is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// Reads the current time.
    #[must_use]
    pub fn now() -> Self {
        Self::from_datetime(OffsetDateTime::now_utc())
    }

    /// Wraps a raw microsecond count.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Returns microseconds since the Unix epoch.
    #[must_use]
    pub const fn as_micros(self) -> i64 {
        self.0
    }

    /// Converts from a [`OffsetDateTime`], saturating at the representable range.
    #[must_use]
    pub fn from_datetime(value: OffsetDateTime) -> Self {
        let micros = value.unix_timestamp_nanos() / 1_000;
        Self(i64::try_from(micros).unwrap_or(i64::MAX))
    }

    /// Converts to an [`OffsetDateTime`] in UTC.
    ///
    /// # Errors
    ///
    /// Returns an error if the stored value is outside the range `time` can
    /// represent, which only happens for corrupted rows.
    pub fn to_datetime(self) -> Result<OffsetDateTime, time::error::ComponentRange> {
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(self.0) * 1_000)
    }

    /// Parses an RFC 3339 timestamp, such as `2026-08-20T17:22:02.051Z`.
    ///
    /// # Errors
    ///
    /// Returns an error if the text is not valid RFC 3339.
    pub fn parse_rfc3339(text: &str) -> Result<Self, time::error::Parse> {
        OffsetDateTime::parse(text, &Rfc3339).map(Self::from_datetime)
    }

    /// Formats as RFC 3339, falling back to the raw microsecond count.
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.to_datetime()
            .ok()
            .and_then(|dt| dt.format(&Rfc3339).ok())
            .unwrap_or_else(|| self.0.to_string())
    }

    /// Whole seconds since the Unix epoch, rounding toward negative infinity.
    #[must_use]
    pub const fn as_unix_seconds(self) -> i64 {
        self.0.div_euclid(MICROS_PER_SECOND as i64)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_datetime() {
        let original = Timestamp::from_micros(1_755_000_000_123_456);
        let datetime = original.to_datetime().expect("representable");
        assert_eq!(Timestamp::from_datetime(datetime), original);
    }

    #[test]
    fn formats_as_rfc3339() {
        let stamp = Timestamp::from_micros(0);
        assert_eq!(stamp.to_rfc3339(), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn truncates_to_whole_seconds() {
        let stamp = Timestamp::from_micros(1_500_000);
        assert_eq!(stamp.as_unix_seconds(), 1);
    }

    #[test]
    fn parses_the_transcript_timestamp_format() {
        let stamp = Timestamp::parse_rfc3339("2026-08-20T17:22:02.051Z").expect("parses");
        assert_eq!(stamp.to_rfc3339(), "2026-08-20T17:22:02.051Z");
    }

    #[test]
    fn rejects_text_that_is_not_a_timestamp() {
        assert!(Timestamp::parse_rfc3339("yesterday").is_err());
    }

    #[test]
    fn now_is_after_the_epoch() {
        assert!(Timestamp::now().as_micros() > 1_700_000_000_000_000);
    }
}
