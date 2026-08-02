//! Borrowed fragment views + claim buffers for transport operations.
//!
//! `FragmentBuffer` exposes the borrowed `&[u8]` from a polled Aeron fragment,
//! tagged with a [`FragmentHeader`] (position, session, stream).
//! `ClaimBuffer` is a `&mut [u8]` view into a claimed slot in the publication
//! term buffer, with an explicit commit-or-abort lifecycle, produced by
//! `RusteronPublisher::try_claim`.

use std::marker::PhantomData;
use std::ops::Deref;

#[cfg(feature = "aeron")]
use rusteron_client::AeronBufferClaim;

#[cfg(feature = "aeron")]
use crate::adapters::aeron::error::TransportError;

// ---------------------------------------------------------------------------
// ClaimBuffer
// ---------------------------------------------------------------------------

/// A claimed slot in an Aeron publication's term buffer for direct writing.
///
/// `ClaimBuffer<'a>` binds its lifetime to the `&'a mut` borrow of the
/// publisher that produced it — the borrow checker therefore prevents a
/// concurrent `try_claim` / `offer` on the same publisher until this value is
/// dropped or consumed.
///
/// # Lifecycle
///
/// Aeron mandates a single-shot **commit-or-abort** contract on every claim.
/// A leaked claim blocks new publications on the same channel until the
/// `AERON_PUBLICATION_UNBLOCK_TIMEOUT_NS` default (15 seconds) elapses — far
/// too long for low-latency hot paths.
///
/// 1. Obtain a `ClaimBuffer` from a publisher's `try_claim` impl.
/// 2. Write payload bytes via [`data`](Self::data).
/// 3. Finalise with [`commit`](Self::commit) (publish) or
///    [`abort`](Self::abort) (discard). Both consume `self`, encoding the
///    one-shot contract in the type system.
///
/// If the value is dropped without an explicit `commit` / `abort` (e.g. on
/// panic), the [`Drop`] impl calls `abort()` as a defensive backstop. The
/// result of that abort is discarded.
///
/// # API note
///
/// `Deref` / `DerefMut` are deliberately **not** implemented. Routing all
/// buffer access through the explicit [`data`](Self::data) accessor keeps
/// the commit/abort invariant visible at every call site.
pub struct ClaimBuffer<'a> {
    // REVIEW NOTE: the rusteron-owned fields are feature-gated because
    // `AeronBufferClaim` only exists with the `aeron` feature.
    // Without the feature `ClaimBuffer` is publicly unconstructable — the
    // trait default `try_claim` returns `Err` before any construction — so
    // the type still appears in the trait signature without dragging rusteron
    // into every build configuration.
    #[cfg(feature = "aeron")]
    claim: AeronBufferClaim,
    #[cfg(feature = "aeron")]
    position: i64,
    #[cfg(feature = "aeron")]
    finalised: bool,
    _publisher: PhantomData<&'a mut ()>,
}

impl<'a> std::fmt::Debug for ClaimBuffer<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("ClaimBuffer");
        #[cfg(feature = "aeron")]
        {
            dbg.field("position", &self.position)
                .field("finalised", &self.finalised)
                .field("len", &self.claim.length());
        }
        dbg.finish_non_exhaustive()
    }
}

#[cfg(feature = "aeron")]
impl<'a> ClaimBuffer<'a> {
    /// Constructs a `ClaimBuffer` from a successful rusteron `try_claim` result.
    ///
    /// Crate-private — the only legitimate caller is
    /// `RusteronPublisher::try_claim`.
    pub(crate) fn from_aeron(claim: AeronBufferClaim, position: i64) -> Self {
        ClaimBuffer {
            claim,
            position,
            finalised: false,
            _publisher: PhantomData,
        }
    }

    /// Returns the writable claimed slice.
    pub fn data(&mut self) -> &mut [u8] {
        self.claim.data()
    }

    /// Returns the length of the claimed slice in bytes.
    pub fn len(&self) -> usize {
        self.claim.length()
    }

    /// Returns `true` if the claimed slice has zero length.
    pub fn is_empty(&self) -> bool {
        self.claim.length() == 0
    }

    /// Returns the stream position assigned to this claim by Aeron.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// Commits the claim, publishing the bytes to subscribers.
    ///
    /// Consumes `self`. On error the claim is also considered finalised
    /// (Aeron's commit semantics): the `Drop` backstop does not retry.
    pub fn commit(mut self) -> Result<(), TransportError> {
        self.finalised = true;
        self.claim
            .commit()
            .map(|_| ())
            .map_err(|e| TransportError::Backend(format!("aeron buffer commit: {:?}", e)))
    }

    /// Aborts the claim, discarding the slot as padding for subscribers.
    pub fn abort(mut self) -> Result<(), TransportError> {
        self.finalised = true;
        self.claim
            .abort()
            .map(|_| ())
            .map_err(|e| TransportError::Backend(format!("aeron buffer abort: {:?}", e)))
    }
}

impl<'a> Drop for ClaimBuffer<'a> {
    fn drop(&mut self) {
        #[cfg(feature = "aeron")]
        if !self.finalised {
            // Defensive backstop: aborting an un-finalised claim releases the
            // term-buffer slot immediately instead of waiting 15s for the
            // Aeron publication-unblock timeout. The Result is discarded
            // because Drop has nowhere to surface it.
            let _ = self.claim.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// FragmentBuffer / FragmentHeader
// ---------------------------------------------------------------------------

/// A received message fragment buffer.
///
/// Provides read-only access to a fragment polled from an Aeron subscription.
/// The data is borrowed directly from the Aeron buffer without copying.
#[derive(Debug)]
pub struct FragmentBuffer<'a> {
    buffer: &'a [u8],
    header: FragmentHeader,
}

/// Metadata about a received message fragment.
#[derive(Debug, Clone, Copy)]
pub struct FragmentHeader {
    /// Position in the stream where this fragment starts.
    pub position: i64,
    /// Session ID for this fragment.
    pub session_id: i32,
    /// Stream ID for this fragment.
    pub stream_id: i32,
}

impl<'a> FragmentBuffer<'a> {
    /// Creates a new fragment buffer wrapping the given slice with header metadata.
    pub fn new(buffer: &'a [u8], header: FragmentHeader) -> Self {
        FragmentBuffer { buffer, header }
    }

    /// Returns the length of the fragment data.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Returns true if the fragment is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Returns the fragment header metadata.
    pub fn header(&self) -> &FragmentHeader {
        &self.header
    }

    /// Returns the position in the stream for this fragment.
    pub fn position(&self) -> i64 {
        self.header.position
    }
}

impl<'a> Deref for FragmentBuffer<'a> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.buffer
    }
}

impl<'a> AsRef<[u8]> for FragmentBuffer<'a> {
    fn as_ref(&self) -> &[u8] {
        self.buffer
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn header(position: i64) -> FragmentHeader {
        FragmentHeader {
            position,
            session_id: 1,
            stream_id: 1001,
        }
    }

    #[test]
    fn given_constructed_fragment_buffer_when_header_called_then_returns_constructor_header() {
        let data: [u8; 4] = [1, 2, 3, 4];
        let buf = FragmentBuffer::new(&data, header(4242));
        let got = buf.header();
        assert_eq!(got.position, 4242);
        assert_eq!(got.session_id, 1);
        assert_eq!(got.stream_id, 1001);
    }

    #[test]
    fn given_constructed_fragment_buffer_when_position_called_then_returns_header_position() {
        let data: [u8; 1] = [0];
        let buf = FragmentBuffer::new(&data, header(99));
        assert_eq!(buf.position(), 99);
    }

    #[test]
    fn given_empty_fragment_buffer_when_is_empty_called_then_returns_true() {
        let data: [u8; 0] = [];
        let buf = FragmentBuffer::new(&data, header(0));
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn given_non_empty_fragment_buffer_when_deref_called_then_yields_source_slice() {
        let data: [u8; 3] = [9, 8, 7];
        let buf = FragmentBuffer::new(&data, header(0));
        let slice: &[u8] = &buf;
        assert_eq!(slice, &data[..]);
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn given_non_empty_fragment_buffer_when_as_ref_called_then_yields_source_slice() {
        let data: [u8; 3] = [10, 20, 30];
        let buf = FragmentBuffer::new(&data, header(0));
        assert_eq!(buf.as_ref(), &data[..]);
    }

    // ClaimBuffer tests need rusteron's AeronBufferClaim default to construct
    // a "defused" claim whose finalised flag is pre-set so Drop does not call
    // the FFI on a zero-default (null inner ptr) claim.
    #[cfg(feature = "aeron")]
    mod claim_buffer {
        use super::super::*;
        use rusteron_client::AeronBufferClaim;

        /// Constructs a `ClaimBuffer` whose `finalised` flag is pre-set so its
        /// `Drop` impl does **not** invoke the Aeron FFI (the zero-default
        /// claim has a null inner pointer, which would segfault on abort).
        fn defused_claim_buffer<'a>(position: i64) -> ClaimBuffer<'a> {
            let mut buf = ClaimBuffer::from_aeron(AeronBufferClaim::default(), position);
            buf.finalised = true;
            buf
        }

        #[test]
        fn given_defused_claim_buffer_when_position_called_then_returns_constructor_position() {
            let buf = defused_claim_buffer(4242);
            assert_eq!(buf.position(), 4242);
        }

        #[test]
        fn given_defused_claim_buffer_when_len_called_then_returns_zero_for_default_claim() {
            let buf = defused_claim_buffer(0);
            assert_eq!(buf.len(), 0);
            assert!(buf.is_empty());
        }

        #[test]
        fn given_defused_claim_buffer_when_finalised_set_then_drop_does_not_call_ffi() {
            // The mere fact that this test runs to completion without
            // segfaulting on the null-inner-ptr `AeronBufferClaim::default()`
            // is the assertion: Drop must skip its FFI branch when
            // `finalised == true`.
            let buf = defused_claim_buffer(7);
            drop(buf);
        }
    }
}
