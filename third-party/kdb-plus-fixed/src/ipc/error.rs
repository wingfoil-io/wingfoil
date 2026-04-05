//! This module provides error implementation.

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Load Libraries
//++++++++++++++++++++++++++++++++++++++++++++++++++//

use super::K;
use crate::qtype;
use std::error::Error as StdError;
use std::fmt;
use std::io::Error as IOError;

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >>  Structs
//++++++++++++++++++++++++++++++++++++++++++++++++++//

/// Error from network IO or invalid operation.
pub enum Error {
    /// Invalid time was used to build `Date` or `DateTime`.
    InvalidDateTime,
    /// Network error.
    IO(IOError),
    /// Tried to cast to wrong type.
    InvalidCast {
        from: &'static str,
        to: &'static str,
    },
    /// Tried to cast a list to wrong type.
    InvalidCastList(&'static str),
    /// Tried to access an index which is out of range.
    IndexOutOfBounds { length: usize, index: usize },
    /// Invalid operation to a wrong type.
    InvalidOperation {
        operator: &'static str,
        operand_type: &'static str,
        expected: Option<&'static str>,
    },
    /// Length of dictionary key and value do not match.
    LengthMismatch {
        key_length: usize,
        value_length: usize,
    },
    /// Tried to get non-existing column.
    NoSuchColumn(String),
    /// Tried to insert or push wrong element.
    InsertWrongElement {
        is_insert: bool,
        destination: &'static str,
        expected: &'static str,
    },
    /// Tried to pop from empty list.
    PopFromEmptyList,
    /// Tried to convert but coluld not.
    Object(K),
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Implementation
//++++++++++++++++++++++++++++++++++++++++++++++++++//

impl Error {
    /// Construct `InvalidCast` error.
    pub(crate) fn invalid_cast(from: i8, to: i8) -> Self {
        Self::InvalidCast {
            from: type_to_string(from),
            to: type_to_string(to),
        }
    }

    /// Construct `InvalidCastList` error.
    pub(crate) fn invalid_cast_list(from: i8) -> Self {
        Self::InvalidCastList(type_to_string(from))
    }

    /// Construct `IndexOutOfBounds` error.
    pub(crate) fn index_out_of_bounds(length: usize, index: usize) -> Self {
        Self::IndexOutOfBounds { length, index }
    }

    /// Construct `InvalidOperation` error.
    pub(crate) fn invalid_operation(
        operator: &'static str,
        operand_type: i8,
        expected: Option<i8>,
    ) -> Self {
        Self::InvalidOperation {
            operator,
            operand_type: type_to_string(operand_type),
            expected: expected.map(type_to_string),
        }
    }

    /// Construct `LengthMismatch` error.
    pub(crate) fn length_mismatch(key_length: usize, value_length: usize) -> Self {
        Self::LengthMismatch {
            key_length,
            value_length,
        }
    }

    /// Construct `NoSuchColumn` error.
    pub(crate) fn no_such_column(column: String) -> Self {
        Self::NoSuchColumn(column)
    }

    /// Construct `InsertWrongElement` error.
    pub(crate) fn insert_wrong_element(
        is_insert: bool,
        destination: i8,
        expected: &'static str,
    ) -> Self {
        Self::InsertWrongElement {
            is_insert,
            destination: type_to_string(destination),
            expected,
        }
    }

    /// Construct `PopFromEmptyList` error.
    pub(crate) fn pop_from_empty_list() -> Self {
        Self::PopFromEmptyList
    }

    /// Construct returned object as a result of an error.
    pub(crate) fn object(returned: K) -> Self {
        Self::Object(returned)
    }

    /// Comsume error and retrieve original object returned from some operation.
    /// `None` is returned if the error does not contain `K` object.
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let int = K::new_int(777);
    /// match int.flip(){
    ///   Ok(_) => eprintln!("miracle!!"),
    ///   Err(original_) => {
    ///     let original = original_.into_inner().unwrap();
    ///     assert_eq!(original.get_int().unwrap(), 777);
    ///   }
    /// }
    /// ```
    pub fn into_inner(self) -> Option<K> {
        match self {
            Self::Object(object) => Some(object),
            _ => None,
        }
    }
}

impl From<IOError> for Error {
    fn from(error: IOError) -> Self {
        Self::IO(error)
    }
}

impl PartialEq<Self> for Error {
    fn eq(&self, other: &Error) -> bool {
        match (self, other) {
            (Self::IO(left), Self::IO(right)) => left.to_string() == right.to_string(),
            (Self::IO(_), _) => false,
            (Self::InvalidCast { from: f, to: t }, Self::InvalidCast { from: f2, to: t2 }) => {
                f == f2 && t == t2
            }
            (Self::InvalidCastList(left), Self::InvalidCastList(right)) => left == right,
            (
                Self::IndexOutOfBounds {
                    length: l,
                    index: i,
                },
                Self::IndexOutOfBounds {
                    length: l2,
                    index: i2,
                },
            ) => l == l2 && i == i2,
            (
                Self::InvalidOperation {
                    operator: o,
                    operand_type: ot,
                    expected: e,
                },
                Self::InvalidOperation {
                    operator: o2,
                    operand_type: ot2,
                    expected: e2,
                },
            ) => o == o2 && ot == ot2 && e == e2,
            (
                Self::LengthMismatch {
                    key_length: k,
                    value_length: l,
                },
                Self::LengthMismatch {
                    key_length: k2,
                    value_length: l2,
                },
            ) => k == k2 && l == l2,
            (Self::NoSuchColumn(left), Self::NoSuchColumn(right)) => left == right,
            (
                Self::InsertWrongElement {
                    is_insert: i,
                    destination: d,
                    expected: e,
                },
                Self::InsertWrongElement {
                    is_insert: i2,
                    destination: d2,
                    expected: e2,
                },
            ) => i == i2 && d == d2 && e == e2,
            (Self::Object(left), Self::Object(right)) => {
                left.0.qtype == right.0.qtype && left.0.attribute == right.0.attribute
            }
            (Self::PopFromEmptyList, Self::PopFromEmptyList) => true,
            _ => false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidDateTime => write!(f, "invalid datetime"),
            Self::IO(error) => write!(f, "IO error: {}", error),
            Self::InvalidCast { from, to } => write!(f, "invalid cast from {} to {}", from, to),
            Self::InvalidCastList(from) => {
                write!(f, "invalid cast from {} to list of generics T", from)
            }
            Self::IndexOutOfBounds { length, index } => write!(
                f,
                "index out of bounds: specified {} but length is {}",
                index, length
            ),
            Self::InvalidOperation {
                operator,
                operand_type,
                expected,
            } => match expected {
                Some(expected_type) => write!(
                    f,
                    "invalid operation {} on {}. expected: {}",
                    operator, operand_type, expected_type
                ),
                None => write!(f, "invalid operation {} on {}", operator, operand_type),
            },
            Self::LengthMismatch {
                key_length,
                value_length,
            } => write!(
                f,
                "key-value length mismatch: {} and {}",
                key_length, value_length
            ),
            Self::NoSuchColumn(column) => write!(f, "no such column: {}", column),
            Self::InsertWrongElement {
                is_insert,
                destination,
                expected,
            } => {
                let operation = match is_insert {
                    true => "insert",
                    false => "push",
                };
                write!(
                    f,
                    "{} wrong element to {}: expected {}",
                    operation, destination, expected
                )
            }
            Self::Object(object) => write!(f, "{}", object),
            Self::PopFromEmptyList => write!(f, "pop from empty list"),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidDateTime => write!(f, "invalid datetime"),
            Self::IO(error) => write!(f, "IO error: {:?}", error),
            Self::InvalidCast { from, to } => write!(f, "invalid cast from {} to {}", from, to),
            Self::InvalidCastList(from) => {
                write!(f, "invalid cast from {} to list of generics T", from)
            }
            Self::IndexOutOfBounds { length, index } => write!(
                f,
                "index out of bounds: specified {} but length is {}",
                index, length
            ),
            Self::InvalidOperation {
                operator,
                operand_type,
                expected,
            } => match expected {
                Some(expected_type) => write!(
                    f,
                    "invalid operation {} on {}. expected: {}",
                    operator, operand_type, expected_type
                ),
                None => write!(f, "invalid operation {} on {}", operator, operand_type),
            },
            Self::LengthMismatch {
                key_length,
                value_length,
            } => write!(
                f,
                "key-value length mismatch: {} and {}",
                key_length, value_length
            ),
            Self::NoSuchColumn(column) => write!(f, "no such column: {}", column),
            Self::InsertWrongElement {
                is_insert,
                destination,
                expected,
            } => {
                let operation = match is_insert {
                    true => "insert",
                    false => "push",
                };
                write!(
                    f,
                    "{} wrong element to {}: expected {}",
                    operation, destination, expected
                )
            }
            Self::Object(object) => write!(f, "{}", object),
            Self::PopFromEmptyList => write!(f, "pop from empty list"),
        }
    }
}

impl StdError for Error {}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Private Function
//++++++++++++++++++++++++++++++++++++++++++++++++++//

/// Return a corresponding type name of a given type indicator.
fn type_to_string(qtype: i8) -> &'static str {
    match qtype {
        qtype::BOOL_ATOM => "bool",
        qtype::GUID_ATOM => "guid",
        qtype::BYTE_ATOM => "byte",
        qtype::SHORT_ATOM => "short",
        qtype::INT_ATOM => "int",
        qtype::LONG_ATOM => "long",
        qtype::REAL_ATOM => "real",
        qtype::FLOAT_ATOM => "float",
        qtype::CHAR => "char",
        qtype::SYMBOL_ATOM => "symbol",
        qtype::TIMESTAMP_ATOM => "timestamp",
        qtype::MONTH_ATOM => "month",
        qtype::DATE_ATOM => "date",
        qtype::DATETIME_ATOM => "datetime",
        qtype::TIMESPAN_ATOM => "timespan",
        qtype::MINUTE_ATOM => "minute",
        qtype::SECOND_ATOM => "second",
        qtype::TIME_ATOM => "time",
        qtype::COMPOUND_LIST => "compound list",
        qtype::BOOL_LIST => "bool list",
        qtype::GUID_LIST => "guid list",
        qtype::BYTE_LIST => "byte list",
        qtype::SHORT_LIST => "short list",
        qtype::INT_LIST => "int list",
        qtype::LONG_LIST => "long list",
        qtype::REAL_LIST => "real list",
        qtype::FLOAT_LIST => "float list",
        qtype::STRING => "string",
        qtype::SYMBOL_LIST => "symbol list",
        qtype::TIMESTAMP_LIST => "timestamp list",
        qtype::MONTH_LIST => "month list",
        qtype::DATE_LIST => "date list",
        qtype::DATETIME_LIST => "datetime list",
        qtype::TIMESPAN_LIST => "timespan list",
        qtype::MINUTE_LIST => "minute list",
        qtype::SECOND_LIST => "second list",
        qtype::TIME_LIST => "time list",
        qtype::TABLE => "table",
        qtype::DICTIONARY => "dictionary",
        qtype::NULL => "null",
        qtype::SORTED_DICTIONARY => "sorted dictionary",
        qtype::ERROR => "error",
        _ => "not supported",
    }
}
