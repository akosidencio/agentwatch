//! Adapter lookup.

use std::collections::HashMap;

use agentwatch_adapter_claude::ClaudeAdapter;
use agentwatch_events::{AdapterError, AgentEvent, HookAdapter, HookEnvelope, PROTOCOL_VERSION};

/// Routes envelopes to the adapter that claims their source.
#[derive(Default)]
pub struct AdapterRegistry {
    adapters: HashMap<&'static str, Box<dyn HookAdapter>>,
}

impl std::fmt::Debug for AdapterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterRegistry")
            .field("sources", &self.adapters.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl AdapterRegistry {
    /// Builds an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a registry with every adapter this build ships.
    #[must_use]
    pub fn with_builtin_adapters() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(ClaudeAdapter::new()));
        registry
    }

    /// Adds an adapter, replacing any adapter claiming the same source.
    pub fn register(&mut self, adapter: Box<dyn HookAdapter>) {
        self.adapters.insert(adapter.source(), adapter);
    }

    /// Normalizes an envelope using the adapter that claims its source.
    ///
    /// # Errors
    ///
    /// Returns an error if the protocol version is unsupported, no adapter
    /// claims the source, or the payload does not parse.
    pub fn normalize(&self, envelope: &HookEnvelope) -> Result<AgentEvent, AdapterError> {
        if envelope.v != PROTOCOL_VERSION {
            return Err(AdapterError::UnsupportedProtocol {
                version: envelope.v,
                expected: PROTOCOL_VERSION,
            });
        }

        let adapter = self.adapters.get(envelope.source.as_str()).ok_or_else(|| {
            AdapterError::UnknownSource {
                declared: envelope.source.clone(),
            }
        })?;

        adapter.normalize(envelope)
    }
}

#[cfg(test)]
mod tests {
    use agentwatch_types::Timestamp;
    use serde_json::value::RawValue;

    use super::*;

    fn envelope(version: u16, source: &str) -> HookEnvelope {
        let json = serde_json::json!({
            "v": version,
            "source": source,
            "sent_at": Timestamp::now().as_micros(),
            "hook_version": "0.1.0",
            "payload": RawValue::from_string(
                r#"{"hook_event_name":"SessionStart"}"#.to_owned()
            ).expect("valid json"),
        });
        serde_json::from_value(json).expect("valid envelope")
    }

    #[test]
    fn routes_a_claude_envelope_to_the_claude_adapter() {
        let registry = AdapterRegistry::with_builtin_adapters();
        let event = registry.normalize(&envelope(PROTOCOL_VERSION, "claude-code"));
        assert!(event.is_ok());
    }

    #[test]
    fn rejects_an_unknown_source() {
        let registry = AdapterRegistry::with_builtin_adapters();
        let error = registry.normalize(&envelope(PROTOCOL_VERSION, "nonesuch"));
        assert!(matches!(error, Err(AdapterError::UnknownSource { .. })));
    }

    #[test]
    fn rejects_a_future_protocol_version() {
        let registry = AdapterRegistry::with_builtin_adapters();
        let error = registry.normalize(&envelope(PROTOCOL_VERSION + 1, "claude-code"));
        assert!(matches!(
            error,
            Err(AdapterError::UnsupportedProtocol { .. })
        ));
    }
}
