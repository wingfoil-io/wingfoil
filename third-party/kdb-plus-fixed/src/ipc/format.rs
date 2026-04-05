//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Load Libraries
//++++++++++++++++++++++++++++++++++++++++++++++++++//

use super::*;
use std::fmt;

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Implementation
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% Display %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl fmt::Display for K {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.0.qtype {
            qtype::ERROR => write!(f, "'{}", self.get_error_string().unwrap()),
            _ => {
                let mut stream = String::new();
                if let Some(precision) = f.precision() {
                    put_q(self, &mut stream, precision);
                } else {
                    put_q(self, &mut stream, 0);
                }
                write!(f, "{}", stream)
            }
        }
    }
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Private Functions
//++++++++++++++++++++++++++++++++++++++++++++++++++//

fn put_bool(boolean: G, stream: &mut String) {
    stream.push(match boolean {
        0 => '0',
        _ => '1',
    });
}

fn put_guid(guid: U, stream: &mut String) {
    let strguid = guid
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
    stream.push_str(
        format!(
            "{}-{}-{}-{}-{}",
            &strguid[0..8],
            &strguid[8..12],
            &strguid[12..16],
            &strguid[16..20],
            &strguid[20..32]
        )
        .as_str(),
    );
}

fn put_byte(byte: G, stream: &mut String) {
    stream.push_str(format!("{:02x}", byte).as_str());
}

fn put_short(short: H, stream: &mut String) {
    if short == qnull_base::H {
        stream.push_str("0N")
    } else if short == qinf_base::H {
        stream.push_str("0W")
    } else if short == qninf_base::H {
        stream.push_str("-0W")
    } else {
        stream.push_str(format!("{}", short).as_str())
    }
}

fn put_int(int: I, stream: &mut String) {
    if int == qnull_base::I {
        stream.push_str("0N")
    } else if int == qinf_base::I {
        stream.push_str("0W")
    } else if int == qninf_base::I {
        stream.push_str("-0W")
    } else {
        stream.push_str(format!("{}", int).as_str())
    }
}

fn put_long(long: J, stream: &mut String) {
    if long == qnull_base::J {
        stream.push_str("0N")
    } else if long == qinf_base::J {
        stream.push_str("0W")
    } else if long == qninf_base::J {
        stream.push_str("-0W")
    } else {
        stream.push_str(format!("{}", long).as_str())
    }
}

fn put_real(real: E, stream: &mut String, precision: usize) {
    if real.is_nan() {
        stream.push_str("0N")
    } else if real == qinf_base::E {
        stream.push_str("0W")
    } else if real == qninf_base::E {
        stream.push_str("-0W")
    } else if precision != 0 {
        stream.push_str(format!("{1:.*}", precision, real).as_str())
    } else {
        stream.push_str(format!("{}", real).as_str())
    }
}

fn put_float(float: F, stream: &mut String, precision: usize) {
    if float.is_nan() {
        stream.push_str("0n")
    } else if float.is_infinite() && float.is_sign_negative() {
        stream.push_str("-0w")
    } else if float.is_infinite() {
        stream.push_str("0w")
    } else if precision != 0 {
        stream.push_str(format!("{1:.*}", precision, float).as_str())
    } else {
        stream.push_str(format!("{}", float).as_str())
    }
}

fn put_symbol(symbol: &str, stream: &mut String) {
    stream.push('`');
    stream.push_str(symbol);
}

/// Put formatted timestamp value to a stream and return if 'p' suffix is necessaery in case of atom.
fn put_timestamp(nanos: J, stream: &mut String) -> bool {
    if nanos == qnull_base::J {
        stream.push_str("0N");
        true
    } else if nanos == qinf_base::J {
        stream.push_str("0W");
        true
    } else if nanos == qninf_base::J {
        stream.push_str("-0W");
        true
    } else {
        stream.push_str(
            q_timestamp_to_datetime(nanos)
                .format("%Y.%m.%dD%H:%M:%S%.9f")
                .to_string()
                .as_str(),
        );
        false
    }
}

fn put_month(months: I, stream: &mut String) {
    if months == qnull_base::I {
        stream.push_str("0N")
    } else if months == qinf_base::I {
        stream.push_str("0W")
    } else if months == qninf_base::I {
        stream.push_str("-0W")
    } else {
        match months.signum() {
            -1 => {
                let year = 2000 + months.signum() * (1 + months.abs() / 12);
                let month = 12 + (1 + months % 12);
                stream.push_str(format!("{}.{:02}", year, month).as_str())
            }
            _ => {
                stream.push_str(format!("{}.{:02}", 2000 + months / 12, 1 + (months % 12)).as_str())
            }
        }
    }
}

/// Put formatted date value to a stream and return if 'd' suffix is necessaery in case of atom.
fn put_date(days: I, stream: &mut String) -> bool {
    if days == qnull_base::I {
        stream.push_str("0N");
        true
    } else if days == qinf_base::I {
        stream.push_str("0W");
        true
    } else if days == qninf_base::I {
        stream.push_str("-0W");
        true
    } else {
        stream.push_str(
            Utc.timestamp_nanos(ONE_DAY_NANOS * (days + KDB_DAY_OFFSET) as i64)
                .date_naive()
                .format("%Y.%m.%d")
                .to_string()
                .as_str(),
        );
        false
    }
}

/// Put formatted datetime value to a stream and return if 'z' suffix is necessaery in case of atom.
fn put_datetime(days: F, stream: &mut String) -> bool {
    if days.is_nan() {
        stream.push_str("0N");
        true
    } else if days.is_infinite() && days.is_sign_negative() {
        stream.push_str("-0W");
        true
    } else if days.is_infinite() {
        stream.push_str("0W");
        true
    } else {
        stream.push_str(
            q_datetime_to_datetime(days)
                .format("%Y.%m.%dT%H:%M:%S%.3f")
                .to_string()
                .as_str(),
        );
        false
    }
}

/// Put formatted timespan value to a stream and return if 'n' suffix is necessaery in case of atom.
fn put_timespan(nanos: J, stream: &mut String) -> bool {
    if nanos == qnull_base::J {
        stream.push_str("0N");
        true
    } else if nanos == qinf_base::J {
        stream.push_str("0W");
        true
    } else if nanos == qninf_base::J {
        stream.push_str("-0W");
        true
    } else {
        let duration = q_timespan_to_duration(nanos);
        if duration.num_nanoseconds().unwrap() < 0 {
            stream.push_str(
                format!(
                    "-{}D{:02}:{:02}:{:02}.{:09}",
                    duration.num_days().abs(),
                    duration.num_hours().abs() % 24,
                    duration.num_minutes().abs() % 60,
                    duration.num_seconds().abs() % 60,
                    duration.num_nanoseconds().unwrap_or(0).abs() % 1_000_000_000_i64
                )
                .as_str(),
            );
        } else {
            stream.push_str(
                format!(
                    "{}D{:02}:{:02}:{:02}.{:09}",
                    duration.num_days(),
                    duration.num_hours() % 24,
                    duration.num_minutes() % 60,
                    duration.num_seconds() % 60,
                    duration.num_nanoseconds().unwrap_or(0) % 1_000_000_000_i64
                )
                .as_str(),
            );
        }
        false
    }
}

/// Put formatted minute value to a stream and return if 'u' suffix is necessaery in case of atom.
fn put_minute(minutes: I, stream: &mut String) -> bool {
    if minutes == qnull_base::I {
        stream.push_str("0N");
        true
    } else if minutes == qinf_base::I {
        stream.push_str("0W");
        true
    } else if minutes == qninf_base::I {
        stream.push_str("-0W");
        true
    } else {
        let duration = q_minute_to_duration(minutes);
        if duration.num_minutes() < 0 {
            stream.push_str(
                format!(
                    "-{:02}:{:02}",
                    duration.num_hours().abs() % 24,
                    duration.num_minutes().abs() % 60
                )
                .as_str(),
            );
        } else {
            stream.push_str(
                format!(
                    "{:02}:{:02}",
                    duration.num_hours() % 24,
                    duration.num_minutes() % 60
                )
                .as_str(),
            );
        }
        false
    }
}

/// Put formatted second value to a stream and return if 'v' suffix is necessaery in case of atom.
fn put_second(seconds: I, stream: &mut String) -> bool {
    if seconds == qnull_base::I {
        stream.push_str("0N");
        true
    } else if seconds == qinf_base::I {
        stream.push_str("0W");
        true
    } else if seconds == qninf_base::I {
        stream.push_str("-0W");
        true
    } else {
        let duration = q_second_to_duration(seconds);
        if duration.num_seconds() < 0 {
            stream.push_str(
                format!(
                    "-{:02}:{:02}:{:02}",
                    duration.num_hours().abs() % 24,
                    duration.num_minutes().abs() % 60,
                    duration.num_seconds().abs() % 60
                )
                .as_str(),
            );
        } else {
            stream.push_str(
                format!(
                    "{:02}:{:02}:{:02}",
                    duration.num_hours() % 24,
                    duration.num_minutes() % 60,
                    duration.num_seconds() % 60
                )
                .as_str(),
            );
        }
        false
    }
}

/// Put formatted time value to a stream and return if 't' suffix is necessaery in case of atom.
fn put_time(millis: I, stream: &mut String) -> bool {
    if millis == qnull_base::I {
        stream.push_str("0N");
        true
    } else if millis == qinf_base::I {
        stream.push_str("0W");
        true
    } else if millis == qninf_base::I {
        stream.push_str("-0W");
        true
    } else {
        let duration = q_time_to_duration(millis);
        if duration.num_milliseconds() < 0 {
            stream.push_str(
                format!(
                    "-{:02}:{:02}:{:02}.{:03}",
                    duration.num_hours().abs() % 24,
                    duration.num_minutes().abs() % 60,
                    duration.num_seconds().abs() % 60,
                    duration.num_milliseconds().abs() % 1000_i64
                )
                .as_str(),
            );
        } else {
            stream.push_str(
                format!(
                    "{:02}:{:02}:{:02}.{:03}",
                    duration.num_hours() % 24,
                    duration.num_minutes() % 60,
                    duration.num_seconds() % 60,
                    duration.num_milliseconds() % 1000_i64
                )
                .as_str(),
            );
        }
        false
    }
}

fn put_attribute(attribute: i8, stream: &mut String) {
    match attribute {
        qattribute::SORTED => stream.push_str("`s#"),
        qattribute::PARTED => stream.push_str("`p#"),
        qattribute::UNIQUE => stream.push_str("`u#"),
        qattribute::GROUPED => stream.push_str("`g#"),
        // Nothing to do
        _ => {}
    }
}

fn put_bool_list(list: &[G], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`bool$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        list.iter().for_each(|element| {
            put_bool(*element, stream);
        });
        stream.push('b');
    }
}

fn put_guid_list(list: &[U], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`guid$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_guid(*item, stream);
            stream.push(' ');
        }
        put_guid(list[size - 1], stream);
    }
}

fn put_byte_list(list: &[G], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`byte$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        stream.push_str("0x");
        list.iter().for_each(|element| {
            put_byte(*element, stream);
        });
    }
}

fn put_short_list(list: &[H], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`short$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_short(*item, stream);
            stream.push(' ');
        }
        put_short(list[size - 1], stream);
        stream.push('h');
    }
}

fn put_int_list(list: &[I], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`int$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_int(*item, stream);
            stream.push(' ');
        }
        put_int(list[size - 1], stream);
        stream.push('i');
    }
}

fn put_long_list(list: &[J], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`long$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_long(*item, stream);
            stream.push(' ');
        }
        put_long(list[size - 1], stream);
    }
}

fn put_real_list(list: &[E], stream: &mut String, precision: usize) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`real$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_real(*item, stream, precision);
            stream.push(' ');
        }
        put_real(list[size - 1], stream, precision);
        stream.push('e');
    }
}

fn put_float_list(list: &[F], stream: &mut String, precision: usize) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`float$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_float(*item, stream, precision);
            stream.push(' ');
        }
        put_float(list[size - 1], stream, precision);
    }
}

fn put_string(string: &str, stream: &mut String) {
    let size = string.len();
    if size == 1 {
        stream.push(',');
    }
    stream.push('"');
    stream.push_str(string);
    stream.push('"');
}

fn put_symbol_list(list: &[S], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`symbol$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_symbol(item, stream);
        }
        put_symbol(&list[size - 1], stream);
    }
}

fn put_timestamp_list(list: &[J], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`timestamp$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_timestamp(*item, stream);
            stream.push(' ');
        }
        if put_timestamp(list[size - 1], stream) {
            stream.push('p');
        }
    }
}

fn put_month_list(list: &[I], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`month$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_month(*item, stream);
            stream.push(' ');
        }
        put_month(list[size - 1], stream);
        stream.push('m');
    }
}

fn put_date_list(list: &[I], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`date$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_date(*item, stream);
            stream.push(' ');
        }
        if put_date(list[size - 1], stream) {
            stream.push('d');
        }
    }
}

fn put_datetime_list(list: &[F], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`datetime$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_datetime(*item, stream);
            stream.push(' ');
        }
        if put_datetime(list[size - 1], stream) {
            stream.push('z');
        }
    }
}

fn put_timespan_list(list: &[J], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`timespan$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_timespan(*item, stream);
            stream.push(' ');
        }
        if put_timespan(list[size - 1], stream) {
            stream.push('n');
        }
    }
}

fn put_minute_list(list: &[I], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`minute$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_minute(*item, stream);
            stream.push(' ');
        }
        if put_minute(list[size - 1], stream) {
            stream.push('u');
        }
    }
}

fn put_second_list(list: &[I], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`second$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_second(*item, stream);
            stream.push(' ');
        }
        if put_second(list[size - 1], stream) {
            stream.push('v');
        }
    }
}

fn put_time_list(list: &[I], stream: &mut String) {
    let size = list.len();
    if size == 0 {
        stream.push_str("`time$()");
    } else {
        if size == 1 {
            stream.push(',');
        }
        for item in list.iter().take(size - 1) {
            put_time(*item, stream);
            stream.push(' ');
        }
        if put_time(list[size - 1], stream) {
            stream.push('t');
        }
    }
}

fn put_compound_list(list: &[K], stream: &mut String, precision: usize) {
    let size = list.len();
    if size == 0 {
        stream.push_str("()");
    } else if size == 1 {
        stream.push(',');
        put_q(&list[0], stream, precision);
    } else {
        stream.push('(');
        for item in list.iter().take(size - 1) {
            put_q(item, stream, precision);
            stream.push(';');
        }
        put_q(&list[size - 1], stream, precision);
        stream.push(')');
    }
}

fn put_table(table: &K, stream: &mut String, precision: usize) {
    stream.push('+');
    put_dictionary(table.get_dictionary().unwrap(), stream, precision);
}

fn put_dictionary(dictionary: &K, stream: &mut String, precision: usize) {
    let dictionary_ = dictionary.as_vec::<K>().unwrap();
    let is_keyed_table = dictionary_[0].get_type() == qtype::TABLE;
    if is_keyed_table {
        stream.push('(');
    }
    put_q(&dictionary_[0], stream, precision);
    if is_keyed_table {
        stream.push(')');
    }
    stream.push('!');
    if is_keyed_table {
        stream.push('(');
    }
    put_q(&dictionary_[1], stream, precision);
    if is_keyed_table {
        stream.push(')');
    }
}

fn put_q(object: &K, stream: &mut String, precision: usize) {
    match object.0.qtype {
        qtype::BOOL_ATOM => {
            put_bool(object.get_byte().unwrap(), stream);
            stream.push('b');
        }
        qtype::GUID_ATOM => put_guid(object.get_guid().unwrap(), stream),
        qtype::BYTE_ATOM => {
            stream.push_str("0x");
            put_byte(object.get_byte().unwrap(), stream);
        }
        qtype::SHORT_ATOM => {
            put_short(object.get_short().unwrap(), stream);
            stream.push('h');
        }
        qtype::INT_ATOM => {
            put_int(object.get_int().unwrap(), stream);
            stream.push('i');
        }
        qtype::LONG_ATOM => put_long(object.get_long().unwrap(), stream),
        qtype::REAL_ATOM => {
            put_real(object.get_real().unwrap(), stream, precision);
            stream.push('e');
        }
        qtype::FLOAT_ATOM => put_float(object.get_float().unwrap(), stream, precision),
        qtype::CHAR => {
            stream.push('"');
            stream.push(object.get_char().unwrap());
            stream.push('"');
        }
        qtype::SYMBOL_ATOM => put_symbol(object.get_symbol().unwrap(), stream),
        qtype::TIMESTAMP_ATOM => {
            if put_timestamp(object.get_long().unwrap(), stream) {
                stream.push('p');
            }
        }
        qtype::MONTH_ATOM => {
            put_month(object.get_int().unwrap(), stream);
            stream.push('m');
        }
        qtype::DATE_ATOM => {
            if put_date(object.get_int().unwrap(), stream) {
                stream.push('d');
            }
        }
        qtype::DATETIME_ATOM => {
            if put_datetime(object.get_float().unwrap(), stream) {
                stream.push('z');
            }
        }
        qtype::TIMESPAN_ATOM => {
            if put_timespan(object.get_long().unwrap(), stream) {
                stream.push('n');
            }
        }
        qtype::MINUTE_ATOM => {
            if put_minute(object.get_int().unwrap(), stream) {
                stream.push('u');
            }
        }
        qtype::SECOND_ATOM => {
            if put_second(object.get_int().unwrap(), stream) {
                stream.push('v');
            }
        }
        qtype::TIME_ATOM => {
            if put_time(object.get_int().unwrap(), stream) {
                stream.push('t');
            }
        }
        qtype::COMPOUND_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_compound_list(object.as_vec::<K>().unwrap(), stream, precision)
        }
        qtype::BOOL_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_bool_list(object.as_vec::<G>().unwrap(), stream)
        }
        qtype::GUID_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_guid_list(object.as_vec::<U>().unwrap(), stream)
        }
        qtype::BYTE_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_byte_list(object.as_vec::<G>().unwrap(), stream)
        }
        qtype::SHORT_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_short_list(object.as_vec::<H>().unwrap(), stream)
        }
        qtype::INT_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_int_list(object.as_vec::<I>().unwrap(), stream)
        }
        qtype::LONG_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_long_list(object.as_vec::<J>().unwrap(), stream)
        }
        qtype::REAL_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_real_list(object.as_vec::<E>().unwrap(), stream, precision)
        }
        qtype::FLOAT_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_float_list(object.as_vec::<F>().unwrap(), stream, precision)
        }
        qtype::STRING => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_string(object.as_string().unwrap(), stream)
        }
        qtype::SYMBOL_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_symbol_list(object.as_vec::<S>().unwrap(), stream)
        }
        qtype::TIMESTAMP_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_timestamp_list(object.as_vec::<J>().unwrap(), stream)
        }
        qtype::MONTH_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_month_list(object.as_vec::<I>().unwrap(), stream)
        }
        qtype::DATE_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_date_list(object.as_vec::<I>().unwrap(), stream)
        }
        qtype::DATETIME_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_datetime_list(object.as_vec::<F>().unwrap(), stream)
        }
        qtype::TIMESPAN_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_timespan_list(object.as_vec::<J>().unwrap(), stream)
        }
        qtype::MINUTE_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_minute_list(object.as_vec::<I>().unwrap(), stream)
        }
        qtype::SECOND_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_second_list(object.as_vec::<I>().unwrap(), stream)
        }
        qtype::TIME_LIST => {
            // Put an attribute.
            put_attribute(object.0.attribute, stream);
            put_time_list(object.as_vec::<I>().unwrap(), stream)
        }
        qtype::TABLE => put_table(object, stream, precision),
        qtype::DICTIONARY | qtype::SORTED_DICTIONARY => put_dictionary(object, stream, precision),
        qtype::NULL => stream.push_str("::"),
        _ => unimplemented!(),
    }
}
