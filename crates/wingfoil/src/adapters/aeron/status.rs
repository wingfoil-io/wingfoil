//! Lifecycle status enum for an Aeron transport endpoint.

/// Lifecycle status of an Aeron transport endpoint.
///
/// Represents the connection state of a publisher or subscriber,
/// enabling status monitoring and lifecycle tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum AeronStatus {
    /// The endpoint is connected and actively communicating.
    Connected,
    /// The endpoint is not connected (initial state).
    #[default]
    Disconnected,
    /// The endpoint is experiencing back-pressure.
    BackPressured,
    /// The endpoint has been closed.
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_no_explicit_value_when_default_aeron_status_called_then_returns_disconnected() {
        assert_eq!(AeronStatus::default(), AeronStatus::Disconnected);
    }

    #[test]
    fn given_aeron_status_when_matched_then_each_variant_distinguishable() {
        // Same-crate matches treat `#[non_exhaustive]` enums as exhaustive,
        // so a wildcard arm here would be unreachable. The downstream-crate
        // compile-pass guarantee is provided by the attribute itself.
        let label = match AeronStatus::Connected {
            AeronStatus::Connected => "connected",
            AeronStatus::Disconnected => "disconnected",
            AeronStatus::BackPressured => "back-pressured",
            AeronStatus::Closed => "closed",
        };
        assert_eq!(label, "connected");
    }
}
