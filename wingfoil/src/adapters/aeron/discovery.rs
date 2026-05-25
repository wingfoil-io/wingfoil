//! Named publisher / subscriber discovery.
//!
//! Provides a process-global, in-memory registry that maps logical names to
//! `(channel, stream_id)` tuples. Applications register their endpoints at
//! startup; consumers either look the registered tuple up directly via
//! [`lookup_pub`] / [`lookup_sub`], or hand a separately-constructed publisher
//! (or subscriber) through [`aeron_pub_named`] / [`aeron_sub_named`] to assert
//! that the name is known to the registry.
//!
//! [`aeron_pub_named`] and [`aeron_sub_named`] are pass-through guards — they
//! validate the name and return the supplied publisher/subscriber unchanged.
//! They do **not** construct a publisher from the registered `(channel,
//! stream_id)` tuple; callers wire their own backend and use the registry
//! purely as a name-resolution check. A future story may add a resolver-style
//! API once a real consumer drives the requirements.
//!
//! The registry is intentionally **in-process only**; file or network
//! discovery is out of scope.
//!
//! # Tracing
//!
//! Mutex-poison recovery emits a `tracing::warn!` event; the host process must
//! install a `tracing` subscriber to observe it. Wingfoil's `tracing` crate
//! dependency is non-optional, so the call is always compiled.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use super::transport::{AeronPublisherBackend, AeronSubscriberBackend};

/// Errors returned by named discovery operations.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiscoveryError {
    /// No publisher or subscriber has been registered under the given name.
    Unknown(String),
    /// Registration or lookup rejected because the name is empty or
    /// whitespace-only.
    EmptyName,
    /// Registration or lookup rejected because the name has leading/trailing
    /// whitespace or contains a character outside the ASCII allowlist
    /// (alphanumerics plus `_`, `-`, `.`).
    InvalidName(String),
    /// Registration rejected because the channel string is empty.
    EmptyChannel,
    /// Registration rejected because `stream_id` is not strictly positive.
    InvalidStreamId(i32),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscoveryError::Unknown(name) => write!(f, "unknown discovery name: {name}"),
            DiscoveryError::EmptyName => write!(f, "discovery name must not be empty"),
            DiscoveryError::InvalidName(reason) => write!(f, "invalid discovery name: {reason}"),
            DiscoveryError::EmptyChannel => write!(f, "discovery channel must not be empty"),
            DiscoveryError::InvalidStreamId(id) => {
                write!(f, "discovery stream_id must be > 0, got {id}")
            }
        }
    }
}

impl std::error::Error for DiscoveryError {}

type Registry = OnceLock<Mutex<HashMap<String, (String, i32)>>>;

static PUB_REGISTRY: Registry = OnceLock::new();
static SUB_REGISTRY: Registry = OnceLock::new();

fn registry(reg: &'static Registry) -> &'static Mutex<HashMap<String, (String, i32)>> {
    reg.get_or_init(|| Mutex::new(HashMap::new()))
}

fn recover<T>(label: &'static str, p: PoisonError<T>) -> T {
    tracing::warn!(
        registry = label,
        "wingfoil discovery: registry mutex was poisoned by an earlier panic; \
         recovering inner state. Map contents may be inconsistent."
    );
    p.into_inner()
}

fn lock_pub() -> MutexGuard<'static, HashMap<String, (String, i32)>> {
    registry(&PUB_REGISTRY)
        .lock()
        .unwrap_or_else(|p| recover("pub", p))
}

fn lock_sub() -> MutexGuard<'static, HashMap<String, (String, i32)>> {
    registry(&SUB_REGISTRY)
        .lock()
        .unwrap_or_else(|p| recover("sub", p))
}

fn validate_name(name: &str) -> Result<(), DiscoveryError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(DiscoveryError::EmptyName);
    }
    if trimmed != name {
        return Err(DiscoveryError::InvalidName(format!(
            "leading/trailing whitespace in {name:?}"
        )));
    }
    if let Some(ch) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(*c, '_' | '-' | '.')))
    {
        return Err(DiscoveryError::InvalidName(format!(
            "disallowed character U+{:04X} in name (only ASCII alphanumerics and '_', '-', '.' permitted)",
            ch as u32
        )));
    }
    Ok(())
}

fn validate_registration(channel: &str, stream_id: i32) -> Result<(), DiscoveryError> {
    if channel.is_empty() {
        return Err(DiscoveryError::EmptyChannel);
    }
    if stream_id <= 0 {
        return Err(DiscoveryError::InvalidStreamId(stream_id));
    }
    Ok(())
}

/// Registers a publisher endpoint under `name`.
///
/// Re-registering an existing name **overwrites** the prior entry. This is
/// deterministic and matches the typical "configuration applied last wins"
/// expectation from application startup code.
///
/// # Errors
///
/// - [`DiscoveryError::EmptyName`] if `name` is empty or whitespace-only.
/// - [`DiscoveryError::InvalidName`] if `name` has leading/trailing whitespace
///   or contains control characters.
/// - [`DiscoveryError::EmptyChannel`] if `channel` is empty.
/// - [`DiscoveryError::InvalidStreamId`] if `stream_id <= 0`.
#[must_use = "registration may fail validation; check the Result"]
pub fn register_pub(name: &str, channel: String, stream_id: i32) -> Result<(), DiscoveryError> {
    validate_name(name)?;
    validate_registration(&channel, stream_id)?;
    let mut map = lock_pub();
    map.insert(name.to_string(), (channel, stream_id));
    Ok(())
}

/// Registers a subscriber endpoint under `name`.
///
/// Re-registering an existing name **overwrites** the prior entry.
///
/// # Errors
///
/// Same set as [`register_pub`].
#[must_use = "registration may fail validation; check the Result"]
pub fn register_sub(name: &str, channel: String, stream_id: i32) -> Result<(), DiscoveryError> {
    validate_name(name)?;
    validate_registration(&channel, stream_id)?;
    let mut map = lock_sub();
    map.insert(name.to_string(), (channel, stream_id));
    Ok(())
}

/// Returns the registered `(channel, stream_id)` for a publisher name, if any.
///
/// Returns `None` for any name that fails [`validate_name`] (empty,
/// whitespace-only, leading/trailing whitespace, control characters) or that
/// has not been registered. If you need to distinguish "name invalid" from
/// "name unregistered", call [`aeron_pub_named`] instead.
pub fn lookup_pub(name: &str) -> Option<(String, i32)> {
    if validate_name(name).is_err() {
        return None;
    }
    let map = lock_pub();
    map.get(name).cloned()
}

/// Returns the registered `(channel, stream_id)` for a subscriber name, if any.
///
/// See [`lookup_pub`] for behaviour on invalid names.
pub fn lookup_sub(name: &str) -> Option<(String, i32)> {
    if validate_name(name).is_err() {
        return None;
    }
    let map = lock_sub();
    map.get(name).cloned()
}

/// Validates that `name` is a registered publisher and returns the supplied
/// publisher unchanged.
///
/// This is a pass-through guard intended for application startup wiring:
/// callers construct the publisher however their backend requires and then
/// hand it through `aeron_pub_named` to assert that the logical name is known
/// to the discovery layer. The registered `(channel, stream_id)` tuple is
/// **not** used to configure or rewrap the publisher.
///
/// Use this on the publisher side via the chained-call pattern:
/// `stream.aeron_pub(aeron_pub_named("name", backend)?, serialiser)`.
///
/// # Errors
///
/// - [`DiscoveryError::EmptyName`] / [`DiscoveryError::InvalidName`] if `name`
///   fails [`validate_name`].
/// - [`DiscoveryError::Unknown`] if `name` is well-formed but not registered.
pub fn aeron_pub_named<B: AeronPublisherBackend>(
    name: &str,
    publisher: B,
) -> Result<B, DiscoveryError> {
    validate_name(name)?;
    let map = lock_pub();
    if map.contains_key(name) {
        Ok(publisher)
    } else {
        Err(DiscoveryError::Unknown(name.to_string()))
    }
}

/// Validates that `name` is a registered subscriber and returns the supplied
/// subscriber unchanged.
///
/// This is the symmetric counterpart of [`aeron_pub_named`]. See its docs for
/// the pass-through-guard contract.
///
/// # Errors
///
/// Same set as [`aeron_pub_named`].
pub fn aeron_sub_named<B: AeronSubscriberBackend>(
    name: &str,
    subscriber: B,
) -> Result<B, DiscoveryError> {
    validate_name(name)?;
    let map = lock_sub();
    if map.contains_key(name) {
        Ok(subscriber)
    } else {
        Err(DiscoveryError::Unknown(name.to_string()))
    }
}

/// Deprecated alias for [`aeron_sub_named`].
///
/// Retained for one minor release after the rename so existing callers compile
/// without churn. New code should call [`aeron_sub_named`].
#[deprecated(since = "0.2.0", note = "renamed to aeron_sub_named")]
pub fn aeron_sub_discover<B: AeronSubscriberBackend>(
    name: &str,
    subscriber: B,
) -> Result<B, DiscoveryError> {
    aeron_sub_named(name, subscriber)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct MockPublisher;

    impl AeronPublisherBackend for MockPublisher {
        fn offer(&mut self, _buffer: &[u8]) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct MockSubscriber;

    impl AeronSubscriberBackend for MockSubscriber {
        fn poll(&mut self, _handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize> {
            Ok(0)
        }
    }

    #[test]
    fn given_registered_pub_when_aeron_pub_named_then_returns_publisher() {
        register_pub("test_pub_round_trip", "aeron:ipc".to_string(), 42).unwrap();
        let result = aeron_pub_named("test_pub_round_trip", MockPublisher);
        assert!(result.is_ok());
        assert_eq!(
            lookup_pub("test_pub_round_trip"),
            Some(("aeron:ipc".to_string(), 42))
        );
    }

    #[test]
    fn given_unregistered_name_when_aeron_pub_named_then_returns_unknown() {
        let result = aeron_pub_named("test_pub_unregistered_xyz", MockPublisher);
        assert_eq!(
            result.err(),
            Some(DiscoveryError::Unknown(
                "test_pub_unregistered_xyz".to_string()
            ))
        );
    }

    #[test]
    fn given_registered_sub_when_aeron_sub_named_then_returns_subscriber() {
        register_sub(
            "test_sub_round_trip",
            "aeron:udp?endpoint=127.0.0.1:40000".to_string(),
            7,
        )
        .unwrap();
        let result = aeron_sub_named("test_sub_round_trip", MockSubscriber);
        assert!(result.is_ok());
        assert_eq!(
            lookup_sub("test_sub_round_trip"),
            Some(("aeron:udp?endpoint=127.0.0.1:40000".to_string(), 7))
        );
    }

    #[test]
    fn given_unregistered_name_when_aeron_sub_named_then_returns_unknown() {
        let result = aeron_sub_named("test_sub_unregistered_xyz", MockSubscriber);
        assert_eq!(
            result.err(),
            Some(DiscoveryError::Unknown(
                "test_sub_unregistered_xyz".to_string()
            ))
        );
    }

    #[test]
    #[allow(deprecated)]
    fn given_registered_sub_when_aeron_sub_discover_then_delegates() {
        register_sub("test_sub_discover_alias", "aeron:ipc".to_string(), 42).unwrap();
        let result = aeron_sub_discover("test_sub_discover_alias", MockSubscriber);
        assert!(result.is_ok());
    }

    #[test]
    #[allow(deprecated)]
    fn given_unregistered_name_when_aeron_sub_discover_then_returns_unknown() {
        let result = aeron_sub_discover("test_sub_discover_unregistered_xyz", MockSubscriber);
        assert_eq!(
            result.err(),
            Some(DiscoveryError::Unknown(
                "test_sub_discover_unregistered_xyz".to_string()
            ))
        );
    }

    #[test]
    fn given_registered_pub_when_re_registered_then_overwrites() {
        register_pub("test_pub_overwrite", "aeron:ipc".to_string(), 1).unwrap();
        register_pub(
            "test_pub_overwrite",
            "aeron:udp?endpoint=127.0.0.1:1".to_string(),
            2,
        )
        .unwrap();
        assert_eq!(
            lookup_pub("test_pub_overwrite"),
            Some(("aeron:udp?endpoint=127.0.0.1:1".to_string(), 2))
        );
    }

    #[test]
    fn given_registered_sub_when_re_registered_then_overwrites() {
        register_sub("test_sub_overwrite", "aeron:ipc".to_string(), 1).unwrap();
        register_sub(
            "test_sub_overwrite",
            "aeron:udp?endpoint=127.0.0.1:2".to_string(),
            9,
        )
        .unwrap();
        assert_eq!(
            lookup_sub("test_sub_overwrite"),
            Some(("aeron:udp?endpoint=127.0.0.1:2".to_string(), 9))
        );
    }

    #[test]
    fn given_empty_name_when_register_pub_then_returns_empty_name_error() {
        let err = register_pub("", "aeron:ipc".to_string(), 1).unwrap_err();
        assert_eq!(err, DiscoveryError::EmptyName);
    }

    #[test]
    fn given_empty_name_when_register_sub_then_returns_empty_name_error() {
        let err = register_sub("", "aeron:ipc".to_string(), 1).unwrap_err();
        assert_eq!(err, DiscoveryError::EmptyName);
    }

    #[test]
    fn given_discovery_error_when_display_then_includes_name() {
        let err = DiscoveryError::Unknown("foo".to_string());
        assert_eq!(format!("{err}"), "unknown discovery name: foo");
    }

    #[test]
    fn given_empty_name_error_when_display_then_describes_issue() {
        let err = DiscoveryError::EmptyName;
        assert_eq!(format!("{err}"), "discovery name must not be empty");
    }

    #[test]
    fn given_invalid_name_error_when_display_then_describes_issue() {
        let err = DiscoveryError::InvalidName(
            "disallowed character U+000A in name (only ASCII alphanumerics and '_', '-', '.' permitted)"
                .to_string(),
        );
        assert_eq!(
            format!("{err}"),
            "invalid discovery name: disallowed character U+000A in name (only ASCII alphanumerics and '_', '-', '.' permitted)"
        );
    }

    #[test]
    fn given_empty_channel_error_when_display_then_describes_issue() {
        let err = DiscoveryError::EmptyChannel;
        assert_eq!(format!("{err}"), "discovery channel must not be empty");
    }

    #[test]
    fn given_invalid_stream_id_error_when_display_then_describes_issue() {
        let err = DiscoveryError::InvalidStreamId(-1);
        assert_eq!(format!("{err}"), "discovery stream_id must be > 0, got -1");
    }

    #[test]
    fn given_unregistered_name_when_lookup_pub_then_returns_none() {
        assert_eq!(lookup_pub("test_lookup_pub_nonexistent"), None);
    }

    #[test]
    fn given_unregistered_name_when_lookup_sub_then_returns_none() {
        assert_eq!(lookup_sub("test_lookup_sub_nonexistent"), None);
    }

    #[test]
    fn given_whitespace_name_when_register_pub_then_returns_empty_name_error() {
        let err = register_pub("   ", "aeron:ipc".to_string(), 1).unwrap_err();
        assert_eq!(err, DiscoveryError::EmptyName);
    }

    #[test]
    fn given_whitespace_name_when_register_sub_then_returns_empty_name_error() {
        let err = register_sub("   ", "aeron:ipc".to_string(), 1).unwrap_err();
        assert_eq!(err, DiscoveryError::EmptyName);
    }

    #[test]
    fn given_empty_name_when_lookup_pub_then_returns_none() {
        assert_eq!(lookup_pub(""), None);
    }

    #[test]
    fn given_empty_name_when_lookup_sub_then_returns_none() {
        assert_eq!(lookup_sub(""), None);
    }

    #[test]
    fn given_leading_whitespace_name_when_register_pub_then_returns_invalid_name() {
        let err = register_pub(" test_pub_lead", "aeron:ipc".to_string(), 1).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_trailing_whitespace_name_when_register_sub_then_returns_invalid_name() {
        let err = register_sub("test_sub_trail ", "aeron:ipc".to_string(), 1).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_newline_in_name_when_register_pub_then_returns_invalid_name() {
        let err = register_pub("test_pub\nctrl", "aeron:ipc".to_string(), 1).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_tab_in_name_when_register_sub_then_returns_invalid_name() {
        let err = register_sub("test_sub\tctrl", "aeron:ipc".to_string(), 1).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_empty_channel_when_register_pub_then_returns_empty_channel() {
        let err = register_pub("test_pub_empty_chan", String::new(), 1).unwrap_err();
        assert_eq!(err, DiscoveryError::EmptyChannel);
    }

    #[test]
    fn given_empty_channel_when_register_sub_then_returns_empty_channel() {
        let err = register_sub("test_sub_empty_chan", String::new(), 1).unwrap_err();
        assert_eq!(err, DiscoveryError::EmptyChannel);
    }

    #[test]
    fn given_zero_stream_id_when_register_pub_then_returns_invalid_stream_id() {
        let err = register_pub("test_pub_zero_sid", "aeron:ipc".to_string(), 0).unwrap_err();
        assert_eq!(err, DiscoveryError::InvalidStreamId(0));
    }

    #[test]
    fn given_negative_stream_id_when_register_sub_then_returns_invalid_stream_id() {
        let err = register_sub("test_sub_neg_sid", "aeron:ipc".to_string(), -3).unwrap_err();
        assert_eq!(err, DiscoveryError::InvalidStreamId(-3));
    }

    #[test]
    fn given_empty_name_when_aeron_pub_named_then_returns_empty_name() {
        let err = aeron_pub_named("", MockPublisher).unwrap_err();
        assert_eq!(err, DiscoveryError::EmptyName);
    }

    #[test]
    fn given_whitespace_name_when_aeron_pub_named_then_returns_empty_name() {
        let err = aeron_pub_named("   ", MockPublisher).unwrap_err();
        assert_eq!(err, DiscoveryError::EmptyName);
    }

    #[test]
    fn given_invalid_name_when_aeron_pub_named_then_returns_invalid_name() {
        let err = aeron_pub_named("test_pub_aeron\nbad", MockPublisher).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_empty_name_when_aeron_sub_named_then_returns_empty_name() {
        let err = aeron_sub_named("", MockSubscriber).unwrap_err();
        assert_eq!(err, DiscoveryError::EmptyName);
    }

    #[test]
    fn given_invalid_name_when_aeron_sub_named_then_returns_invalid_name() {
        let err = aeron_sub_named(" test_sub_lead", MockSubscriber).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_validate_name_when_zwsp_then_returns_invalid_name() {
        let err = register_pub("test_pub_zwsp\u{200B}", "aeron:ipc".to_string(), 1).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_validate_name_when_cyrillic_a_then_returns_invalid_name() {
        // Cyrillic 'а' (U+0430) is visually identical to Latin 'a' (U+0061).
        let err = register_pub("test_pub_\u{0430}lpha", "aeron:ipc".to_string(), 1).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_validate_name_when_embedded_space_then_returns_invalid_name() {
        let err = register_pub("test pub embedded", "aeron:ipc".to_string(), 1).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_validate_name_when_dotted_then_accepts() {
        register_pub("test.pub.dotted", "aeron:ipc".to_string(), 1).unwrap();
        assert_eq!(
            lookup_pub("test.pub.dotted"),
            Some(("aeron:ipc".to_string(), 1))
        );
    }
}
