//! The venue's inline ids: [`ExecId`] for an execution and [`VenueId`] for
//! an order, each held inline and without allocating.
//!
//! They are two types over one representation — bounded venue text, parsed
//! once and carried verbatim — because they are two different things: a
//! fill's execution id is what reconciliation matches a trade on, and the
//! venue's order id is what an amend or a cancel addresses. One type for
//! both would let either stand in for the other.
//!
//! # Why not a `String`
//!
//! Not the allocation. Fills arrive at single-digit rates per second against
//! a market-data feed doing a hundred thousand, so one `malloc` per fill is
//! free and would be a silly thing to optimise.
//!
//! It is that a `Fill` rides a graph edge as a plain
//! value. Everything else in the execution vocabulary — an `Order`, a spec, a
//! `Px` — is `Copy`, so a burst of fills is copied, queued and matched without
//! touching the allocator or an atomic, and a `Burst<Fill>` is a `TinyVec`
//! holding them by value. One `String` field is enough to take that away from
//! the whole type. (It is *not* what the position fold keys on: that keys on
//! the instrument.)
//!
//! # Why not `[u8; N]`
//!
//! Because a venue's id is not fixed length, and an exec id is the field
//! reconciliation matches on.
//!
//! A venue's ids are venue-assigned text with no documented bound, and the
//! shape can change in the venue's life: one venue's went from a prefixed
//! form (`XYZ-2696083`) to a bare decimal counter (`48079254`) in a single
//! release. A counter grows a digit every time it crosses a power of ten,
//! and nothing in an API contract pins a width.
//!
//! So this is a *bounded* id, not a fixed one: the capacity is generous, and
//! the only way to build one is a parse that **refuses** an id that does not
//! fit. Truncating instead would be the worst available outcome — a fill that
//! looks fine on the graph, reconciles against nothing, and is found at
//! settlement. An error at the decode boundary is loud, is attributable to
//! one venue message, and is fixed by raising the capacity both share
//! ([`ExecId::CAPACITY`], [`VenueId::CAPACITY`]).

use std::fmt;

/// Why a venue's id was refused.
///
/// Both variants are a decode-boundary error on one venue message, not a
/// reason to stop: the message is dropped and reported, and the id it named is
/// never approximated. One enum for [`ExecId`] and [`VenueId`], because what
/// is refused is the text, whichever id it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdError {
    /// The venue sent an empty id.
    ///
    /// Refused so that the empty id has exactly one meaning.
    /// [`Default`] is the pre-first-tick placeholder on a graph edge and
    /// [`ExecId::is_empty`] is how a fold tells that placeholder from an
    /// execution; an id off the wire that parsed to the same value would make
    /// "no execution" and "an execution the venue named with nothing"
    /// indistinguishable.
    Empty,
    /// The id is longer than an [`ExecId`] or a [`VenueId`] holds.
    ///
    /// Refused rather than truncated: a shortened id reconciles against
    /// nothing and surfaces at settlement. The fix is to raise the capacity
    /// ([`ExecId::CAPACITY`]), which is why the error names both lengths.
    TooLong {
        /// What the venue sent.
        len: usize,
        /// What the id holds.
        capacity: usize,
    },
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a venue id is empty"),
            Self::TooLong { len, capacity } => write!(
                f,
                "a venue id of {len} bytes is longer than the {capacity} an id holds"
            ),
        }
    }
}

impl std::error::Error for IdError {}

/// The representation both ids share: venue text, stored inline.
///
/// Equality is over the text: the unused tail is always zeroed, so two ids
/// built from the same string compare equal whatever was built before them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Inline {
    /// The id's bytes, zero-padded past `len`. Valid UTF-8 for `..len`,
    /// because [`Inline::new`] is the only way to fill it.
    bytes: [u8; Inline::CAPACITY],
    /// How much of `bytes` is the id.
    len: u8,
}

impl Inline {
    /// The longest id this holds, in bytes.
    ///
    /// Sized for the longest shape in common use rather than for any one
    /// venue's own: a UUID is 36 characters, and this crate is written for
    /// more than one venue. 47 keeps an id at 48 bytes, a multiple of `Px`'s
    /// 16-byte alignment, so it costs no padding where it sits.
    const CAPACITY: usize = 47;

    /// The empty id: what [`Default`] is, and nothing a parse produces.
    const EMPTY: Inline = Inline {
        bytes: [0u8; Inline::CAPACITY],
        len: 0,
    };

    fn new(id: &str) -> Result<Inline, IdError> {
        if id.is_empty() {
            return Err(IdError::Empty);
        }
        if id.len() > Inline::CAPACITY {
            return Err(IdError::TooLong {
                len: id.len(),
                capacity: Inline::CAPACITY,
            });
        }
        let mut bytes = [0u8; Inline::CAPACITY];
        bytes[..id.len()].copy_from_slice(id.as_bytes());
        Ok(Inline {
            bytes,
            // `CAPACITY` is far inside a `u8`, and the length is at most that.
            len: id.len() as u8,
        })
    }

    fn as_str(&self) -> &str {
        // Only `new` writes `bytes`, and it writes one `&str` into the front
        // of it, so the head is the UTF-8 it came from.
        std::str::from_utf8(&self.bytes[..self.len as usize])
            .expect("an inline id holds the bytes of a &str")
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Stamps one public id over [`Inline`]: the same capacity, the same parse,
/// the empty default, `Display`, and a `Debug` that names the type.
macro_rules! inline_id {
    ($(#[$doc:meta])* $name:ident, $default_doc:literal) => {
        $(#[$doc])*
        ///
        /// `Copy`, `Hash` and `Ord`, so it is usable as a map key and as a
        /// value on a graph edge. Equality is over the id's text.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Inline);

        impl $name {
            /// The longest id this holds, in bytes.
            ///
            /// Sized for the longest shape in common use rather than for
            /// any one venue's own: a UUID is 36 characters, and this crate
            /// is written for more than one venue. 47 keeps the id at 48
            /// bytes, a multiple of `Px`'s 16-byte alignment, so it costs no
            /// padding where it sits.
            pub const CAPACITY: usize = Inline::CAPACITY;

            /// Read a venue's id.
            ///
            /// # Errors
            ///
            /// [`IdError::Empty`] or [`IdError::TooLong`] — an id is never
            /// truncated, and the empty id belongs to [`Default`] alone.
            pub fn new(id: &str) -> Result<$name, IdError> {
                Inline::new(id).map($name)
            }

            /// The id as the venue sent it.
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }

            /// Whether this is the empty id — the [`Default`], which is not
            /// an id any venue sent.
            pub const fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }

        #[doc = $default_doc]
        impl Default for $name {
            fn default() -> Self {
                $name(Inline::EMPTY)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        /// Shows the id, not 47 bytes of mostly zero — this ends up in every
        /// log line and test failure about it.
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({:?})"), self.as_str())
            }
        }
    };
}

inline_id!(
    /// A venue-assigned execution id, stored inline: what a [`Fill`] names
    /// its trade by, and what reconciliation matches on.
    ///
    /// [`Fill`]: crate::adapters::execution::order::Fill
    ExecId,
    "The empty id. Present because a value on a graph edge must have one (a \
     `Burst<T>` is a `TinyVec<[T; 1]>`), not because an empty execution id \
     means anything: it is what a `Fill` on a `Burst` starts life as."
);

inline_id!(
    /// The venue's own id for an order, stored inline: what an [`Ack`]
    /// carries, and what an amend or a cancel addresses at the venue.
    ///
    /// Not an [`ExecId`], though the text is the same shape: an order id
    /// names an order, an execution id names one trade against it, and one
    /// type for both would let either stand in for the other.
    ///
    /// [`Ack`]: crate::adapters::execution::edge::Ack
    VenueId,
    "The empty id: an ack from a venue that names none (a simulator, whose \
     client id *is* the order), and the placeholder a value on a graph edge \
     must have."
);

#[cfg(test)]
mod tests {
    use super::*;

    /// The shapes venues actually send: a bare counter, a prefixed id, a UUID.
    #[test]
    fn the_ids_a_venue_actually_sends_round_trip() {
        for id in [
            "48079254",                             // a bare counter
            "XYZ-2696083",                          // a prefixed counter
            "550e8400-e29b-41d4-a716-446655440000", // a UUID, 36 bytes
            "1",
        ] {
            let parsed = ExecId::new(id).unwrap_or_else(|e| panic!("{id} should fit: {e}"));
            assert_eq!(parsed.as_str(), id);
            assert_eq!(parsed.to_string(), id);
        }
    }

    /// The whole reason this is not `[u8; N]`. A truncated id is a fill that
    /// reconciles against nothing, so the boundary refuses it instead — and
    /// says both lengths, or raising `CAPACITY` is guesswork.
    #[test]
    fn an_id_that_does_not_fit_is_refused_not_truncated() {
        let long = "x".repeat(ExecId::CAPACITY + 1);
        assert_eq!(
            ExecId::new(&long),
            Err(IdError::TooLong {
                len: ExecId::CAPACITY + 1,
                capacity: ExecId::CAPACITY,
            })
        );

        // Exactly at capacity still fits — the bound is inclusive.
        assert!(ExecId::new(&"x".repeat(ExecId::CAPACITY)).is_ok());
    }

    /// The padding must not leak into equality: an id built after a longer one
    /// is the same value as the same id built after a shorter one.
    #[test]
    fn equality_is_over_the_text_not_the_padding() {
        let short = ExecId::new("48079254").unwrap();
        let same = ExecId::new("48079254").unwrap();
        assert_eq!(short, same);
        assert_eq!(short.as_str(), "48079254");

        // A prefix is not the same id as what it prefixes.
        assert_ne!(short, ExecId::new("4807925").unwrap());
        assert_ne!(ExecId::default(), short);

        use std::collections::HashSet;
        let seen: HashSet<ExecId> = [short, same, ExecId::new("48079255").unwrap()]
            .into_iter()
            .collect();
        assert_eq!(seen.len(), 2);
    }

    /// The point of the type. A `Fill` that allocates is a `Fill` that cannot
    /// ride a `Burst` by value.
    #[test]
    fn it_is_a_plain_value_of_a_known_size() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<ExecId>();
        assert_eq!(size_of::<ExecId>(), 48);
    }

    /// Present so a `Fill` can ride a `Burst`, and distinguishable from a real
    /// id so it cannot be mistaken for one.
    #[test]
    fn the_default_is_empty() {
        let default = ExecId::default();
        assert!(default.is_empty());
        assert_eq!(default.as_str(), "");
        assert!(!ExecId::new("48079254").unwrap().is_empty());
    }

    /// `Default` is the only way to an empty id. If a parse could produce one
    /// too, `is_empty()` would no longer mean "no execution" — a fill the
    /// venue identified with nothing would read as the placeholder.
    #[test]
    fn an_empty_id_is_refused_so_the_default_stays_the_only_empty_one() {
        assert_eq!(ExecId::new(""), Err(IdError::Empty));
        // And the Default is still reachable, and still empty.
        assert!(ExecId::default().is_empty());
    }

    #[test]
    fn debug_shows_the_id() {
        assert_eq!(
            format!("{:?}", ExecId::new("48079254").unwrap()),
            r#"ExecId("48079254")"#
        );
    }

    /// The order id is its own type over the same text: it parses, refuses
    /// and shows the same way, and names itself.
    #[test]
    fn a_venue_order_id_is_the_same_text_under_its_own_name() {
        let id = VenueId::new("BTC-31415926").unwrap();
        assert_eq!(id.as_str(), "BTC-31415926");
        assert_eq!(id.to_string(), "BTC-31415926");
        assert_eq!(format!("{id:?}"), r#"VenueId("BTC-31415926")"#);
        assert_eq!(VenueId::new(""), Err(IdError::Empty));
        assert_eq!(
            VenueId::new(&"x".repeat(VenueId::CAPACITY + 1)),
            Err(IdError::TooLong {
                len: VenueId::CAPACITY + 1,
                capacity: VenueId::CAPACITY,
            })
        );
        assert!(VenueId::default().is_empty());
        assert_eq!(size_of::<VenueId>(), 48);
    }
}
