//! Where an event came from, and how much to trust it.
//!
//! Recording this on every event is what lets the UI avoid implying it observed
//! something it merely inferred.

use serde::{Deserialize, Serialize};

/// The mechanism that produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EvidenceSource {
    /// An agent hook fired synchronously as the action happened.
    Hook,
    /// Read back from the agent's own session transcript.
    Transcript,
    /// Reported by the agent over OpenTelemetry.
    OpenTelemetry,
    /// Observed by the operating system rather than the agent.
    OperatingSystem,
    /// Inferred by AgentWatch from other events.
    Derived,
}

impl EvidenceSource {
    /// The stable string stored in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Transcript => "transcript",
            Self::OpenTelemetry => "open_telemetry",
            Self::OperatingSystem => "operating_system",
            Self::Derived => "derived",
        }
    }
}

/// How confident AgentWatch is that an event describes what actually happened.
///
/// Always in `0.0..=1.0`; the constructor clamps rather than failing, because a
/// nonsensical confidence should never drop an otherwise good event.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Confidence(f32);

impl Confidence {
    /// Directly observed: the agent told us as it happened.
    pub const CERTAIN: Self = Self(1.0);

    /// Builds a confidence, clamping into range.
    #[must_use]
    pub fn new(value: f32) -> Self {
        Self(value.clamp(0.0, 1.0))
    }

    /// The underlying value.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.0
    }
}

impl Default for Confidence {
    fn default() -> Self {
        Self::CERTAIN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_clamps_out_of_range_values() {
        assert_eq!(Confidence::new(4.2).value(), 1.0);
        assert_eq!(Confidence::new(-1.0).value(), 0.0);
    }

    #[test]
    fn evidence_source_strings_are_stable() {
        assert_eq!(EvidenceSource::Hook.as_str(), "hook");
        assert_eq!(EvidenceSource::Transcript.as_str(), "transcript");
    }
}
