//! Transport error types for the Aeron adapter.

use std::fmt;

/// Unified error type for transport operations.
///
/// This error type wraps errors from the underlying Aeron client and enables
/// uniform error handling in application code.
#[derive(Debug)]
#[non_exhaustive]
pub enum TransportError {
    /// Back-pressure condition: buffer is full, retry later.
    ///
    /// Returned when the Aeron publication buffer cannot accept new messages.
    /// In HFT systems this typically indicates the need for flow control or
    /// rate limiting.
    BackPressure,

    /// Connection-related errors (not connected, closed, etc.).
    Connection(String),

    /// Backend-specific error with descriptive message.
    ///
    /// Contains error details from the underlying Aeron client.
    Backend(String),

    /// Invalid operation or parameters.
    Invalid(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::BackPressure => write!(f, "Back-pressure: buffer full"),
            TransportError::Connection(msg) => write!(f, "Connection error: {}", msg),
            TransportError::Backend(msg) => write!(f, "Backend error: {}", msg),
            TransportError::Invalid(msg) => write!(f, "Invalid operation: {}", msg),
        }
    }
}

impl std::error::Error for TransportError {}

// `From<TransportError> for anyhow::Error` is supplied by anyhow's blanket
// `impl<E: std::error::Error + Send + Sync + 'static> From<E> for anyhow::Error`.
// `cycle()`'s `anyhow::Result<bool>` return therefore accepts `?` on a
// `Result<_, TransportError>` without an explicit conversion in this crate.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_back_pressure_transport_error_when_display_called_then_returns_buffer_full_message() {
        let err = TransportError::BackPressure;
        assert_eq!(format!("{}", err), "Back-pressure: buffer full");
    }

    #[test]
    fn given_connection_transport_error_with_message_when_display_called_then_includes_message() {
        let err = TransportError::Connection("subscriber not connected".to_string());
        assert_eq!(
            format!("{}", err),
            "Connection error: subscriber not connected"
        );
    }

    #[test]
    fn given_backend_transport_error_when_display_called_then_includes_message() {
        let err = TransportError::Backend("rusteron init failed".to_string());
        assert_eq!(format!("{}", err), "Backend error: rusteron init failed");
    }

    #[test]
    fn given_invalid_transport_error_when_display_called_then_includes_message() {
        let err = TransportError::Invalid("zero length claim".to_string());
        assert_eq!(format!("{}", err), "Invalid operation: zero length claim");
    }

    #[test]
    fn given_back_pressure_transport_error_when_converted_to_anyhow_then_display_round_trips() {
        let original = TransportError::BackPressure;
        let display_before = format!("{}", original);
        let err: anyhow::Error = original.into();
        assert_eq!(format!("{}", err), display_before);
    }

    #[test]
    fn given_invalid_transport_error_when_converted_to_anyhow_then_downcasts_back() {
        let err: anyhow::Error = TransportError::Invalid("nope".to_string()).into();
        let downcast = err
            .downcast_ref::<TransportError>()
            .expect("anyhow round-trips TransportError via downcast_ref");
        assert!(matches!(downcast, TransportError::Invalid(msg) if msg == "nope"));
    }
}
