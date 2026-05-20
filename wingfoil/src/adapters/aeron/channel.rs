//! Type-safe builders for Aeron channel URIs.
//!
//! Constructing Aeron channel URIs by hand is error-prone — typos in the
//! `aeron:udp?endpoint=...` syntax are silently accepted by the media driver
//! and surface only as a non-connecting publication. The [`ChannelUri`]
//! helpers below produce the canonical strings for the most common channel
//! shapes used by `wingfoil`-based applications.

use std::net::Ipv6Addr;
use std::str::FromStr;

use super::TransportError;

/// ASCII punctuation accepted in URI parameter values.
///
/// Covers IPv4 host/port (`.`, `:`), bracketed IPv6 (`[`, `]`), DNS hostnames
/// (`.`, `-`), and identifier characters (`_`). The validator is an
/// **allowlist** — any character outside this set (including non-ASCII letters,
/// Unicode invisibles, whitespace, and Aeron URI separators `|?=#,;`) is
/// rejected.
const URI_ALLOWED_PUNCT: &[char] = &[':', '[', ']', '.', '-', '_'];

fn is_uri_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || URI_ALLOWED_PUNCT.contains(&c)
}

/// Validates an Aeron URI parameter value: non-empty and ASCII allowlist.
fn validate_param(label: &str, value: &str) -> Result<(), TransportError> {
    if value.is_empty() {
        return Err(TransportError::Invalid(format!(
            "{label} must not be empty"
        )));
    }
    if let Some(ch) = value.chars().find(|c| !is_uri_char(*c)) {
        return Err(TransportError::Invalid(format!(
            "{label} contains invalid character '{ch}' (U+{:04X}); only ASCII alphanumerics and ':[].-_' are permitted",
            ch as u32
        )));
    }
    Ok(())
}

/// Validates that `value` is shaped like `host:port` or `[ipv6]:port`.
///
/// `port` must parse as a `u16`. Bare IPv6 (multiple colons, no brackets) is
/// rejected because it is ambiguous: `::1` could mean host `::1` with no port
/// or host `:` port `:1`.
fn validate_host_port(label: &str, value: &str) -> Result<(), TransportError> {
    validate_param(label, value)?;

    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let close = rest.find(']').ok_or_else(|| {
            TransportError::Invalid(format!(
                "{label} bracketed IPv6 address missing closing ']' in '{value}'"
            ))
        })?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let port = after.strip_prefix(':').ok_or_else(|| {
            TransportError::Invalid(format!(
                "{label} bracketed IPv6 address must be followed by ':port' in '{value}'"
            ))
        })?;
        Ipv6Addr::from_str(host).map_err(|_| {
            TransportError::Invalid(format!(
                "{label} bracketed IPv6 '{host}' is not a valid IPv6 address in '{value}'"
            ))
        })?;
        (host, port)
    } else {
        let colons = value.matches(':').count();
        if colons == 0 {
            return Err(TransportError::Invalid(format!(
                "{label} expected 'host:port' in '{value}'"
            )));
        }
        if colons > 1 {
            return Err(TransportError::Invalid(format!(
                "{label} bare IPv6 must be bracketed like '[::1]:port' (got '{value}')"
            )));
        }
        // Exactly one ':' so split is infallible.
        let (host, port) = value.split_once(':').unwrap();
        if host.contains('[') || host.contains(']') {
            return Err(TransportError::Invalid(format!(
                "{label} brackets are only allowed as the bracketed-IPv6 prefix '[ipv6]:port' (got '{value}')"
            )));
        }
        (host, port)
    };

    if host.is_empty() {
        return Err(TransportError::Invalid(format!(
            "{label} host part must not be empty"
        )));
    }
    port.parse::<u16>().map_err(|_| {
        TransportError::Invalid(format!(
            "{label} port '{port}' must be a valid u16 (0-65535)"
        ))
    })?;
    Ok(())
}

/// Builders for Aeron channel URI strings.
///
/// Parameterised constructors return `Result<String, TransportError>`. They
/// reject inputs that are empty, contain non-ASCII or non-allowlist characters
/// (Unicode invisibles, whitespace, control chars, Aeron URI separators
/// `|?=#,;`), or do not match the expected `host:port` shape (with bracketed
/// IPv6 supported as `[::1]:port`). These checks run at startup, not on the
/// hot path.
#[derive(Debug)]
pub struct ChannelUri;

impl ChannelUri {
    /// Returns the IPC channel URI: `aeron:ipc`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use wingfoil::adapters::aeron::ChannelUri;
    /// assert_eq!(ChannelUri::ipc(), "aeron:ipc");
    /// ```
    pub fn ipc() -> String {
        "aeron:ipc".to_string()
    }

    /// Returns a UDP unicast channel URI: `aeron:udp?endpoint={endpoint}`.
    ///
    /// `endpoint` must be `host:port` (or `[ipv6]:port`).
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Invalid`] if `endpoint` is empty, contains
    /// disallowed characters, or does not match the `host:port` shape.
    ///
    /// # Examples
    ///
    /// ```
    /// # use wingfoil::adapters::aeron::ChannelUri;
    /// assert_eq!(
    ///     ChannelUri::udp("127.0.0.1:40123").unwrap(),
    ///     "aeron:udp?endpoint=127.0.0.1:40123"
    /// );
    /// ```
    pub fn udp(endpoint: &str) -> Result<String, TransportError> {
        validate_host_port("endpoint", endpoint)?;
        Ok(format!("aeron:udp?endpoint={endpoint}"))
    }

    /// Returns an MDC publication channel URI:
    /// `aeron:udp?control={control}|control-mode=dynamic`.
    ///
    /// Use this for the publisher side of a Multi-Destination-Cast stream.
    /// `control` must be `host:port` (or `[ipv6]:port`).
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Invalid`] if `control` is empty, contains
    /// disallowed characters, or does not match the `host:port` shape.
    ///
    /// # Examples
    ///
    /// ```
    /// # use wingfoil::adapters::aeron::ChannelUri;
    /// assert_eq!(
    ///     ChannelUri::mdc_publication("127.0.0.1:40456").unwrap(),
    ///     "aeron:udp?control=127.0.0.1:40456|control-mode=dynamic"
    /// );
    /// ```
    pub fn mdc_publication(control: &str) -> Result<String, TransportError> {
        validate_host_port("control", control)?;
        Ok(format!("aeron:udp?control={control}|control-mode=dynamic"))
    }

    /// Returns an MDC subscription channel URI:
    /// `aeron:udp?endpoint={endpoint}|control={control}|control-mode=dynamic`.
    ///
    /// Use this for the subscriber side of a Multi-Destination-Cast stream.
    /// Both `endpoint` and `control` must be `host:port` (or `[ipv6]:port`).
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Invalid`] if either parameter is empty,
    /// contains disallowed characters, or does not match the `host:port` shape.
    ///
    /// # Examples
    ///
    /// ```
    /// # use wingfoil::adapters::aeron::ChannelUri;
    /// assert_eq!(
    ///     ChannelUri::mdc_subscription("127.0.0.1:40789", "127.0.0.1:40456").unwrap(),
    ///     "aeron:udp?endpoint=127.0.0.1:40789|control=127.0.0.1:40456|control-mode=dynamic"
    /// );
    /// ```
    pub fn mdc_subscription(endpoint: &str, control: &str) -> Result<String, TransportError> {
        validate_host_port("endpoint", endpoint)?;
        validate_host_port("control", control)?;
        Ok(format!(
            "aeron:udp?endpoint={endpoint}|control={control}|control-mode=dynamic"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_channel_uri_when_ipc_then_returns_aeron_ipc() {
        assert_eq!(ChannelUri::ipc(), "aeron:ipc");
    }

    #[test]
    fn given_channel_uri_when_udp_then_formats_endpoint() {
        assert_eq!(
            ChannelUri::udp("127.0.0.1:40123").unwrap(),
            "aeron:udp?endpoint=127.0.0.1:40123"
        );
    }

    #[test]
    fn given_channel_uri_when_udp_with_bracketed_ipv6_then_formats_endpoint() {
        assert_eq!(
            ChannelUri::udp("[::1]:40123").unwrap(),
            "aeron:udp?endpoint=[::1]:40123"
        );
    }

    #[test]
    fn given_channel_uri_when_udp_with_hostname_then_formats_endpoint() {
        assert_eq!(
            ChannelUri::udp("aeron-host.example.com:40123").unwrap(),
            "aeron:udp?endpoint=aeron-host.example.com:40123"
        );
    }

    #[test]
    fn given_channel_uri_when_mdc_publication_then_formats_control_dynamic() {
        assert_eq!(
            ChannelUri::mdc_publication("127.0.0.1:40456").unwrap(),
            "aeron:udp?control=127.0.0.1:40456|control-mode=dynamic"
        );
    }

    #[test]
    fn given_channel_uri_when_mdc_subscription_then_formats_endpoint_and_control() {
        assert_eq!(
            ChannelUri::mdc_subscription("127.0.0.1:40789", "127.0.0.1:40456").unwrap(),
            "aeron:udp?endpoint=127.0.0.1:40789|control=127.0.0.1:40456|control-mode=dynamic"
        );
    }

    #[test]
    fn given_channel_uri_when_udp_empty_endpoint_then_returns_error() {
        let err = ChannelUri::udp("").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_pipe_in_endpoint_then_returns_error() {
        let err = ChannelUri::udp("host|control-mode=dynamic").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_question_mark_in_endpoint_then_returns_error() {
        let err = ChannelUri::udp("host?evil=1").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_mdc_publication_empty_control_then_returns_error() {
        let err = ChannelUri::mdc_publication("").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_mdc_subscription_empty_endpoint_then_returns_error() {
        let err = ChannelUri::mdc_subscription("", "127.0.0.1:40456").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_mdc_subscription_empty_control_then_returns_error() {
        let err = ChannelUri::mdc_subscription("127.0.0.1:40789", "").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_equals_in_endpoint_then_returns_error() {
        let err = ChannelUri::udp("host:1234=evil").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_hash_in_endpoint_then_returns_error() {
        let err = ChannelUri::udp("host:1234#frag").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_space_in_endpoint_then_returns_error() {
        let err = ChannelUri::udp("host 1234").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_comma_in_endpoint_then_returns_error() {
        let err = ChannelUri::udp("host1:1,host2:2").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_semicolon_in_endpoint_then_returns_error() {
        let err = ChannelUri::udp("host:1234;evil").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_non_ascii_in_endpoint_then_returns_error() {
        // Cyrillic 'а' (U+0430) is visually identical to Latin 'a' (U+0061).
        let err = ChannelUri::udp("\u{0430}.example.com:1234").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_zero_width_space_in_endpoint_then_returns_error() {
        // U+200B (ZWSP) is not matched by char::is_whitespace.
        let err = ChannelUri::udp("127.0.0.1:40123\u{200B}").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_no_colon_then_returns_error() {
        let err = ChannelUri::udp("hostonly").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_bare_ipv6_then_returns_error() {
        let err = ChannelUri::udp("::1").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_port_too_large_then_returns_error() {
        let err = ChannelUri::udp("127.0.0.1:65536").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_non_numeric_port_then_returns_error() {
        let err = ChannelUri::udp("127.0.0.1:abc").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_empty_host_then_returns_error() {
        let err = ChannelUri::udp(":1234").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_unclosed_bracket_then_returns_error() {
        let err = ChannelUri::udp("[::1:1234").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_udp_bracket_without_port_then_returns_error() {
        let err = ChannelUri::udp("[::1]").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_mdc_publication_no_colon_then_returns_error() {
        let err = ChannelUri::mdc_publication("hostonly").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_channel_uri_when_mdc_subscription_endpoint_no_colon_then_returns_error() {
        let err = ChannelUri::mdc_subscription("hostonly", "127.0.0.1:40456").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_validate_host_port_when_brackets_outside_prefix_then_returns_error() {
        let err = ChannelUri::udp("host[evil]:1234").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_validate_host_port_when_bracketed_hostname_non_hex_then_returns_error() {
        let err = ChannelUri::udp("[hello-world]:80").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_validate_host_port_when_bracketed_colon_only_then_returns_error() {
        let err = ChannelUri::udp("[:]:80").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_validate_host_port_when_bracketed_ipv4_then_returns_error() {
        let err = ChannelUri::udp("[127.0.0.1]:80").unwrap_err();
        assert!(matches!(err, TransportError::Invalid(_)));
    }

    #[test]
    fn given_validate_host_port_when_zero_colons_then_error_mentions_host_port() {
        let err = ChannelUri::udp("hostonly").unwrap_err();
        match err {
            TransportError::Invalid(msg) => {
                assert!(
                    msg.contains("host:port"),
                    "expected error to mention 'host:port' for zero-colon input, got: {msg}"
                );
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn given_validate_host_port_when_multiple_colons_then_error_mentions_bracketed_ipv6() {
        let err = ChannelUri::udp("::1").unwrap_err();
        match err {
            TransportError::Invalid(msg) => {
                assert!(
                    msg.contains("bracketed"),
                    "expected error to mention bracketed-IPv6 form for bare IPv6 input, got: {msg}"
                );
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }
}
