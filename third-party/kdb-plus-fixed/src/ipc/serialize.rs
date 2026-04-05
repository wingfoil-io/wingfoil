//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Load Libraries
//++++++++++++++++++++++++++++++++++++++++++++++++++//

use super::*;

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Global Variable
//++++++++++++++++++++++++++++++++++++++++++++++++++//

// %% System encoding %%//vvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Endian of OS used to serialize `K` object.
/// - 0: Big Endian
/// - 1: Little Endian
#[cfg(target_endian = "big")]
pub const ENCODING: u8 = 0;

/// Endian of OS used to serialize `K` object.
/// - 0: Big Endian
/// - 1: Little Endian
#[cfg(target_endian = "little")]
pub const ENCODING: u8 = 1;

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Implementation
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% K %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl K {
    /// Serialize q object to bytes in a manner of q function `-8!` without the IPC message
    ///  header (encoding, message type, compressed, reserved null byte and total message length).
    pub(crate) fn q_ipc_encode(&self) -> Vec<u8> {
        let mut stream = Vec::new();
        serialize_q(self, &mut stream);
        stream
    }
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Private Functions
//++++++++++++++++++++++++++++++++++++++++++++++++++//

fn serialize_q(obj: &K, stream: &mut Vec<u8>) {
    match obj.0.qtype {
        qtype::BOOL_ATOM | qtype::BYTE_ATOM | qtype::CHAR => serialize_byte(obj, stream),
        qtype::GUID_ATOM => serialize_guid(obj, stream),
        qtype::SHORT_ATOM => serialize_short(obj, stream),
        qtype::INT_ATOM
        | qtype::MONTH_ATOM
        | qtype::DATE_ATOM
        | qtype::MINUTE_ATOM
        | qtype::SECOND_ATOM
        | qtype::TIME_ATOM => serialize_int(obj, stream),
        qtype::LONG_ATOM | qtype::TIMESTAMP_ATOM | qtype::TIMESPAN_ATOM => {
            serialize_long(obj, stream)
        }
        qtype::REAL_ATOM => serialize_real(obj, stream),
        qtype::FLOAT_ATOM | qtype::DATETIME_ATOM => serialize_float(obj, stream),
        qtype::SYMBOL_ATOM => serialize_symbol(obj, stream),
        qtype::COMPOUND_LIST => serialize_compound_list(obj, stream),
        qtype::BOOL_LIST | qtype::BYTE_LIST => serialize_byte_list(obj, stream),
        qtype::GUID_LIST => serialize_guid_list(obj, stream),
        qtype::SHORT_LIST => serialize_short_list(obj, stream),
        qtype::INT_LIST
        | qtype::MONTH_LIST
        | qtype::DATE_LIST
        | qtype::MINUTE_LIST
        | qtype::SECOND_LIST
        | qtype::TIME_LIST => serialize_int_list(obj, stream),
        qtype::LONG_LIST | qtype::TIMESTAMP_LIST | qtype::TIMESPAN_LIST => {
            serialize_long_list(obj, stream)
        }
        qtype::REAL_LIST => serialize_real_list(obj, stream),
        qtype::FLOAT_LIST | qtype::DATETIME_LIST => serialize_float_list(obj, stream),
        qtype::STRING => serialize_string(obj, stream),
        qtype::SYMBOL_LIST => serialize_symbol_list(obj, stream),
        qtype::TABLE => serialize_table(obj, stream),
        qtype::DICTIONARY | qtype::SORTED_DICTIONARY => serialize_dictionary(obj, stream),
        qtype::NULL => serialize_null(stream),
        _ => unimplemented!(),
    };
}

fn serialize_guid(guid: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0xfe);
    // Element
    stream.extend_from_slice(&guid.get_guid().unwrap());
}

fn serialize_byte(byte: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(byte.0.qtype as u8);
    // Element
    stream.push(byte.get_byte().unwrap());
}

fn serialize_short(short: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0xfb);
    // Element
    stream.extend_from_slice(&match ENCODING {
        0 => short.get_short().unwrap().to_be_bytes(),
        _ => short.get_short().unwrap().to_le_bytes(),
    });
}

fn serialize_int(int: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(int.0.qtype as u8);
    // Element
    stream.extend_from_slice(&match ENCODING {
        0 => int.get_int().unwrap().to_be_bytes(),
        _ => int.get_int().unwrap().to_le_bytes(),
    });
}

fn serialize_long(long: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(long.0.qtype as u8);
    // Element
    stream.extend_from_slice(&match ENCODING {
        0 => long.get_long().unwrap().to_be_bytes(),
        _ => long.get_long().unwrap().to_le_bytes(),
    });
}

fn serialize_real(real: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0xf8);
    // Element
    stream.extend_from_slice(&match ENCODING {
        0 => real.get_real().unwrap().to_be_bytes(),
        _ => real.get_real().unwrap().to_le_bytes(),
    });
}

fn serialize_float(float: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(float.0.qtype as u8);
    // Element
    stream.extend_from_slice(&match ENCODING {
        0 => float.get_float().unwrap().to_be_bytes(),
        _ => float.get_float().unwrap().to_le_bytes(),
    });
}

fn serialize_symbol(symbol: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0xf5);
    // Element
    stream.extend_from_slice(symbol.get_symbol().unwrap().as_bytes());
    // Null byte
    stream.push(0x00);
}

fn serialize_guid_list(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0x02);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_vec::<U>().unwrap();
    // Length of vector
    let length = match ENCODING {
        0 => (vector.len() as u32).to_be_bytes(),
        _ => (vector.len() as u32).to_le_bytes(),
    };
    stream.extend_from_slice(&length);
    vector
        .iter()
        .for_each(|element| stream.extend_from_slice(element));
}

fn serialize_byte_list(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(list.0.qtype as u8);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_vec::<G>().unwrap();
    // Length of vector
    let length = match ENCODING {
        0 => (vector.len() as u32).to_be_bytes(),
        _ => (vector.len() as u32).to_le_bytes(),
    };
    stream.extend_from_slice(&length);
    stream.extend_from_slice(vector.as_slice());
}

fn serialize_short_list(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0x05);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_vec::<H>().unwrap();
    match ENCODING {
        0 => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_be_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_be_bytes());
            });
        }
        _ => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_le_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_le_bytes());
            });
        }
    }
}

fn serialize_int_list(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(list.0.qtype as u8);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_vec::<I>().unwrap();
    match ENCODING {
        0 => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_be_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_be_bytes());
            });
        }
        _ => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_le_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_le_bytes());
            });
        }
    }
}

fn serialize_long_list(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(list.0.qtype as u8);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_vec::<J>().unwrap();
    match ENCODING {
        0 => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_be_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_be_bytes());
            });
        }
        _ => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_le_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_le_bytes());
            });
        }
    }
}

fn serialize_real_list(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0x08);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_vec::<E>().unwrap();
    match ENCODING {
        0 => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_be_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_be_bytes());
            });
        }
        _ => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_le_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_le_bytes());
            });
        }
    }
}

fn serialize_float_list(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(list.0.qtype as u8);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_vec::<F>().unwrap();
    match ENCODING {
        0 => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_be_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_be_bytes());
            });
        }
        _ => {
            // Length of vector
            stream.extend_from_slice(&(vector.len() as u32).to_le_bytes());
            // Data
            vector.iter().for_each(|element| {
                stream.extend_from_slice(&element.to_le_bytes());
            });
        }
    }
}

fn serialize_string(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0x0a);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_string().unwrap().as_bytes();
    // Length of vector
    stream.extend_from_slice(&match ENCODING {
        0 => (vector.len() as u32).to_be_bytes(),
        _ => (vector.len() as u32).to_le_bytes(),
    });
    // Data
    stream.extend_from_slice(vector);
}

fn serialize_symbol_list(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0x0b);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_vec::<S>().unwrap();
    // Length of vector
    stream.extend_from_slice(&match ENCODING {
        0 => (vector.len() as u32).to_be_bytes(),
        _ => (vector.len() as u32).to_le_bytes(),
    });
    // Data
    vector.iter().for_each(|element| {
        stream.extend_from_slice(element.as_bytes());
        stream.push(0x00);
    });
}

fn serialize_compound_list(list: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(list.0.qtype as u8);
    // Attribute
    stream.push(list.0.attribute as u8);
    // Length and data
    let vector = list.as_vec::<K>().unwrap();
    // Length and data
    stream.extend_from_slice(&match ENCODING {
        0 => (vector.len() as u32).to_be_bytes(),
        _ => (vector.len() as u32).to_le_bytes(),
    });
    // Data
    vector.iter().for_each(|element| {
        serialize_q(element, stream);
    });
}

fn serialize_table(table: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(0x62);
    stream.push(0);
    stream.push(0x63);
    // Retrieve underying dictionary
    let vector = table.get_dictionary().unwrap().as_vec::<K>().unwrap();
    // Serialize keys
    serialize_symbol_list(&vector[0], stream);
    // Serialize values
    serialize_compound_list(&vector[1], stream);
}

fn serialize_dictionary(dictionary: &K, stream: &mut Vec<u8>) {
    // Type
    stream.push(dictionary.0.qtype as u8);
    // Data
    let vector = dictionary.as_vec::<K>().unwrap();
    // Serialize keys
    serialize_q(&vector[0], stream);
    // Serialize values
    serialize_q(&vector[1], stream);
}

fn serialize_null(stream: &mut Vec<u8>) {
    // Type
    stream.push(0x65);
    // Data
    stream.push(0x00);
}
