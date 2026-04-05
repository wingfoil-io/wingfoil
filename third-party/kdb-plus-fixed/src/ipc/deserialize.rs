//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Load Libraries
//++++++++++++++++++++++++++++++++++++++++++++++++++//

use super::*;
use async_recursion::async_recursion;
use std::convert::TryInto;

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Macros
//++++++++++++++++++++++++++++++++++++++++++++++++++//

/// Read given bytes witha  gievn cursor and build a basic type element of the specified type.
macro_rules! build_element {
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, i16) => {{
        let element = match $encode {
            0 => i16::from_be_bytes($bytes[$cursor..$cursor + 2].try_into().unwrap()),
            _ => i16::from_le_bytes($bytes[$cursor..$cursor + 2].try_into().unwrap()),
        };
        (
            K::new($qtype, qattribute::NONE, k0_inner::short(element)),
            $cursor + 2,
        )
    }};
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, i32) => {{
        let element = match $encode {
            0 => i32::from_be_bytes($bytes[$cursor..$cursor + 4].try_into().unwrap()),
            _ => i32::from_le_bytes($bytes[$cursor..$cursor + 4].try_into().unwrap()),
        };
        (
            K::new($qtype, qattribute::NONE, k0_inner::int(element)),
            $cursor + 4,
        )
    }};
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, i64) => {{
        let element = match $encode {
            0 => i64::from_be_bytes($bytes[$cursor..$cursor + 8].try_into().unwrap()),
            _ => i64::from_le_bytes($bytes[$cursor..$cursor + 8].try_into().unwrap()),
        };
        (
            K::new($qtype, qattribute::NONE, k0_inner::long(element)),
            $cursor + 8,
        )
    }};
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, f32) => {{
        let element = match $encode {
            0 => f32::from_be_bytes($bytes[$cursor..$cursor + 4].try_into().unwrap()),
            _ => f32::from_le_bytes($bytes[$cursor..$cursor + 4].try_into().unwrap()),
        };
        (
            K::new($qtype, qattribute::NONE, k0_inner::real(element)),
            $cursor + 4,
        )
    }};
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, f64) => {{
        let element = match $encode {
            0 => f64::from_be_bytes($bytes[$cursor..$cursor + 8].try_into().unwrap()),
            _ => f64::from_le_bytes($bytes[$cursor..$cursor + 8].try_into().unwrap()),
        };
        (
            K::new($qtype, qattribute::NONE, k0_inner::float(element)),
            $cursor + 8,
        )
    }};
}

/// Read given bytes with a given cursor and build a basic type list of the specified type.
macro_rules! build_list {
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, i16) => {
        async {
            let (attribute, size, cursor) = get_attribute_and_size($bytes, $cursor, $encode);
            let list = match $encode {
                0 => $bytes[cursor..cursor + 2 * size]
                    .chunks(2)
                    .map(|element| i16::from_be_bytes(element.try_into().unwrap()))
                    .collect::<Vec<H>>(),
                _ => $bytes[cursor..cursor + 2 * size]
                    .chunks(2)
                    .map(|element| i16::from_le_bytes(element.try_into().unwrap()))
                    .collect::<Vec<H>>(),
            };
            let k = K::new($qtype, attribute, k0_inner::list(k0_list::new(list)));
            (k, cursor + 2 * size)
        }
    };
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, i32) => {
        async {
            let (attribute, size, cursor) = get_attribute_and_size($bytes, $cursor, $encode);
            let list = match $encode {
                0 => $bytes[cursor..cursor + 4 * size]
                    .chunks(4)
                    .map(|element| i32::from_be_bytes(element.try_into().unwrap()))
                    .collect::<Vec<I>>(),
                _ => $bytes[cursor..cursor + 4 * size]
                    .chunks(4)
                    .map(|element| i32::from_le_bytes(element.try_into().unwrap()))
                    .collect::<Vec<I>>(),
            };
            let k = K::new($qtype, attribute, k0_inner::list(k0_list::new(list)));
            (k, cursor + 4 * size)
        }
    };
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, i64) => {
        async {
            let (attribute, size, cursor) = get_attribute_and_size($bytes, $cursor, $encode);
            let list = match $encode {
                0 => $bytes[cursor..cursor + 8 * size]
                    .chunks(8)
                    .map(|element| i64::from_be_bytes(element.try_into().unwrap()))
                    .collect::<Vec<J>>(),
                _ => $bytes[cursor..cursor + 8 * size]
                    .chunks(8)
                    .map(|element| i64::from_le_bytes(element.try_into().unwrap()))
                    .collect::<Vec<J>>(),
            };
            let k = K::new($qtype, attribute, k0_inner::list(k0_list::new(list)));
            (k, cursor + 8 * size)
        }
    };
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, f32) => {
        async {
            let (attribute, size, cursor) = get_attribute_and_size($bytes, $cursor, $encode);
            let list = match $encode {
                0 => $bytes[cursor..cursor + 4 * size]
                    .chunks(4)
                    .map(|element| f32::from_be_bytes(element.try_into().unwrap()))
                    .collect::<Vec<E>>(),
                _ => $bytes[cursor..cursor + 4 * size]
                    .chunks(4)
                    .map(|element| f32::from_le_bytes(element.try_into().unwrap()))
                    .collect::<Vec<E>>(),
            };
            let k = K::new($qtype, attribute, k0_inner::list(k0_list::new(list)));
            (k, cursor + 4 * size)
        }
    };
    ($bytes:expr, $cursor:expr, $encode:expr, $qtype:expr, f64) => {
        async {
            let (attribute, size, cursor) = get_attribute_and_size($bytes, $cursor, $encode);
            let list = match $encode {
                0 => $bytes[cursor..cursor + 8 * size]
                    .chunks(8)
                    .map(|element| f64::from_be_bytes(element.try_into().unwrap()))
                    .collect::<Vec<F>>(),
                _ => $bytes[cursor..cursor + 8 * size]
                    .chunks(8)
                    .map(|element| f64::from_le_bytes(element.try_into().unwrap()))
                    .collect::<Vec<F>>(),
            };
            let k = K::new($qtype, attribute, k0_inner::list(k0_list::new(list)));
            (k, cursor + 8 * size)
        }
    };
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Implementation
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% K %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl K {
    /// Deserialize bytes to q object in a manner of q function `-9!`.
    pub(crate) async fn q_ipc_decode(bytes: &[u8], encode: u8) -> Self {
        deserialize_bytes(bytes, 0, encode).await.0
    }
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Private Functions
//++++++++++++++++++++++++++++++++++++++++++++++++++//

#[async_recursion]
async fn deserialize_bytes(bytes: &[u8], cursor: usize, encode: u8) -> (K, usize) {
    match bytes[cursor] as i8 {
        qtype::BOOL_ATOM => deserialize_bool(bytes, cursor + 1, encode),
        qtype::GUID_ATOM => deserialize_guid(bytes, cursor + 1, encode),
        qtype::BYTE_ATOM => deserialize_byte(bytes, cursor + 1, encode),
        qtype::SHORT_ATOM => build_element!(bytes, cursor + 1, encode, qtype::SHORT_ATOM, i16),
        qtype::INT_ATOM => build_element!(bytes, cursor + 1, encode, qtype::INT_ATOM, i32),
        qtype::LONG_ATOM => build_element!(bytes, cursor + 1, encode, qtype::LONG_ATOM, i64),
        qtype::REAL_ATOM => build_element!(bytes, cursor + 1, encode, qtype::REAL_ATOM, f32),
        qtype::FLOAT_ATOM => build_element!(bytes, cursor + 1, encode, qtype::FLOAT_ATOM, f64),
        qtype::CHAR => deserialize_char(bytes, cursor + 1, encode),
        qtype::SYMBOL_ATOM => deserialize_symbol(bytes, cursor + 1, encode),
        qtype::TIMESTAMP_ATOM => {
            build_element!(bytes, cursor + 1, encode, qtype::TIMESTAMP_ATOM, i64)
        }
        qtype::MONTH_ATOM => build_element!(bytes, cursor + 1, encode, qtype::MONTH_ATOM, i32),
        qtype::DATE_ATOM => build_element!(bytes, cursor + 1, encode, qtype::DATE_ATOM, i32),
        qtype::DATETIME_ATOM => {
            build_element!(bytes, cursor + 1, encode, qtype::DATETIME_ATOM, f64)
        }
        qtype::TIMESPAN_ATOM => {
            build_element!(bytes, cursor + 1, encode, qtype::TIMESPAN_ATOM, i64)
        }
        qtype::MINUTE_ATOM => build_element!(bytes, cursor + 1, encode, qtype::MINUTE_ATOM, i32),
        qtype::SECOND_ATOM => build_element!(bytes, cursor + 1, encode, qtype::SECOND_ATOM, i32),
        qtype::TIME_ATOM => build_element!(bytes, cursor + 1, encode, qtype::TIME_ATOM, i32),
        qtype::COMPOUND_LIST => deserialize_compound_list(bytes, cursor + 1, encode).await,
        qtype::BOOL_LIST => deserialize_bool_list(bytes, cursor + 1, encode),
        qtype::GUID_LIST => deserialize_guid_list(bytes, cursor + 1, encode).await,
        qtype::BYTE_LIST => deserialize_byte_list(bytes, cursor + 1, encode),
        qtype::SHORT_LIST => build_list!(bytes, cursor + 1, encode, qtype::SHORT_LIST, i16).await,
        qtype::INT_LIST => build_list!(bytes, cursor + 1, encode, qtype::INT_LIST, i32).await,
        qtype::LONG_LIST => build_list!(bytes, cursor + 1, encode, qtype::LONG_LIST, i64).await,
        qtype::REAL_LIST => build_list!(bytes, cursor + 1, encode, qtype::REAL_LIST, f32).await,
        qtype::FLOAT_LIST => build_list!(bytes, cursor + 1, encode, qtype::FLOAT_LIST, f64).await,
        qtype::STRING => deserialize_string(bytes, cursor + 1, encode),
        qtype::SYMBOL_LIST => deserialize_symbol_list(bytes, cursor + 1, encode).await,
        qtype::TIMESTAMP_LIST => {
            build_list!(bytes, cursor + 1, encode, qtype::TIMESTAMP_LIST, i64).await
        }
        qtype::MONTH_LIST => build_list!(bytes, cursor + 1, encode, qtype::MONTH_LIST, i32).await,
        qtype::DATE_LIST => build_list!(bytes, cursor + 1, encode, qtype::DATE_LIST, i32).await,
        qtype::DATETIME_LIST => {
            build_list!(bytes, cursor + 1, encode, qtype::DATETIME_LIST, f64).await
        }
        qtype::TIMESPAN_LIST => {
            build_list!(bytes, cursor + 1, encode, qtype::TIMESPAN_LIST, i64).await
        }
        qtype::MINUTE_LIST => build_list!(bytes, cursor + 1, encode, qtype::MINUTE_LIST, i32).await,
        qtype::SECOND_LIST => build_list!(bytes, cursor + 1, encode, qtype::SECOND_LIST, i32).await,
        qtype::TIME_LIST => build_list!(bytes, cursor + 1, encode, qtype::TIME_LIST, i32).await,
        qtype::TABLE => deserialize_table(bytes, cursor + 1, encode).await,
        qtype::DICTIONARY | qtype::SORTED_DICTIONARY => {
            deserialize_dictionary(bytes, cursor + 1, encode).await
        }
        qtype::NULL => deserialize_null(bytes, cursor + 1, encode),
        qtype::ERROR => deserialize_error(bytes, cursor + 1, encode),
        _ => unreachable!(),
    }
}

fn deserialize_bool(bytes: &[u8], cursor: usize, _: u8) -> (K, usize) {
    (K::new_bool(bytes[cursor] != 0), cursor + 1)
}

fn deserialize_guid(bytes: &[u8], cursor: usize, _: u8) -> (K, usize) {
    (
        K::new_guid(bytes[cursor..cursor + 16].try_into().unwrap()),
        cursor + 16,
    )
}

fn deserialize_byte(bytes: &[u8], cursor: usize, _: u8) -> (K, usize) {
    (K::new_byte(bytes[cursor]), cursor + 1)
}

fn deserialize_char(bytes: &[u8], cursor: usize, _: u8) -> (K, usize) {
    (
        K::new(qtype::CHAR, qattribute::NONE, k0_inner::byte(bytes[cursor])),
        cursor + 1,
    )
}

fn deserialize_symbol(bytes: &[u8], cursor: usize, _: u8) -> (K, usize) {
    let null_location = bytes
        .split_at(cursor)
        .1
        .iter()
        .position(|b| *b == 0x00)
        .unwrap();
    let k =
        K::new_symbol(String::from_utf8(bytes[cursor..cursor + null_location].to_vec()).unwrap());
    (k, cursor + null_location + 1)
}

/// Extract attribute and list length and then proceed the cursor.
fn get_attribute_and_size(bytes: &[u8], cursor: usize, encode: u8) -> (i8, usize, usize) {
    let size = match encode {
        0 => u32::from_be_bytes(bytes[cursor + 1..cursor + 5].try_into().unwrap()),
        _ => u32::from_le_bytes(bytes[cursor + 1..cursor + 5].try_into().unwrap()),
    };
    (bytes[cursor] as i8, size as usize, cursor + 5)
}

fn deserialize_bool_list(bytes: &[u8], cursor: usize, encode: u8) -> (K, usize) {
    let (attribute, size, cursor) = get_attribute_and_size(bytes, cursor, encode);
    let list = bytes[cursor..cursor + size].to_vec();
    (
        K::new(
            qtype::BOOL_LIST,
            attribute,
            k0_inner::list(k0_list::new(list)),
        ),
        cursor + size,
    )
}

async fn deserialize_guid_list(bytes: &[u8], cursor: usize, encode: u8) -> (K, usize) {
    let (attribute, size, cursor) = get_attribute_and_size(bytes, cursor, encode);
    let list = bytes[cursor..cursor + 16 * size]
        .chunks(16)
        .map(|guid| guid.try_into().unwrap())
        .collect::<Vec<U>>();
    (K::new_guid_list(list, attribute), cursor + 16 * size)
}

fn deserialize_byte_list(bytes: &[u8], cursor: usize, encode: u8) -> (K, usize) {
    let (attribute, size, cursor) = get_attribute_and_size(bytes, cursor, encode);
    let list = bytes[cursor..cursor + size].to_vec();
    (K::new_byte_list(list, attribute), cursor + size)
}

fn deserialize_string(bytes: &[u8], cursor: usize, encode: u8) -> (K, usize) {
    let (attribute, size, cursor) = get_attribute_and_size(bytes, cursor, encode);
    (
        K::new_string(
            String::from_utf8(bytes[cursor..cursor + size].to_vec()).unwrap(),
            attribute,
        ),
        cursor + size,
    )
}

async fn deserialize_symbol_list(bytes: &[u8], cursor: usize, encode: u8) -> (K, usize) {
    let (attribute, size, mut cursor) = get_attribute_and_size(bytes, cursor, encode);
    let mut list = Vec::<String>::new();
    for _ in 0..size {
        let null_location = bytes
            .split_at(cursor)
            .1
            .iter()
            .position(|b| *b == 0x00)
            .unwrap();
        list.push(String::from_utf8(bytes[cursor..cursor + null_location].to_vec()).unwrap());
        cursor += null_location + 1;
    }
    (K::new_symbol_list(list, attribute), cursor)
}

async fn deserialize_compound_list(bytes: &[u8], cursor: usize, encode: u8) -> (K, usize) {
    let (_, size, cursor) = get_attribute_and_size(bytes, cursor, encode);
    let mut list = Vec::<K>::new();
    let mut cursor_ = cursor;
    for _ in 0..size {
        let (element, cursor) = deserialize_bytes(bytes, cursor_, encode).await;
        list.push(element);
        cursor_ = cursor;
    }
    (K::new_compound_list(list), cursor_)
}

async fn deserialize_table(bytes: &[u8], cursor: usize, encode: u8) -> (K, usize) {
    // Skip table attribute 0x00, dictionary indicator 99 and symbol list indicator 11
    let (headers, cursor) = deserialize_symbol_list(bytes, cursor + 3, encode).await;
    // Skip compound list indicator 0
    let (columns, cursor) = deserialize_compound_list(bytes, cursor + 1, encode).await;
    // Trust kdb+. Should not fail.
    let dictionary = K::new_dictionary(headers, columns).expect("failed to build a dictionary");
    (
        K::new(qtype::TABLE, qattribute::NONE, k0_inner::table(dictionary)),
        cursor,
    )
}

async fn deserialize_dictionary(bytes: &[u8], cursor: usize, encode: u8) -> (K, usize) {
    let (keys, cursor) = deserialize_bytes(bytes, cursor, encode).await;
    let (values, cursor) = deserialize_bytes(bytes, cursor, encode).await;
    (
        K::new_dictionary(keys, values).expect("failed to build a dictionary"),
        cursor,
    )
}

fn deserialize_null(_: &[u8], cursor: usize, _: u8) -> (K, usize) {
    (K::new_null(), cursor + 1)
}

fn deserialize_error(bytes: &[u8], cursor: usize, _: u8) -> (K, usize) {
    let null_location = bytes
        .split_at(cursor)
        .1
        .iter()
        .position(|b| *b == 0x00_u8)
        .unwrap();
    let error = String::from_utf8(bytes[cursor..cursor + null_location].to_vec()).unwrap();
    (K::new_error(error), cursor + null_location + 1)
}
