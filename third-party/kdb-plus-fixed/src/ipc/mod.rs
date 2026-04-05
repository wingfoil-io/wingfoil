//! This `ipc` module provides an interface to interact with q/kdb+ via IPC. The expected usage is to send a (text) query to q/kdb+ process
//!  from Rust client and receive its response. Query to kdb+ is supported in two ways:
//!
//! - text query
//! - functional query which is represented by a compound list of kdb+ ([See detail of IPC](https://code.kx.com/q4m3/11_IO/#116-interprocess-communication)).
//!
//! Compression/decompression of messages is also implemented following [kdb+ implementation](https://code.kx.com/q/basics/ipc/#compression).
//!
//! As for connect method, usually client interfaces of q/kdb+ do not provide a listener due to its protocol. However, sometimes Rust process is
//!  connecting to upstream and q/kdb+ starts afterward or is restarted more frequently. Then providing a listener method is a natural direction
//!  and it was achieved here. Following ways are supported to connect to kdb+:
//!
//! - TCP
//! - TLS
//! - Unix domain socket
//!
//! Furthermore, in order to improve inter-operatability some casting, getter and setter methods are provided.
//!
//! ## Environmentl Variables
//!
//! This crate uses q-native or crate-specific environmental variables.
//!
//! - `KDBPLUS_ACCOUNT_FILE`: A file path to a credential file which an acceptor loads in order to manage access from a q client. This file contains
//!   a user name and SHA-1 hashed password in each line which are delimited by `':'` without any space. For example, a file containing two credentials
//!   `"mattew:oracle"` and `"reluctant:slowday"` looks like this:
//!
//!      ```bash
//!       mattew:431364b6450fc47ccdbf6a2205dfdb1baeb79412
//!       reluctant:d03f5cc1cdb11a77410ee34e26ca1102e67a893c
//!      ```
//!       
//!     The hashed password can be generated with q using a function `.Q.sha1`:
//!  
//!      ```q
//!       q).Q.sha1 "slowday"
//!       0xd03f5cc1cdb11a77410ee34e26ca1102e67a893c
//!      ```
//!
//! - `KDBPLUS_TLS_KEY_FILE` and `KDBPLUS_TLS_KEY_FILE_SECRET`: The pkcs12 file and its password which TLS acceptor uses.
//! - `QUDSPATH` (optional): q-native environmental variable to define an astract namespace. This environmental variable is used by UDS acceptor too.
//!   The abstract nameapace will be `@${QUDSPATH}/kx.[server process port]` if this environmental variable is defined. Otherwise it will be `@/tmp/kx.[server process port]`.
//!
//! *Notes:*
//!
//! - Messages will be sent with OS native endian.
//! - When using this crate for a TLS client you need to set two environmental variables `KX_SSL_CERT_FILE` and `KX_SSL_KEY_FILE` on q side to make q/kdb+
//!   to work as a TLS server. For details, see [the KX website](https://code.kx.com/q/kb/ssl/).
//!
//! ## Type Mapping
//!
//! All types are expressed as `K` struct which is quite similar to the `K` struct of `api` module but its structure is optimized for IPC
//!  usage and for convenience to interact with. The table below shows the input types of each q type which is used to construct `K` object.
//!  Note that the input type can be different from the inner type. For example, timestamp has an input type of `chrono::DateTime<Utc>` but
//!  the inner type is `i64` denoting an elapsed time in nanoseconds since `2000.01.01D00:00:00`.
//!
//! | q                | Rust                                              |
//! |------------------|---------------------------------------------------|
//! | `bool`           | `bool`                                            |
//! | `GUID`           | `[u8; 16]`                                        |
//! | `byte`           | `u8`                                              |
//! | `short`          | `i16`                                             |
//! | `int`            | `i32`                                             |
//! | `long`           | `i64`                                             |
//! | `real`           | `f32`                                             |
//! | `float`          | `f64`                                             |
//! | `char`           | `char`                                            |
//! | `symbol`         | `String`                                          |
//! | `timestamp`      | `chrono::DateTime<Utc>`                           |
//! | `month`          | `chrono::NaiveDate`                               |
//! | `date`           | `chrono::NaiveDate`                               |
//! | `datetime`       | `chrono::DateTime<Utc>`                           |
//! | `timespan`       | `chrono::Duration`                                |
//! | `minute`         | `chrono::Duration`                                |
//! | `second`         | `chrono::Duration`                                |
//! | `time`           | `chrono::Duration`                                |
//! | `list`           | `Vec<Item>` (`Item` is a corrsponding type above) |
//! | `compound list`  | `Vec<K>`                                          |
//! | `table`          | `Vec<K>`                                          |
//! | `dictionary`     | `Vec<K>`                                          |
//! | `null`           | `()`                                              |
//!
//! ## Examples
//!
//! ### Client
//!
//! ```rust
//! use kdbplus::qattribute;
//! use kdbplus::ipc::*;
//!
//! #[tokio::main(flavor = "multi_thread", worker_threads = 2)]
//! async fn main() -> Result<()> {
//!     // Connect to qprocess running on localhost:5000 via UDS
//!     let mut socket = QStream::connect(ConnectionMethod::UDS, "", 5000_u16, "ideal:person").await?;
//!     println!("Connection type: {}", socket.get_connection_type());
//!
//!     // Set remote function with an asynchronous text form message
//!     socket.send_async_message(&"collatz:{[n] seq:enlist n; while[not n = 1; seq,: n:$[n mod 2; 1 + 3 * n; `long$n % 2]]; seq}").await?;
//!
//!     // Send a text form emessage synchronously
//!     let mut result = socket.send_sync_message(&"collatz[12]").await?;
//!     println!("collatz[12]: {}", result);
//!
//!     result = socket.send_sync_message(&"collatz[`a]").await?;
//!     println!("collatz[`a]: {}", result);
//!
//!     // Send a functional form message synchronously.
//!     let mut message = K::new_compound_list(vec![
//!         K::new_symbol(String::from("collatz")),
//!         K::new_long(100),
//!     ]);
//!     result = socket.send_sync_message(&message).await?;
//!     println!("collatz[100]: {}", result);
//!
//!     // Modify the message to (`collatz; 20)
//!     message.pop().unwrap();
//!     message.push(&K::new_long(20)).unwrap();
//!     result = socket.send_sync_message(&message).await?;
//!     println!("collatz[20]: {}", result);
//!
//!     // Send a functional form message asynchronous query.
//!     message = K::new_compound_list(vec![
//!         K::new_string(String::from("show"), qattribute::NONE),
//!         K::new_symbol(String::from("goodbye")),
//!     ]);
//!     socket.send_async_message(&message).await?;
//!
//!     socket.shutdown().await?;
//!
//!     Ok(())
//! }
//!```
//! ### Listener
//!
//! ```no_run
//! use std::io;
//! use kdbplus::ipc::*;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     // Start listenening over TCP at the port 7000 with authentication enabled.
//!     let mut socket_tcp = QStream::accept(ConnectionMethod::TCP, "127.0.0.1", 7000).await?;
//!
//!     // Send a query with the socket.
//!     let greeting = socket_tcp.send_sync_message(&"string `Hello").await?;
//!     println!("Greeting: {}", greeting);
//!
//!     socket_tcp.shutdown().await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! Then q client can connect to this acceptor with the acceptor's host, port and the credential configured in `KDBPLUS_ACCOUNT_FILE`:
//!
//! ```q
//! q)h:hopen `::7000:reluctant:slowday
//! ```

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Settings
//++++++++++++++++++++++++++++++++++++++++++++++++++//

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::borrow_interior_mutable_const)]
#![allow(clippy::declare_interior_mutable_const)]

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Load Libraries
//++++++++++++++++++++++++++++++++++++++++++++++++++//

pub mod error;

use super::{qattribute, qinf_base, qninf_base, qnull_base, qtype};
use chrono::prelude::*;
use chrono::Duration;
use error::Error;
use std::any::Any;
use std::fmt;
use std::result::Result as StdResult;

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Structs
//++++++++++++++++++++++++++++++++++++++++++++++++++//

pub type Result<T> = StdResult<T, Error>;

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Global Variables
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% kdb+ Offset %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// 1 day in nano second.
pub const ONE_DAY_NANOS: i64 = 86400000000000;

/// 1 day in milli second.
pub const ONE_DAY_MILLIS: i64 = 86400000;

/// 2000.01.01 (kdb+ epoch) - 1970.01.01 in month.
pub const KDB_MONTH_OFFSET: i32 = 360;

/// 2000.01.01 (kdb+ epoch) - 1970.01.01 in day.
pub const KDB_DAY_OFFSET: i32 = 10957;

/// 2000.01.01 (kdb+ epoch) - 1970.01.01 in nanosecond.
pub const KDB_TIMESTAMP_OFFSET: i64 = 946684800000000000;

//%% Null & Infinity %%vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

pub mod qnull {
    //! This module provides a list of q null values set on Rust process and used for IPC. The motivation
    //!  to contain them in a module is to tie them up as related items rather than scattered values.
    //!  Hence user should use these indicators with `qnull::` prefix, e.g., `qnull::FLOAT`.

    use super::qnull_base;
    use chrono::prelude::*;
    use chrono::Duration;
    use once_cell::sync::Lazy;

    /// Null value of GUID (`0Ng`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_guid_null = K::new_guid(qnull::GUID);
    /// assert_eq!(
    ///     format!("{}", q_guid_null),
    ///     String::from("00000000-0000-0000-0000-000000000000")
    /// );
    /// ```
    pub const GUID: [u8; 16] = [0_u8; 16];

    /// Null value of short (`0Nh`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_short_null = K::new_short(qnull::SHORT);
    /// assert_eq!(format!("{}", q_short_null), String::from("0Nh"));
    /// ```
    pub const SHORT: i16 = qnull_base::H;

    /// Null value of int (`0Ni`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_int_null = K::new_int(qnull::INT);
    /// assert_eq!(format!("{}", q_int_null), String::from("0Ni"));
    /// ```
    pub const INT: i32 = qnull_base::I;

    /// Null value of long (`0N`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_long_null = K::new_long(qnull::LONG);
    /// assert_eq!(format!("{}", q_long_null), String::from("0N"));
    /// ```
    pub const LONG: i64 = qnull_base::J;

    /// Null value of real (`0Ne`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_real_null = K::new_real(qnull::REAL);
    /// assert_eq!(format!("{}", q_real_null), String::from("0Ne"));
    /// ```
    pub const REAL: f32 = qnull_base::E;

    /// Null value of float (`0n`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_float_null = K::new_float(qnull::FLOAT);
    /// assert_eq!(format!("{}", q_float_null), String::from("0n"));
    /// ```
    pub const FLOAT: f64 = qnull_base::F;

    /// Null value of char (`" "`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_char_null = K::new_char(qnull::CHAR);
    /// assert_eq!(format!("{}", q_char_null), String::from("\" \""));
    /// ```
    pub const CHAR: char = qnull_base::C;

    /// Null value of symbol (<code>`</code>).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_symbol_null = K::new_symbol(qnull::SYMBOL);
    /// assert_eq!(format!("{}", q_symbol_null), String::from("`"));
    /// ```
    pub const SYMBOL: String = String::new();

    /// Null value of timestamp (`0Np`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_timestamp_null = K::new_timestamp(*qnull::TIMESTAMP);
    /// assert_eq!(format!("{}", q_timestamp_null), String::from("0Np"));
    /// ```
    /// # Note
    /// The range of timestamp in Rust is wider than in q.
    pub const TIMESTAMP: Lazy<DateTime<Utc>> = Lazy::new(|| {
        NaiveDate::from_ymd_opt(1707, 9, 22)
            .unwrap()
            .and_hms_nano_opt(0, 12, 43, 145224192)
            .unwrap()
            .and_local_timezone(Utc)
            .unwrap()
    });

    /// Null value of month (`0Nm`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_month_null = K::new_month(qnull::MONTH);
    /// assert_eq!(format!("{}", q_month_null), String::from("0Nm"));
    /// ```
    /// # Note
    /// The range of month in Rust is narrower than in q.
    pub const MONTH: NaiveDate = NaiveDate::MIN;

    /// Null valueo of date (`0Nd`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_date_null = K::new_date(qnull::DATE);
    /// assert_eq!(format!("{}", q_date_null), String::from("0Nd"));
    /// ```
    /// # Note
    /// The range of date in Rust is narrower than in q.
    pub const DATE: NaiveDate = NaiveDate::MIN;

    /// Null value of datetime (`0Nz`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_datetime_null = K::new_datetime(qnull::DATETIME);
    /// assert_eq!(format!("{}", q_datetime_null), String::from("0Nz"));
    /// ```
    /// # Note
    /// The range of datetime in Rust is narrower than in q.
    pub const DATETIME: DateTime<Utc> = DateTime::<Utc>::MIN_UTC;

    /// Null value of timespan (`0Nn`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_timespan_null = K::new_timespan(*qnull::TIMESPAN);
    /// assert_eq!(format!("{}", q_timespan_null), String::from("0Nn"));
    /// ```
    pub const TIMESPAN: Lazy<Duration> = Lazy::new(|| Duration::nanoseconds(qnull_base::J));

    /// Null value of minute (`0Nu`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_minute_null = K::new_minute(*qnull::MINUTE);
    /// assert_eq!(format!("{}", q_minute_null), String::from("0Nu"));
    /// ```
    pub const MINUTE: Lazy<Duration> = Lazy::new(|| Duration::minutes(qnull_base::I as i64));

    /// Null value of second (`0Nv`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_second_null = K::new_second(*qnull::SECOND);
    /// assert_eq!(format!("{}", q_second_null), String::from("0Nv"));
    /// ```
    pub const SECOND: Lazy<Duration> = Lazy::new(|| Duration::seconds(qnull_base::I as i64));

    /// Null value of time (`0Nt`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_time_null = K::new_time(*qnull::TIME);
    /// assert_eq!(format!("{}", q_time_null), String::from("0Nt"));
    /// ```
    pub const TIME: Lazy<Duration> = Lazy::new(|| Duration::milliseconds(qnull_base::I as i64));
}

pub mod qinf {
    //! This module provides a list of q infinite values set on Rust process and used for IPC.
    //!  The motivation to contain them in a module is to tie them up as related items rather
    //!  than scattered values. Hence user should use these indicators with `qnull::` prefix, e.g., `qnull::FLOAT`.

    use super::qinf_base;
    use chrono::prelude::*;
    use chrono::Duration;
    use once_cell::sync::Lazy;

    /// Infinity value of short (`0Wh`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_short_inf = K::new_short(qinf::SHORT);
    /// assert_eq!(format!("{}", q_short_inf), String::from("0Wh"));
    /// ```
    pub const SHORT: i16 = qinf_base::H;

    /// Infinity value of int (`0Wi`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_int_inf = K::new_int(qinf::INT);
    /// assert_eq!(format!("{}", q_int_inf), String::from("0Wi"));
    /// ```
    pub const INT: i32 = qinf_base::I;

    /// Infinity value of long (`0W`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_long = K::new_long(86400000000000);
    /// assert_eq!(format!("{}", q_long), String::from("86400000000000"));
    /// ```
    pub const LONG: i64 = qinf_base::J;

    /// Infinity value of real (`0We`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_real_null = K::new_real(qnull::REAL);
    /// assert_eq!(format!("{}", q_real_null), String::from("0Ne"));
    /// ```
    pub const REAL: f32 = qinf_base::E;

    /// Infinity value of float (`0w`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_float_inf = K::new_float(qinf::FLOAT);
    /// assert_eq!(format!("{}", q_float_inf), String::from("0w"));
    /// ```
    pub const FLOAT: f64 = qinf_base::F;

    /// Infinity value of timestamp (`0Wp`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_timestamp_inf = K::new_timestamp(*qinf::TIMESTAMP);
    /// assert_eq!(format!("{}", q_timestamp_inf), String::from("0Wp"));
    /// ```
    /// # Note
    /// The range of timestamp in Rust is wider than in q.
    pub const TIMESTAMP: Lazy<DateTime<Utc>> = Lazy::new(|| {
        NaiveDate::from_ymd_opt(2292, 4, 10)
            .unwrap()
            .and_hms_nano_opt(23, 47, 16, 854775807)
            .unwrap()
            .and_local_timezone(Utc)
            .unwrap()
    });

    /// Infinity value of month (`0Wm`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_month_inf = K::new_month(*qinf::MONTH);
    /// assert_eq!(format!("{}", q_month_inf), String::from("0Wm"));
    /// ```
    /// # Note
    /// The range of month in Rust is narrower than in q.
    pub const MONTH: Lazy<NaiveDate> = Lazy::new(|| NaiveDate::MAX - Duration::days(30));

    /// Infinity valueo of date (`0Wd`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_date_inf = K::new_date(qinf::DATE);
    /// assert_eq!(format!("{}", q_date_inf), String::from("0Wd"));
    /// ```
    /// # Note
    /// The range of date in Rust is narrower than in q.
    pub const DATE: NaiveDate = NaiveDate::MAX;

    /// Infinity value of datetime (`0Wz`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_datetime_inf = K::new_datetime(*qinf::DATETIME);
    /// assert_eq!(format!("{}", q_datetime_inf), String::from("0Wz"));
    /// ```
    /// # Note
    /// The range of datetime in Rust is narrower than in q.
    pub const DATETIME: Lazy<DateTime<Utc>> =
        Lazy::new(|| DateTime::<Utc>::MAX_UTC - Duration::nanoseconds(999999));

    /// Infinity value of timespan (`0Wn`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_timespan_inf = K::new_timespan(*qinf::TIMESPAN);
    /// assert_eq!(format!("{}", q_timespan_inf), String::from("0Wn"));
    /// ```
    pub const TIMESPAN: Lazy<Duration> = Lazy::new(|| Duration::nanoseconds(qinf_base::J));

    /// Infinity value of minute (`0Wu`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_minute_inf = K::new_minute(*qinf::MINUTE);
    /// assert_eq!(format!("{}", q_minute_inf), String::from("0Wu"));
    /// ```
    pub const MINUTE: Lazy<Duration> = Lazy::new(|| Duration::minutes(qinf_base::I as i64));

    /// Infinity value of second (`0Wv`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_second_inf = K::new_second(*qinf::SECOND);
    /// assert_eq!(format!("{}", q_second_inf), String::from("0Wv"));
    /// ```
    pub const SECOND: Lazy<Duration> = Lazy::new(|| Duration::seconds(qinf_base::I as i64));

    /// Infinity value of time (`0Wt`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_time_inf = K::new_time(*qinf::TIME);
    /// assert_eq!(format!("{}", q_time_inf), String::from("0Wt"));
    /// ```
    pub const TIME: Lazy<Duration> = Lazy::new(|| Duration::milliseconds(qinf_base::I as i64));
}

pub mod qninf {
    //! This module provides a list of q negative infinite values set on Rust process and used for IPC.
    //!  The motivation to contain them in a module is to tie them up as related items rather than
    //!  scattered values. Hence user should use these indicators with `qnull::` prefix, e.g., `qnull::FLOAT`.

    use super::qninf_base;
    use chrono::prelude::*;
    use chrono::Duration;
    use once_cell::sync::Lazy;

    /// Infinity value of short (`-0Wh`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_short_ninf = K::new_short(qninf::SHORT);
    /// assert_eq!(format!("{}", q_short_ninf), String::from("-0Wh"));
    /// ```
    pub const SHORT: i16 = qninf_base::H;

    /// Infinity value of int (`-0Wi`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_int_ninf = K::new_int(qninf::INT);
    /// assert_eq!(format!("{}", q_int_ninf), String::from("-0Wi"));
    /// ```
    pub const INT: i32 = qninf_base::I;

    /// Infinity value of long (-`0W`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_long_ninf = K::new_long(qninf::LONG);
    /// assert_eq!(format!("{}", q_long_ninf), String::from("-0W"));
    /// ```
    pub const LONG: i64 = qninf_base::J;

    /// Infinity value of real (`-0We`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_real_ninf: K = K::new_real(qninf::REAL);
    /// assert_eq!(format!("{}", q_real_ninf), String::from("-0We"));
    /// ```
    pub const REAL: f32 = qninf_base::E;

    /// Infinity value of float (`-0w`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_float_ninf = K::new_float(qninf::FLOAT);
    /// assert_eq!(format!("{}", q_float_ninf), String::from("-0w"));
    /// ```
    pub const FLOAT: f64 = qninf_base::F;

    /// Infinity value of timestamp (`-0Wp`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_timestamp_ninf = K::new_timestamp(*qninf::TIMESTAMP);
    /// assert_eq!(format!("{}", q_timestamp_ninf), String::from("-0Wp"));
    /// ```
    /// # Note
    /// The range of timestamp in Rust is wider than in q.
    pub const TIMESTAMP: Lazy<DateTime<Utc>> = Lazy::new(|| {
        NaiveDate::from_ymd_opt(1707, 9, 22)
            .unwrap()
            .and_hms_nano_opt(0, 12, 43, 145224193)
            .unwrap()
            .and_local_timezone(Utc)
            .unwrap()
    });

    /// Infinity value of month (`-0Wm`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_month_ninf = K::new_month(*qninf::MONTH);
    /// assert_eq!(format!("{}", q_month_ninf), String::from("-0Wm"));
    /// ```
    /// # Note
    /// The range of month in Rust is narrower than in q.
    pub const MONTH: Lazy<NaiveDate> = Lazy::new(|| NaiveDate::MIN + Duration::days(31));

    /// Infinity valueo of date (`-0Wd`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_date_ninf = K::new_date(*qninf::DATE);
    /// assert_eq!(format!("{}", q_date_ninf), String::from("-0Wd"));
    /// ```
    /// # Note
    /// The range of date in Rust is narrower than in q.
    pub const DATE: Lazy<NaiveDate> = Lazy::new(|| NaiveDate::MIN + Duration::days(1));

    /// Infinity value of datetime (`-0Wz`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_datetime_ninf = K::new_datetime(*qninf::DATETIME);
    /// assert_eq!(format!("{}", q_datetime_ninf), String::from("-0Wz"));
    /// ```
    /// # Note
    /// The range of datetime in Rust is narrower than in q.
    pub const DATETIME: Lazy<DateTime<Utc>> =
        Lazy::new(|| DateTime::<Utc>::MIN_UTC + Duration::nanoseconds(1000000));

    /// Infinity value of timespan (`-0Wn`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_timespan_ninf = K::new_timespan(*qninf::TIMESPAN);
    /// assert_eq!(format!("{}", q_timespan_ninf), String::from("-0Wn"));
    /// ```
    pub const TIMESPAN: Lazy<Duration> = Lazy::new(|| Duration::nanoseconds(qninf_base::J));

    /// Infinity value of minute (`-0Wu`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_minute_ninf = K::new_minute(*qninf::MINUTE);
    /// assert_eq!(format!("{}", q_minute_ninf), String::from("-0Wu"));
    /// ```
    pub const MINUTE: Lazy<Duration> = Lazy::new(|| Duration::minutes(qninf_base::I as i64));

    /// Infinity value of second (`-0Wv`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_second_ninf = K::new_second(*qninf::SECOND);
    /// assert_eq!(format!("{}", q_second_ninf), String::from("-0Wv"));
    /// ```
    pub const SECOND: Lazy<Duration> = Lazy::new(|| Duration::seconds(qninf_base::I as i64));

    /// Infinity value of time (`-0Wt`).
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_time_ninf = K::new_time(*qninf::TIME);
    /// assert_eq!(format!("{}", q_time_ninf), String::from("-0Wt"));
    /// ```
    pub const TIME: Lazy<Duration> = Lazy::new(|| Duration::milliseconds(qninf_base::I as i64));
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >>  Structs
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% Alias %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// q type denoting symbol and string.
pub type S = String;
/// q type denoting char.
pub type C = i8;
/// q type denoting bool and byte.
pub type G = u8;
/// q type denoting short.
pub type H = i16;
/// q type denoting int and its compatible types (month, date, minute, second and time) of q.
pub type I = i32;
/// q type denoting long and its compatible types (timestamp and timespan) of q.
pub type J = i64;
/// q type denoting real.
pub type E = f32;
/// q type denoting float and datetime.
pub type F = f64;
/// q type denoting GUID.
pub type U = [G; 16];

//%% AsAny %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Feature of q list object to be cast to concrete type internally.
pub(crate) trait AsAny {
    /// Return as Any type.
    fn as_any(&self) -> &dyn Any;
    /// Return as mutable Any type.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

//%% Klone %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Trait to clone `k0_list`.
pub(crate) trait Klone {
    fn clone_box(&self) -> Box<dyn k0_list_inner>;
}

//%% k0_list_inner %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Feature of q list.
pub(crate) trait k0_list_inner: Klone + fmt::Debug + AsAny + Send + Sync + 'static {
    /// Get a length of inner vector.
    fn len(&self) -> usize;
}

//%% k0_list %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Underlying list value of q object.
/// # Note
/// Usually this struct does not need to be accessed this struct directly unless user wants to
///  access via a raw pointer for non-trivial stuff.
#[derive(Debug)]
pub(crate) struct k0_list {
    /// Length of the list.
    n: J,
    /// Pointer referring to the head of the list. This pointer will be interpreted
    ///  as various types when accessing `K` object to edit the list with
    ///  [`as_mut_slice`](trait.KUtility.html#tymethod.as_mut_slice).
    G0: Box<dyn k0_list_inner>,
}

//%% k0_inner %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Underlying atom value of q object.
/// # Note
/// Usually this struct does not need to be accessed directly unless user wants to
///  access via a raw pointer for non-trivial stuff.
#[derive(Clone, Debug)]
pub(crate) enum k0_inner {
    /// Byte type holder.
    byte(G),
    /// GUID type holder.
    guid(U),
    /// Short type holder.
    short(H),
    /// Int type holder.
    int(I),
    /// Long type older.
    long(J),
    /// Real type holder.
    real(E),
    /// Float type holder.
    float(F),
    /// Symbol type holder.
    /// # Note
    /// String type is also stored here.
    symbol(S),
    /// Table type holder.
    table(K),
    /// List type holder.
    list(k0_list),
    /// Null type holder.
    null(()),
}

//%% k0 %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Underlying struct of q object.
#[derive(Clone, Debug)]
pub(crate) struct k0 {
    /// Type indicator.
    qtype: i8,
    /// Attribute of list.
    attribute: i8,
    /// Underlying value.
    value: k0_inner,
}

//%% K %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Struct representing q object.
#[derive(Clone, Debug)]
pub struct K(pub(crate) Box<k0>);

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Implementation
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% AsAny %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl AsAny for Vec<G> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AsAny for Vec<U> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AsAny for Vec<H> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AsAny for Vec<I> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AsAny for Vec<J> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AsAny for Vec<E> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AsAny for Vec<F> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AsAny for String {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AsAny for Vec<S> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AsAny for Vec<K> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

//%% Klone %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl<T> Klone for T
where
    T: k0_list_inner + Clone,
{
    fn clone_box(&self) -> Box<dyn k0_list_inner> {
        Box::new(self.clone())
    }
}

//%% k0_list_inner %%vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl k0_list_inner for Vec<G> {
    fn len(&self) -> usize {
        self.len()
    }
}

impl k0_list_inner for Vec<U> {
    fn len(&self) -> usize {
        self.len()
    }
}

impl k0_list_inner for Vec<H> {
    fn len(&self) -> usize {
        self.len()
    }
}

impl k0_list_inner for Vec<I> {
    fn len(&self) -> usize {
        self.len()
    }
}

impl k0_list_inner for Vec<J> {
    fn len(&self) -> usize {
        self.len()
    }
}

impl k0_list_inner for Vec<E> {
    fn len(&self) -> usize {
        self.len()
    }
}

impl k0_list_inner for Vec<F> {
    fn len(&self) -> usize {
        self.len()
    }
}

impl k0_list_inner for String {
    fn len(&self) -> usize {
        self.len()
    }
}

impl k0_list_inner for Vec<S> {
    fn len(&self) -> usize {
        self.len()
    }
}

impl k0_list_inner for Vec<K> {
    fn len(&self) -> usize {
        self.len()
    }
}

//%% k0_list %%vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl k0_list {
    pub(crate) fn new<T>(array: T) -> Self
    where
        T: k0_list_inner,
    {
        k0_list {
            n: array.len() as J,
            G0: Box::new(array),
        }
    }
}

impl Clone for k0_list {
    fn clone(&self) -> Self {
        // Deref happens here
        k0_list {
            n: self.n,
            G0: self.G0.clone_box(),
        }
    }
}

//%% K %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl K {
    // Constructor //----------------------------/

    /// Base constructor of `K`.
    pub(crate) fn new(qtype: i8, attribute: i8, inner: k0_inner) -> Self {
        K(Box::new(k0 {
            qtype,
            attribute,
            value: inner,
        }))
    }

    /// Construct q bool from `bool`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_bool_false = K::new_bool(false);
    /// assert_eq!(format!("{}", q_bool_false), String::from("0b"));
    /// ```
    pub fn new_bool(boolean: bool) -> Self {
        K::new(
            qtype::BOOL_ATOM,
            qattribute::NONE,
            k0_inner::byte(boolean as u8),
        )
    }

    /// Construct q GUID from `[u8; 16]`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_guid = K::new_guid([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
    /// assert_eq!(
    ///     format!("{}", q_guid),
    ///     String::from("01020304-0506-0708-090a-0b0c0d0e0f10")
    /// );
    /// ```
    pub fn new_guid(guid: [G; 16]) -> Self {
        K::new(qtype::GUID_ATOM, qattribute::NONE, k0_inner::guid(guid))
    }

    /// Construct q byte from `u8`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_byte = K::new_byte(0x9e);
    /// assert_eq!(format!("{}", q_byte), String::from("0x9e"));
    /// ```
    pub fn new_byte(byte: u8) -> Self {
        K::new(qtype::BYTE_ATOM, qattribute::NONE, k0_inner::byte(byte))
    }

    /// Construct q short from `i16`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_short = K::new_short(17);
    /// assert_eq!(format!("{}", q_short), String::from("17h"));
    /// ```
    pub fn new_short(short: i16) -> Self {
        K::new(qtype::SHORT_ATOM, qattribute::NONE, k0_inner::short(short))
    }

    /// Construct q int from `i32`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_int = K::new_int(-256);
    /// assert_eq!(format!("{}", q_int), String::from("-256i"));
    /// ```
    pub fn new_int(int: i32) -> Self {
        K::new(qtype::INT_ATOM, qattribute::NONE, k0_inner::int(int))
    }

    /// Construct q long from `i64`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_long = K::new_long(86400000000000);
    /// assert_eq!(format!("{}", q_long), String::from("86400000000000"));
    /// ```
    pub fn new_long(long: i64) -> Self {
        K::new(qtype::LONG_ATOM, qattribute::NONE, k0_inner::long(long))
    }

    /// Construct q real from `f32`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_real = K::new_real(0.25);
    /// assert_eq!(format!("{:.2}", q_real), String::from("0.25e"));
    /// ```
    pub fn new_real(real: f32) -> Self {
        K::new(qtype::REAL_ATOM, qattribute::NONE, k0_inner::real(real))
    }

    /// Construct q float from `f64`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_float = K::new_float(113.0456);
    /// assert_eq!(format!("{:.7}", q_float), String::from("113.0456000"));
    /// ```
    pub fn new_float(float: f64) -> Self {
        K::new(qtype::FLOAT_ATOM, qattribute::NONE, k0_inner::float(float))
    }

    /// Construct q char from `char`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_char = K::new_char('r');
    /// assert_eq!(format!("{}", q_char), String::from("\"r\""));
    /// ```
    pub fn new_char(character: char) -> Self {
        K::new(
            qtype::CHAR,
            qattribute::NONE,
            k0_inner::byte(character as G),
        )
    }

    /// Construct q symbol from `String`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_symbol = K::new_symbol(String::from("Jordan"));
    /// assert_eq!(format!("{}", q_symbol), String::from("`Jordan"));
    /// ```
    pub fn new_symbol(symbol: String) -> Self {
        K::new(
            qtype::SYMBOL_ATOM,
            qattribute::NONE,
            k0_inner::symbol(symbol),
        )
    }

    /// Construct q timestamp from `DateTime<Utc>`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_timestamp = K::new_timestamp(
    ///     NaiveDate::from_ymd_opt(2019, 5, 9)
    ///         .unwrap()
    ///         .and_hms_nano_opt(0, 39, 2, 194756)
    ///         .unwrap()
    ///         .and_local_timezone(Utc)
    ///         .unwrap(),
    /// );
    /// assert_eq!(
    ///     format!("{}", q_timestamp),
    ///     String::from("2019.05.09D00:39:02.000194756")
    /// );
    /// ```
    pub fn new_timestamp(timestamp: DateTime<Utc>) -> Self {
        K::new(
            qtype::TIMESTAMP_ATOM,
            qattribute::NONE,
            k0_inner::long(datetime_to_q_timestamp(timestamp)),
        )
    }

    /// Construct q month from `Date<Utc>`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_month = K::new_month(NaiveDate::from_ymd_opt(2019, 12, 15).unwrap());
    /// assert_eq!(format!("{}", q_month), String::from("2019.12m"));
    /// ```
    pub fn new_month(month: NaiveDate) -> Self {
        K::new(
            qtype::MONTH_ATOM,
            qattribute::NONE,
            k0_inner::int(date_to_q_month(month)),
        )
    }

    /// Construct q date from `Date<Utc>`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_date = K::new_date(NaiveDate::from_ymd_opt(2012, 3, 12).unwrap());
    /// assert_eq!(format!("{}", q_date), String::from("2012.03.12"));
    /// ```
    pub fn new_date(date: NaiveDate) -> Self {
        K::new(
            qtype::DATE_ATOM,
            qattribute::NONE,
            k0_inner::int(date_to_q_date(date)),
        )
    }

    /// Construct q datetime from `DateTime<Utc>`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_datetime = K::new_datetime(
    ///     NaiveDate::from_ymd_opt(2013, 1, 10)
    ///         .unwrap()
    ///         .and_hms_milli_opt(0, 9, 50, 38)
    ///         .unwrap()
    ///         .and_local_timezone(Utc)
    ///         .unwrap(),
    /// );
    /// assert_eq!(
    ///     format!("{}", q_datetime),
    ///     String::from("2013.01.10T00:09:50.038")
    /// );
    /// ```
    pub fn new_datetime(datetime: DateTime<Utc>) -> Self {
        K::new(
            qtype::DATETIME_ATOM,
            qattribute::NONE,
            k0_inner::float(datetime_to_q_datetime(datetime)),
        )
    }

    /// Construct q timespan from `Duration`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_timespan = K::new_timespan(Duration::nanoseconds(102899277539844));
    /// assert_eq!(
    ///     format!("{}", q_timespan),
    ///     String::from("1D04:34:59.277539844")
    /// );
    /// ```
    pub fn new_timespan(duration: Duration) -> Self {
        K::new(
            qtype::TIMESPAN_ATOM,
            qattribute::NONE,
            k0_inner::long(duration.num_nanoseconds().expect("duration overflow")),
        )
    }

    /// Construct q minute from `Duration`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_minute = K::new_minute(Duration::minutes(99));
    /// assert_eq!(format!("{}", q_minute), String::from("01:39"));
    /// ```
    pub fn new_minute(minute: Duration) -> Self {
        K::new(
            qtype::MINUTE_ATOM,
            qattribute::NONE,
            k0_inner::int(minute.num_minutes() as i32),
        )
    }

    /// Construct q second from `Duration`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_second = K::new_second(Duration::seconds(3702));
    /// assert_eq!(format!("{}", q_second), String::from("01:01:42"));
    /// ```
    pub fn new_second(second: Duration) -> Self {
        K::new(
            qtype::SECOND_ATOM,
            qattribute::NONE,
            k0_inner::int(second.num_seconds() as i32),
        )
    }

    /// Construct q time from `Duration`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_time = K::new_time(Duration::milliseconds(27843489));
    /// assert_eq!(format!("{}", q_time), String::from("07:44:03.489"));
    /// ```
    pub fn new_time(time: Duration) -> Self {
        K::new(
            qtype::TIME_ATOM,
            qattribute::NONE,
            k0_inner::int(time.num_milliseconds() as i32),
        )
    }

    /// Construct q bool list from `Vec<bool>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_bool_list = K::new_bool_list(vec![true, false, true], qattribute::NONE);
    /// assert_eq!(format!("{}", q_bool_list), String::from("101b"));
    /// ```
    pub fn new_bool_list(list: Vec<bool>, attribute: i8) -> Self {
        let array = list
            .into_iter()
            .map(|element| match element {
                true => 1,
                false => 0,
            })
            .collect::<Vec<G>>();

        K::new(
            qtype::BOOL_LIST,
            attribute,
            k0_inner::list(k0_list::new(array)),
        )
    }

    /// Construct q GUID list from `Vec<U>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_guid_list = K::new_guid_list(
    ///     vec![
    ///         [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    ///         [
    ///             240, 241, 242, 243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255,
    ///         ],
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_guid_list),
    ///     String::from(
    ///         "00010203-0405-0607-0809-0a0b0c0d0e0f f0f1f2f3-f4f5-f6f7-f8f9-fafbfcfdfeff"
    ///     )
    /// );
    /// ```
    pub fn new_guid_list(list: Vec<[u8; 16]>, attribute: i8) -> Self {
        K::new(
            qtype::GUID_LIST,
            attribute,
            k0_inner::list(k0_list::new(list)),
        )
    }

    /// Construct q byte list from `Vec<G>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_byte_list = K::new_byte_list(vec![7, 12, 21, 144], qattribute::NONE);
    /// assert_eq!(format!("{}", q_byte_list), String::from("0x070c1590"));
    /// ```
    pub fn new_byte_list(list: Vec<u8>, attribute: i8) -> Self {
        K::new(
            qtype::BYTE_LIST,
            attribute,
            k0_inner::list(k0_list::new(list)),
        )
    }

    /// Construct q short list from `Vec<H>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_short_list =
    ///     K::new_short_list(vec![qnull::SHORT, -7, 12, 21, 144], qattribute::SORTED);
    /// assert_eq!(
    ///     format!("{}", q_short_list),
    ///     String::from("`s#0N -7 12 21 144h")
    /// );
    /// ```
    pub fn new_short_list(list: Vec<i16>, attribute: i8) -> Self {
        K::new(
            qtype::SHORT_LIST,
            attribute,
            k0_inner::list(k0_list::new(list)),
        )
    }

    /// Construct q int list from `Vec<I>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_int_list = K::new_int_list(
    ///     vec![-10000, -10000, 21, 21, qinf::INT, 144000],
    ///     qattribute::PARTED,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_int_list),
    ///     String::from("`p#-10000 -10000 21 21 0W 144000i")
    /// );
    /// ```
    pub fn new_int_list(list: Vec<i32>, attribute: i8) -> Self {
        K::new(
            qtype::INT_LIST,
            attribute,
            k0_inner::list(k0_list::new(list)),
        )
    }

    /// Construct q long list from `Vec<J>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_long_list = K::new_long_list(vec![-86400000000000], qattribute::UNIQUE);
    /// assert_eq!(
    ///     format!("{}", q_long_list),
    ///     String::from("`u#,-86400000000000")
    /// );
    /// ```
    pub fn new_long_list(list: Vec<i64>, attribute: i8) -> Self {
        K::new(
            qtype::LONG_LIST,
            attribute,
            k0_inner::list(k0_list::new(list)),
        )
    }

    /// Construct q real list from `Vec<E>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_real_list = K::new_real_list(vec![30.2, 5.002], qattribute::NONE);
    /// assert_eq!(format!("{:.3}", q_real_list), String::from("30.200 5.002e"));
    /// ```
    pub fn new_real_list(list: Vec<f32>, attribute: i8) -> Self {
        K::new(
            qtype::REAL_LIST,
            attribute,
            k0_inner::list(k0_list::new(list)),
        )
    }

    /// Construct q float list from `Vec<F>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_float_list = K::new_float_list(
    ///     vec![100.23, 0.4268, qnull::FLOAT, 15.882, qninf::FLOAT],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_float_list),
    ///     String::from("100.23 0.4268 0n 15.882 -0w")
    /// );
    /// ```
    pub fn new_float_list(list: Vec<f64>, attribute: i8) -> Self {
        K::new(
            qtype::FLOAT_LIST,
            attribute,
            k0_inner::list(k0_list::new(list)),
        )
    }

    /// Construct q string from `String`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_string = K::new_string(String::from("super"), qattribute::UNIQUE);
    /// assert_eq!(format!("{}", q_string), String::from("`u#\"super\""));
    /// ```
    /// # Note
    /// q string must be accessed with `as_string` or `as_mut_string`.
    pub fn new_string(string: String, attribute: i8) -> Self {
        K::new(qtype::STRING, attribute, k0_inner::symbol(string))
    }

    /// Construct q symbol list from `Vec<String>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_symbol_list = K::new_symbol_list(
    ///     vec![
    ///         String::from("a"),
    ///         String::from("b"),
    ///         String::from("a"),
    ///         String::from("c"),
    ///     ],
    ///     qattribute::GROUPED,
    /// );
    /// assert_eq!(format!("{}", q_symbol_list), String::from("`g#`a`b`a`c"));
    /// ```
    pub fn new_symbol_list(list: Vec<String>, attribute: i8) -> Self {
        K::new(
            qtype::SYMBOL_LIST,
            attribute,
            k0_inner::list(k0_list::new(list)),
        )
    }

    /// Construct q timestamp list from `Vec<DateTime<Utc>>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_timestamp_list = K::new_timestamp_list(
    ///     vec![
    ///         *qnull::TIMESTAMP,
    ///         NaiveDate::from_ymd_opt(2000, 2, 6)
    ///             .unwrap()
    ///             .and_hms_nano_opt(5, 11, 28, 4032)
    ///             .unwrap()
    ///             .and_local_timezone(Utc)
    ///             .unwrap(),
    ///         *qinf::TIMESTAMP,
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_timestamp_list),
    ///     String::from("0N 2000.02.06D05:11:28.000004032 0Wp")
    /// );
    /// ```
    pub fn new_timestamp_list(list: Vec<DateTime<Utc>>, attribute: i8) -> Self {
        let array = list
            .into_iter()
            .map(datetime_to_q_timestamp)
            .collect::<Vec<J>>();
        K::new(
            qtype::TIMESTAMP_LIST,
            attribute,
            k0_inner::list(k0_list::new(array)),
        )
    }

    /// Construct q month list from `Vec<Date<Utc>>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_month_list = K::new_month_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2006, 3, 9).unwrap(),
    ///         NaiveDate::from_ymd_opt(1999, 5, 31).unwrap(),
    ///         qnull::MONTH,
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_month_list),
    ///     String::from("2006.03 1999.05 0Nm")
    /// );
    /// ```
    pub fn new_month_list(list: Vec<NaiveDate>, attribute: i8) -> Self {
        let array = list.into_iter().map(date_to_q_month).collect::<Vec<I>>();
        K::new(
            qtype::MONTH_LIST,
            attribute,
            k0_inner::list(k0_list::new(array)),
        )
    }

    /// Construct q date list from `Vec<Date<Utc>>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_date_list = K::new_date_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2001, 2, 18).unwrap(),
    ///         NaiveDate::from_ymd_opt(2019, 12, 12).unwrap(),
    ///         qinf::DATE,
    ///         NaiveDate::from_ymd_opt(2003, 10, 16).unwrap(),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_date_list),
    ///     String::from("2001.02.18 2019.12.12 0W 2003.10.16")
    /// );
    /// ```
    pub fn new_date_list(list: Vec<NaiveDate>, attribute: i8) -> Self {
        let array = list.into_iter().map(date_to_q_date).collect::<Vec<I>>();
        K::new(
            qtype::DATE_LIST,
            attribute,
            k0_inner::list(k0_list::new(array)),
        )
    }

    /// Construct q datetime list from `Vec<DateTime<Utc>>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_datetime_list = K::new_datetime_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2002, 1, 26)
    ///             .unwrap()
    ///             .and_hms_nano_opt(9, 39, 2, 368376238)
    ///             .unwrap()
    ///             .and_local_timezone(Utc)
    ///             .unwrap(),
    ///         *qinf::DATETIME,
    ///     ],
    ///     qattribute::SORTED,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_datetime_list),
    ///     String::from("`s#2002.01.26T09:39:02.368 0Wz")
    /// );
    /// ```
    pub fn new_datetime_list(list: Vec<DateTime<Utc>>, attribute: i8) -> Self {
        let array = list
            .into_iter()
            .map(datetime_to_q_datetime)
            .collect::<Vec<F>>();
        K::new(
            qtype::DATETIME_LIST,
            attribute,
            k0_inner::list(k0_list::new(array)),
        )
    }

    /// Construct q timespan list from `Vec<Duration>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_timespan_list = K::new_timespan_list(
    ///     vec![
    ///         *qinf::TIMESPAN,
    ///         Duration::nanoseconds(7240514990625504),
    ///         Duration::nanoseconds(-107695363440640000),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_timespan_list),
    ///     String::from("0W 83D19:15:14.990625504 -1246D11:22:43.440640000")
    /// );
    /// ```
    pub fn new_timespan_list(list: Vec<Duration>, attribute: i8) -> Self {
        let array = list
            .into_iter()
            .map(|duration| duration.num_nanoseconds().expect("duration overflow"))
            .collect::<Vec<J>>();
        K::new(
            qtype::TIMESPAN_LIST,
            attribute,
            k0_inner::list(k0_list::new(array)),
        )
    }

    /// Construct q minute list from `Vec<Duration>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_minute_list = K::new_minute_list(
    ///     vec![Duration::minutes(504), Duration::seconds(-100)],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(format!("{}", q_minute_list), String::from("08:24 -00:01"));
    /// ```
    pub fn new_minute_list(list: Vec<Duration>, attribute: i8) -> Self {
        let array = list
            .into_iter()
            .map(|duration| duration.num_minutes() as i32)
            .collect::<Vec<I>>();
        K::new(
            qtype::MINUTE_LIST,
            attribute,
            k0_inner::list(k0_list::new(array)),
        )
    }

    /// Construct q second list from `Vec<Duration>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_second_list = K::new_second_list(
    ///     vec![
    ///         Duration::seconds(-3554),
    ///         *qinf::SECOND,
    ///         Duration::seconds(13744),
    ///         *qninf::SECOND,
    ///         *qnull::SECOND,
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_second_list),
    ///     String::from("-00:59:14 0W 03:49:04 -0W 0Nv")
    /// );
    /// ```
    pub fn new_second_list(list: Vec<Duration>, attribute: i8) -> Self {
        let array = list
            .into_iter()
            .map(|duration| duration.num_seconds() as i32)
            .collect::<Vec<I>>();
        K::new(
            qtype::SECOND_LIST,
            attribute,
            k0_inner::list(k0_list::new(array)),
        )
    }

    /// Construct q time list from `Vec<Duration>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_time_list = K::new_time_list(
    ///     vec![
    ///         Duration::milliseconds(642982),
    ///         Duration::milliseconds(789848),
    ///         *qninf::TIME,
    ///         Duration::milliseconds(58725553),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(
    ///     format!("{}", q_time_list),
    ///     String::from("00:10:42.982 00:13:09.848 -0W 16:18:45.553")
    /// );
    /// ```
    pub fn new_time_list(list: Vec<Duration>, attribute: i8) -> Self {
        let array = list
            .into_iter()
            .map(|duration| duration.num_milliseconds() as i32)
            .collect::<Vec<I>>();
        K::new(
            qtype::TIME_LIST,
            attribute,
            k0_inner::list(k0_list::new(array)),
        )
    }

    /// Construct q compound list from `Vec<K>`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_compound_list = K::new_compound_list(vec![
    ///     K::new_symbol_list(
    ///         vec![
    ///             String::from("Ruby"),
    ///             String::from("Diamond"),
    ///             String::from("Sapphire"),
    ///         ],
    ///         qattribute::UNIQUE,
    ///     ),
    ///     K::new_timestamp(*qnull::TIMESTAMP),
    ///     K::new_long_list(vec![0, 1, 2, qninf::LONG], qattribute::NONE),
    ///     K::new_month_list(
    ///         vec![NaiveDate::from_ymd_opt(2004, 2, 7).unwrap()],
    ///         qattribute::NONE,
    ///     ),
    /// ]);
    /// assert_eq!(
    ///     format!("{}", q_compound_list),
    ///     String::from("(`u#`Ruby`Diamond`Sapphire;0Np;0 1 2 -0W;,2004.02m)")
    /// );
    /// ```
    pub fn new_compound_list(list: Vec<K>) -> Self {
        K::new(
            qtype::COMPOUND_LIST,
            qattribute::NONE,
            k0_inner::list(k0_list::new(list)),
        )
    }

    /// Construct q dictionary from a pair of keys (`K`) and values (`K`).
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let keys = K::new_int_list(vec![20, 30, 40], qattribute::SORTED);
    /// let values = K::new_bool_list(vec![false, false, true], qattribute::NONE);
    /// let q_dictionary = K::new_dictionary(keys, values).unwrap();
    /// assert_eq!(
    ///     format!("{}", q_dictionary),
    ///     String::from("`s#20 30 40i!001b")
    /// );
    /// ```
    /// # Note
    /// This constructor can return an error object whose type is `qtype::ERROR`. In that case the error message can be
    ///  retrieved by [`get_symbol`](#fn.get_symbol).
    pub fn new_dictionary(keys: K, values: K) -> Result<Self> {
        if keys.len() != values.len() {
            Err(Error::length_mismatch(keys.len(), values.len()))
        } else {
            let qtype = if keys.0.attribute == qattribute::SORTED {
                qtype::SORTED_DICTIONARY
            } else {
                qtype::DICTIONARY
            };
            Ok(K::new(
                qtype,
                qattribute::NONE,
                k0_inner::list(k0_list::new(vec![keys, values])),
            ))
        }
    }

    /// Construct q null.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_null = K::new_null();
    /// assert_eq!(format!("{}", q_null), String::from("::"));
    /// ```
    pub fn new_null() -> Self {
        K::new(qtype::NULL, qattribute::NONE, k0_inner::null(()))
    }

    /// Construct q error object.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    ///let q_error=K::new_error(String::from("woops"));
    ///assert_eq!(format!("{}", q_error), String::from("'woops"));
    /// ```
    pub fn new_error(error: String) -> Self {
        K::new(qtype::ERROR, qattribute::NONE, k0_inner::symbol(error))
    }

    // Getter //---------------------------------/

    /// Get underlying `bool` value.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_bool = K::new_bool(true);
    /// assert_eq!(q_bool.get_bool(), Ok(true));
    /// ```
    pub fn get_bool(&self) -> Result<bool> {
        match self.0.qtype {
            qtype::BOOL_ATOM => match self.0.value {
                k0_inner::byte(boolean) => Ok(boolean != 0),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::BOOL_ATOM)),
        }
    }

    /// Get underlying `[u8; 16]` value.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_guid = K::new_guid([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    /// assert_eq!(
    ///     q_guid.get_guid(),
    ///     Ok([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
    /// );
    /// ```
    pub fn get_guid(&self) -> Result<[u8; 16]> {
        match self.0.qtype {
            qtype::GUID_ATOM => match self.0.value {
                k0_inner::guid(guid) => Ok(guid),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::GUID_ATOM)),
        }
    }

    /// Get underlying `u8` value. Compatible types are:
    /// - bool
    /// - byte
    /// - char
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_byte = K::new_byte(0x77);
    /// assert_eq!(q_byte.get_byte(), Ok(0x77));
    /// ```
    pub fn get_byte(&self) -> Result<u8> {
        match self.0.qtype {
            qtype::BOOL_ATOM | qtype::BYTE_ATOM | qtype::CHAR => match self.0.value {
                k0_inner::byte(byte) => Ok(byte),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::BYTE_ATOM)),
        }
    }

    /// Get underlying `i16` value.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_short = K::new_short(-12);
    /// assert_eq!(q_short.get_short(), Ok(-12));
    /// ```
    pub fn get_short(&self) -> Result<i16> {
        match self.0.qtype {
            qtype::SHORT_ATOM => match self.0.value {
                k0_inner::short(short) => Ok(short),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::SHORT_ATOM)),
        }
    }

    /// Get underlying `i32` value. Compatible types are:
    /// - int
    /// - month
    /// - date
    /// - minute
    /// - second
    /// - time
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_int = K::new_int(144000);
    /// assert_eq!(q_int.get_int(), Ok(144000));
    /// ```
    pub fn get_int(&self) -> Result<i32> {
        match self.0.qtype {
            qtype::INT_ATOM
            | qtype::MONTH_ATOM
            | qtype::DATE_ATOM
            | qtype::MINUTE_ATOM
            | qtype::SECOND_ATOM
            | qtype::TIME_ATOM => match self.0.value {
                k0_inner::int(int) => Ok(int),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::INT_ATOM)),
        }
    }

    /// Get underlying `i64` value. Compatible types are:
    /// - long
    /// - timestamp
    /// - timespan
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_long = K::new_long(86400000000000);
    /// assert_eq!(q_long.get_long(), Ok(86400000000000));
    /// ```
    pub fn get_long(&self) -> Result<i64> {
        match self.0.qtype {
            qtype::LONG_ATOM | qtype::TIMESTAMP_ATOM | qtype::TIMESPAN_ATOM => match self.0.value {
                k0_inner::long(long) => Ok(long),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::LONG_ATOM)),
        }
    }

    /// Get underlying `f32` value.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_real = K::new_real(0.25);
    /// assert_eq!(q_real.get_real(), Ok(0.25));
    /// ```
    pub fn get_real(&self) -> Result<f32> {
        match self.0.qtype {
            qtype::REAL_ATOM => match self.0.value {
                k0_inner::real(real) => Ok(real),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::REAL_ATOM)),
        }
    }

    /// Get underlying `i32` value. Compatible types are:
    /// - float
    /// - datetime
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_float = K::new_float(1000.23456);
    /// assert_eq!(q_float.get_float(), Ok(1000.23456));
    /// ```
    pub fn get_float(&self) -> Result<f64> {
        match self.0.qtype {
            qtype::FLOAT_ATOM | qtype::DATETIME_ATOM => match self.0.value {
                k0_inner::float(float) => Ok(float),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::FLOAT_ATOM)),
        }
    }

    /// Get underlying `char` value.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_char = K::new_char('C');
    /// assert_eq!(q_char.get_char(), Ok('C'));
    /// ```
    pub fn get_char(&self) -> Result<char> {
        match self.0.qtype {
            qtype::CHAR => match self.0.value {
                k0_inner::byte(ch) => Ok(ch as char),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::CHAR)),
        }
    }

    /// Get underlying `i32` value.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// let q_symbol = K::new_symbol(String::from("Rust"));
    /// assert_eq!(q_symbol.get_symbol(), Ok("Rust"));
    /// ```
    pub fn get_symbol(&self) -> Result<&str> {
        match self.0.qtype {
            qtype::SYMBOL_ATOM => match &self.0.value {
                k0_inner::symbol(symbol) => Ok(symbol),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::SYMBOL_ATOM)),
        }
    }

    /// Get underlying timestamp value as `DateTime<Utc>`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_timestamp = K::new_timestamp(
    ///     NaiveDate::from_ymd_opt(2001, 9, 15)
    ///         .unwrap()
    ///         .and_hms_nano_opt(4, 2, 30, 37204)
    ///         .unwrap()
    ///         .and_local_timezone(Utc)
    ///         .unwrap(),
    /// );
    /// assert_eq!(
    ///     q_timestamp.get_timestamp(),
    ///     Ok(NaiveDate::from_ymd_opt(2001, 9, 15)
    ///         .unwrap()
    ///         .and_hms_nano_opt(4, 2, 30, 37204)
    ///         .unwrap()
    ///         .and_local_timezone(Utc)
    ///         .unwrap())
    /// );
    /// ```
    pub fn get_timestamp(&self) -> Result<DateTime<Utc>> {
        match self.0.qtype {
            qtype::TIMESTAMP_ATOM => match self.0.value {
                k0_inner::long(nanos) => Ok(q_timestamp_to_datetime(nanos)),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::TIMESTAMP_ATOM)),
        }
    }

    /// Get underlying month value as `Date<Utc>`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_month = K::new_month(NaiveDate::from_ymd_opt(2007, 8, 30).unwrap());
    /// assert_eq!(
    ///     q_month.get_month(),
    ///     Ok(NaiveDate::from_ymd_opt(2007, 8, 1).unwrap())
    /// );
    /// ```
    pub fn get_month(&self) -> Result<NaiveDate> {
        match self.0.qtype {
            qtype::MONTH_ATOM => match self.0.value {
                k0_inner::int(months) => Ok(q_month_to_date(months)),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::MONTH_ATOM)),
        }
    }

    /// Get underlying date value as `Date<Utc>`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_date = K::new_date(NaiveDate::from_ymd_opt(2000, 5, 10).unwrap());
    /// assert_eq!(
    ///     q_date.get_date(),
    ///     Ok(NaiveDate::from_ymd_opt(2000, 5, 10).unwrap())
    /// );
    /// ```
    pub fn get_date(&self) -> Result<NaiveDate> {
        match self.0.qtype {
            qtype::DATE_ATOM => match self.0.value {
                k0_inner::int(days) => Ok(q_date_to_date(days)?),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::DATE_ATOM)),
        }
    }

    /// Get underlying datetime value as `DateTime<Utc>`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_datetime = K::new_datetime(
    ///     NaiveDate::from_ymd_opt(2011, 4, 7)
    ///         .unwrap()
    ///         .and_hms_milli_opt(19, 5, 41, 385)
    ///         .unwrap()
    ///         .and_local_timezone(Utc)
    ///         .unwrap(),
    /// );
    /// assert_eq!(
    ///     q_datetime.get_datetime(),
    ///     Ok(NaiveDate::from_ymd_opt(2011, 4, 7)
    ///         .unwrap()
    ///         .and_hms_milli_opt(19, 5, 41, 385)
    ///         .unwrap()
    ///         .and_local_timezone(Utc)
    ///         .unwrap())
    /// );
    /// ```
    pub fn get_datetime(&self) -> Result<DateTime<Utc>> {
        match self.0.qtype {
            qtype::DATETIME_ATOM => match self.0.value {
                k0_inner::float(days) => Ok(q_datetime_to_datetime(days)),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::DATETIME_ATOM)),
        }
    }

    /// Get underlying timespan value as `Duration`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_timespan = K::new_timespan(Duration::nanoseconds(131400000000000));
    /// assert_eq!(
    ///     q_timespan.get_timespan(),
    ///     Ok(Duration::nanoseconds(131400000000000))
    /// );
    /// ```
    pub fn get_timespan(&self) -> Result<Duration> {
        match self.0.qtype {
            qtype::TIMESPAN_ATOM => match self.0.value {
                k0_inner::long(nanos) => Ok(q_timespan_to_duration(nanos)),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::TIMESPAN_ATOM)),
        }
    }

    /// Get underlying minute value as `Duration`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_minute = K::new_minute(Duration::minutes(30));
    /// assert_eq!(q_minute.get_minute(), Ok(Duration::minutes(30)));
    /// ```
    pub fn get_minute(&self) -> Result<Duration> {
        match self.0.qtype {
            qtype::MINUTE_ATOM => match self.0.value {
                k0_inner::int(minutes) => Ok(q_minute_to_duration(minutes)),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::MINUTE_ATOM)),
        }
    }

    /// Get underlying second value as `Duration`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_second = K::new_second(Duration::seconds(30));
    /// assert_eq!(q_second.get_second(), Ok(Duration::seconds(30)));
    /// ```
    pub fn get_second(&self) -> Result<Duration> {
        match self.0.qtype {
            qtype::SECOND_ATOM => match self.0.value {
                k0_inner::int(seconds) => Ok(q_second_to_duration(seconds)),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::SECOND_ATOM)),
        }
    }

    /// Get underlying time value as `Duration`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let q_time = K::new_time(Duration::milliseconds(3000));
    /// assert_eq!(q_time.get_time(), Ok(Duration::milliseconds(3000)));
    /// ```
    pub fn get_time(&self) -> Result<Duration> {
        match self.0.qtype {
            qtype::TIME_ATOM => match self.0.value {
                k0_inner::int(millis) => Ok(q_time_to_duration(millis)),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::TIME_ATOM)),
        }
    }

    /// Get underlying immutable dictionary (flipped table) of table type as `K`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let headers = K::new_symbol_list(
    ///     vec![String::from("fruit"), String::from("price")],
    ///     qattribute::NONE,
    /// );
    /// let columns = K::new_compound_list(vec![
    ///     K::new_symbol_list(
    ///         vec![
    ///             String::from("strawberry"),
    ///             String::from("orange"),
    ///             qnull::SYMBOL,
    ///         ],
    ///         qattribute::PARTED,
    ///     ),
    ///     K::new_float_list(vec![2.5, 1.25, 117.8], qattribute::NONE),
    /// ]);
    /// let q_dictionary = K::new_dictionary(headers, columns).unwrap();
    /// let q_table = q_dictionary.flip().unwrap();
    /// assert_eq!(
    ///     format!("{}", q_table.get_dictionary().unwrap()),
    ///     String::from("`fruit`price!(`p#`strawberry`orange`;2.5 1.25 117.8)")
    /// );
    /// ```
    pub fn get_dictionary(&self) -> Result<&K> {
        match self.0.qtype {
            qtype::TABLE => match &self.0.value {
                k0_inner::table(dictionary) => Ok(dictionary),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::TABLE)),
        }
    }

    /// Get underlying mutable dictionary (flipped table) of table type as `K`.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let headers = K::new_symbol_list(
    ///     vec![String::from("fruit"), String::from("price")],
    ///     qattribute::NONE,
    /// );
    /// let columns = K::new_compound_list(vec![
    ///     K::new_symbol_list(
    ///         vec![
    ///             String::from("strawberry"),
    ///             String::from("orange"),
    ///             qnull::SYMBOL,
    ///         ],
    ///         qattribute::PARTED,
    ///     ),
    ///     K::new_float_list(vec![2.5, 1.25, 117.8], qattribute::NONE),
    /// ]);
    /// let q_dictionary = K::new_dictionary(headers, columns).unwrap();
    /// let mut q_table = q_dictionary.flip().unwrap();
    /// let inner = q_table.get_mut_dictionary().unwrap();
    ///
    /// // modify inner dictionary
    /// inner.as_mut_vec::<K>().unwrap()[0]
    ///     .push(&String::from("color"))
    ///     .unwrap();
    /// inner.as_mut_vec::<K>().unwrap()[1]
    ///     .push(&K::new_string(String::from("RO"), qattribute::NONE))
    ///     .unwrap();
    ///
    /// assert_eq!(
    ///     format!("{}", q_table),
    ///     String::from("+`fruit`price`color!(`p#`strawberry`orange`;2.5 1.25 117.8;\"RO\")")
    /// );
    /// ```
    pub fn get_mut_dictionary(&mut self) -> Result<&mut K> {
        match self.0.qtype {
            qtype::TABLE => match &mut self.0.value {
                k0_inner::table(dictionary) => Ok(dictionary),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::TABLE)),
        }
    }

    /// Get underlying error value as `String`.
    /// # Example
    /// ```
    /// use kdbplus::ipc::*;
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<()> {
    ///     let mut socket = QStream::connect(ConnectionMethod::TCP, "localhost", 5000, "kdbuser:pass")
    ///         .await
    ///         .expect("Failed to connect");
    ///     let result = socket.send_sync_message(&"1+`a").await?;
    ///     assert_eq!(result.get_error_string(), Ok("type"));
    ///     Ok(())
    /// }
    /// ```
    pub fn get_error_string(&self) -> Result<&str> {
        match self.0.qtype {
            qtype::ERROR => match &self.0.value {
                k0_inner::symbol(error) => Ok(error),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::ERROR)),
        }
    }

    /// Get underlying immutable `String` value.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let string = K::new_string(String::from("something"), qattribute::NONE);
    /// assert_eq!(string.as_string().unwrap(), "something");
    /// ```
    pub fn as_string(&self) -> Result<&str> {
        match self.0.qtype {
            qtype::STRING => match &self.0.value {
                k0_inner::symbol(string) => Ok(string),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::STRING)),
        }
    }

    /// Get underlying mutable `String` value.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut string = K::new_string(String::from("something"), qattribute::NONE);
    /// string.as_mut_string().unwrap().push('!');
    /// assert_eq!(format!("{}", string), String::from("\"something!\""));
    /// ```
    pub fn as_mut_string(&mut self) -> Result<&mut String> {
        match self.0.qtype {
            qtype::STRING => match &mut self.0.value {
                k0_inner::symbol(string) => Ok(string),
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast(self.0.qtype, qtype::STRING)),
        }
    }

    /// Get the underlying mutable vector. If the specified type is wrong, it returns an empty vector.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut timestamp_list = K::new_timestamp_list(
    ///     vec![
    ///         Utc.ymd(2018, 2, 18).and_hms_nano(4, 0, 0, 100),
    ///         Utc.ymd(2019, 12, 3).and_hms_nano(4, 5, 10, 3456),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// timestamp_list
    ///     .as_mut_vec::<J>()
    ///     .unwrap()
    ///     .push(682184439000046395);
    /// assert_eq!(format!("{}", timestamp_list), String::from("2018.02.18D04:00:00.000000100 2019.12.03D04:05:10.000003456 2021.08.13D15:40:39.000046395"));
    /// ```
    pub fn as_mut_vec<T>(&mut self) -> Result<&mut Vec<T>>
    where
        T: 'static,
    {
        match self.0.qtype {
            qtype::COMPOUND_LIST
            | qtype::BOOL_LIST
            | qtype::GUID_LIST
            | qtype::BYTE_LIST
            | qtype::SHORT_LIST
            | qtype::INT_LIST
            | qtype::LONG_LIST
            | qtype::REAL_LIST
            | qtype::FLOAT_LIST
            | qtype::SYMBOL_LIST
            | qtype::TIMESTAMP_LIST
            | qtype::MONTH_LIST
            | qtype::DATE_LIST
            | qtype::DATETIME_LIST
            | qtype::TIMESPAN_LIST
            | qtype::MINUTE_LIST
            | qtype::SECOND_LIST
            | qtype::TIME_LIST
            | qtype::DICTIONARY
            | qtype::SORTED_DICTIONARY => match &mut self.0.value {
                k0_inner::list(list) => match list.G0.as_any_mut().downcast_mut::<Vec<T>>() {
                    Some(vector) => Ok(vector),
                    _ => Err(Error::invalid_cast_list(self.0.qtype)),
                },
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast_list(self.0.qtype)),
        }
    }

    /// Get the underlying immutable vector. If the specified type is wrong, it returns an empty vector.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    ///let bool_list=K::new_bool_list(vec![true, false], qattribute::UNIQUE);
    ///assert_eq!(*bool_list.as_vec::<G>().unwrap(), vec![1_u8, 0]);
    /// ```
    pub fn as_vec<T>(&self) -> Result<&Vec<T>>
    where
        T: 'static,
    {
        match self.0.qtype {
            qtype::COMPOUND_LIST
            | qtype::BOOL_LIST
            | qtype::GUID_LIST
            | qtype::BYTE_LIST
            | qtype::SHORT_LIST
            | qtype::INT_LIST
            | qtype::LONG_LIST
            | qtype::REAL_LIST
            | qtype::FLOAT_LIST
            | qtype::SYMBOL_LIST
            | qtype::TIMESTAMP_LIST
            | qtype::MONTH_LIST
            | qtype::DATE_LIST
            | qtype::DATETIME_LIST
            | qtype::TIMESPAN_LIST
            | qtype::MINUTE_LIST
            | qtype::SECOND_LIST
            | qtype::TIME_LIST
            | qtype::DICTIONARY
            | qtype::SORTED_DICTIONARY => match &self.0.value {
                k0_inner::list(list) => match list.G0.as_any().downcast_ref::<Vec<T>>() {
                    Some(vector) => Ok(vector),
                    _ => Err(Error::invalid_cast_list(self.0.qtype)),
                },
                _ => unreachable!(),
            },
            _ => Err(Error::invalid_cast_list(self.0.qtype)),
        }
    }

    /// Get an immutable column of a table with a specified name.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let header = K::new_symbol_list(
    ///     vec![String::from("time"), String::from("sym")],
    ///     qattribute::NONE,
    /// );
    /// let time_column = K::new_timestamp_list(
    ///     vec![
    ///         Utc.ymd(2019, 4, 10).and_hms_nano(3, 19, 48, 1234),
    ///         Utc.ymd(2019, 4, 10).and_hms_nano(3, 21, 30, 948532),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let sym_column = K::new_symbol_list(
    ///     vec![String::from("eggplant"), String::from("tomato")],
    ///     qattribute::NONE,
    /// );
    /// let table = K::new_dictionary(header, K::new_compound_list(vec![time_column, sym_column]))
    ///     .unwrap()
    ///     .flip()
    ///     .unwrap();
    /// let syms = table.get_column("sym").unwrap();
    /// println!("syms: {}", syms);
    /// ```
    pub fn get_column<T>(&self, column: T) -> Result<&K>
    where
        T: ToString,
    {
        match self.0.qtype {
            qtype::TABLE => {
                let dictionary = self.get_dictionary().unwrap().as_vec::<K>().unwrap();
                match dictionary[0]
                    .as_vec::<S>()
                    .unwrap()
                    .iter()
                    .position(|name| *name == column.to_string())
                {
                    // It is assured that value is a compound list because this is a table
                    Some(index) => Ok(&dictionary[1].as_vec::<K>().unwrap()[index]),
                    _ => Err(Error::no_such_column(column.to_string())),
                }
            }
            qtype::DICTIONARY => {
                let key_value = self.as_vec::<K>().unwrap();
                if key_value[0].0.qtype == qtype::TABLE {
                    // Keyed table
                    if let Ok(found_column) = key_value[0].get_column(column.to_string()) {
                        // Found in key table
                        Ok(found_column)
                    } else if let Ok(found_column) = key_value[1].get_column(column.to_string()) {
                        // Found in value table
                        Ok(found_column)
                    } else {
                        Err(Error::no_such_column(column.to_string()))
                    }
                } else {
                    // Not a keyed table
                    Err(Error::invalid_operation("get_column", self.0.qtype, None))
                }
            }
            _ => Err(Error::invalid_operation("get_column", self.0.qtype, None)),
        }
    }

    /// Get a mutable column of a table with a specified name.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let header = K::new_symbol_list(
    ///     vec![String::from("time"), String::from("sym")],
    ///     qattribute::NONE,
    /// );
    /// let time_column = K::new_timestamp_list(
    ///     vec![
    ///         Utc.ymd(2019, 4, 10).and_hms_nano(3, 19, 48, 1234),
    ///         Utc.ymd(2019, 4, 10).and_hms_nano(3, 21, 30, 948532),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let sym_column = K::new_symbol_list(
    ///     vec![String::from("eggplant"), String::from("tomato")],
    ///     qattribute::NONE,
    /// );
    /// let mut table =
    ///     K::new_dictionary(header, K::new_compound_list(vec![time_column, sym_column]))
    ///         .unwrap()
    ///         .flip()
    ///         .unwrap();
    /// let mut syms = table.get_mut_column("sym").unwrap();
    /// println!("syms: {}", syms);
    /// let _ = std::mem::replace(
    ///     syms,
    ///     K::new_symbol_list(
    ///         vec![String::from("banana"), String::from("strawberry")],
    ///         qattribute::NONE,
    ///     ),
    /// );
    /// println!("table: {}", table);
    /// ```
    pub fn get_mut_column<T>(&mut self, column: T) -> Result<&mut K>
    where
        T: ToString,
    {
        match self.0.qtype {
            qtype::TABLE => {
                let dictionary = self
                    .get_mut_dictionary()
                    .unwrap()
                    .as_mut_vec::<K>()
                    .unwrap();
                match dictionary[0]
                    .as_vec::<S>()
                    .unwrap()
                    .iter()
                    .position(|name| *name == column.to_string())
                {
                    // It is assured that value is a compound list because this is a table
                    Some(index) => Ok(&mut dictionary[1].as_mut_vec::<K>().unwrap()[index]),
                    _ => Err(Error::no_such_column(column.to_string())),
                }
            }
            qtype::DICTIONARY => {
                let key_value = self.as_vec::<K>().unwrap();
                if key_value[0].0.qtype == qtype::TABLE {
                    // Keyed table
                    // Search from key table
                    let mut dictionary = key_value[0]
                        .get_dictionary()
                        .unwrap()
                        .as_vec::<K>()
                        .unwrap();
                    if let Some(index) = dictionary[0]
                        .as_vec::<S>()
                        .unwrap()
                        .iter()
                        .position(|name| *name == column.to_string())
                    {
                        // It is assured that value is a compound list because this is a table
                        return Ok(&mut self.as_mut_vec::<K>().unwrap()[0]
                            .get_mut_dictionary()
                            .unwrap()
                            .as_mut_vec::<K>()
                            .unwrap()[1]
                            .as_mut_vec::<K>()
                            .unwrap()[index]);
                    }
                    // Search from value table
                    dictionary = key_value[1]
                        .get_dictionary()
                        .unwrap()
                        .as_vec::<K>()
                        .unwrap();
                    if let Some(index) = dictionary[0]
                        .as_vec::<S>()
                        .unwrap()
                        .iter()
                        .position(|name| *name == column.to_string())
                    {
                        // It is assured that value is a compound list because this is a table
                        Ok(&mut self.as_mut_vec::<K>().unwrap()[1]
                            .get_mut_dictionary()
                            .unwrap()
                            .as_mut_vec::<K>()
                            .unwrap()[1]
                            .as_mut_vec::<K>()
                            .unwrap()[index])
                    } else {
                        Err(Error::no_such_column(column.to_string()))
                    }
                } else {
                    // Not a keyed table
                    Err(Error::invalid_operation(
                        "get_mut_column",
                        self.0.qtype,
                        None,
                    ))
                }
            }
            _ => Err(Error::invalid_operation(
                "get_mut_column",
                self.0.qtype,
                None,
            )),
        }
    }

    /// Get a type of q object.
    /// # Example
    /// ```
    /// use kdbplus::*;
    /// use kdbplus::ipc::*;
    ///
    /// let q_int = K::new_int(12);
    /// assert_eq!(q_int.get_type(), qtype::INT_ATOM);
    /// ```
    pub fn get_type(&self) -> i8 {
        self.0.qtype
    }

    /// Get an attribute of q object.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let timestamp_list = K::new_timestamp_list(
    ///     vec![
    ///         Utc.ymd(2021, 3, 9).and_hms_nano(12, 5, 40, 67824),
    ///         Utc.ymd(2021, 3, 13).and_hms_nano(5, 47, 2, 260484387),
    ///     ],
    ///     qattribute::SORTED,
    /// );
    /// assert_eq!(timestamp_list.get_attribute(), qattribute::SORTED);
    /// ```
    pub fn get_attribute(&self) -> i8 {
        self.0.attribute
    }

    // Setter //---------------------------------/

    /// Set an attribute to the underlying q object.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut timestamp_list = K::new_timestamp_list(
    ///     vec![Utc.ymd(2021, 3, 9).and_hms_nano(12, 5, 40, 67824)],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(timestamp_list.get_attribute(), qattribute::NONE);
    /// // Push timestamp
    /// timestamp_list
    ///     .push(&Utc.ymd(2021, 3, 13).and_hms_nano(5, 47, 2, 260484387))
    ///     .unwrap();
    /// timestamp_list.set_attribute(qattribute::SORTED);
    /// assert_eq!(timestamp_list.get_attribute(), qattribute::SORTED);
    /// ```
    /// # Note
    /// The validity of the attribute is not checked. For example, if you set a sorted attribute to
    ///  an unsorted list, it does not return an error. It will fail in q process.
    pub fn set_attribute(&mut self, attribute: i8) {
        self.0.attribute = attribute;
    }

    // Push/Pop //-------------------------------/

    /// Increment `n` of `k0_list`.
    fn increment(&mut self) {
        match &mut self.0.value {
            k0_inner::list(list) => list.n += 1,
            _ => unreachable!(),
        }
    }

    /// Decrement `n` of `k0_list`.
    fn decrement(&mut self) {
        match &mut self.0.value {
            k0_inner::list(list) => list.n -= 1,
            _ => unreachable!(),
        }
    }

    /// Add an element to the tail of the underlying list.
    /// # Parameters
    /// - `element`: An element to insert. The type needs to be a one used for atom constructor `K::new_*`. For example,
    ///   int element must be a `i32` type and timestamp element must be a `DateTime<Utc>` type.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut symbol_list = K::new_symbol_list(vec![String::from("first")], qattribute::NONE);
    /// symbol_list.push(&String::from("second")).unwrap();
    /// if let Err(error) = symbol_list.push(&12) {
    ///     eprintln!("Oh no!! {}", error);
    /// }
    /// assert_eq!(
    ///     *symbol_list.as_vec::<S>().unwrap(),
    ///     vec![String::from("first"), String::from("second")]
    /// );
    ///
    /// let mut string_list = K::new_compound_list(vec![K::new_string(
    ///     String::from("string"),
    ///     qattribute::NONE,
    /// )]);
    /// string_list.push(&K::new_bool(false)).unwrap();
    /// assert_eq!(format!("{}", string_list), String::from("(\"string\";0b)"));
    /// ```
    pub fn push(&mut self, element: &dyn Any) -> Result<()> {
        match self.0.qtype {
            qtype::BOOL_LIST => {
                if let Some(boolean) = element.downcast_ref::<bool>() {
                    self.increment();
                    self.as_mut_vec::<G>().unwrap().push(*boolean as u8);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(false, qtype::BOOL_LIST, "bool"))
                }
            }
            qtype::GUID_LIST => {
                if let Some(guid) = element.downcast_ref::<U>() {
                    self.increment();
                    self.as_mut_vec::<U>().unwrap().push(*guid);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::GUID_LIST,
                        "[u8; 16]",
                    ))
                }
            }
            qtype::BYTE_LIST => {
                if let Some(byte) = element.downcast_ref::<u8>() {
                    self.increment();
                    self.as_mut_vec::<G>().unwrap().push(*byte);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(false, qtype::BYTE_LIST, "u8"))
                }
            }
            qtype::SHORT_LIST => {
                if let Some(short) = element.downcast_ref::<i16>() {
                    self.increment();
                    self.as_mut_vec::<H>().unwrap().push(*short);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(false, qtype::SHORT_LIST, "i16"))
                }
            }
            qtype::INT_LIST => {
                if let Some(int) = element.downcast_ref::<i32>() {
                    self.increment();
                    self.as_mut_vec::<I>().unwrap().push(*int);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::INT_LIST,
                        "[u8; 16]",
                    ))
                }
            }
            qtype::LONG_LIST => {
                if let Some(long) = element.downcast_ref::<i64>() {
                    self.increment();
                    self.as_mut_vec::<J>().unwrap().push(*long);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(false, qtype::LONG_LIST, "i64"))
                }
            }
            qtype::REAL_LIST => {
                if let Some(real) = element.downcast_ref::<f32>() {
                    self.increment();
                    self.as_mut_vec::<E>().unwrap().push(*real);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(false, qtype::FLOAT_LIST, "f32"))
                }
            }
            qtype::FLOAT_LIST => {
                if let Some(float) = element.downcast_ref::<f64>() {
                    self.increment();
                    self.as_mut_vec::<F>().unwrap().push(*float);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(false, qtype::FLOAT_LIST, "f64"))
                }
            }
            qtype::STRING => {
                if let Some(ch) = element.downcast_ref::<char>() {
                    self.as_mut_string().unwrap().push(*ch);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(false, qtype::STRING, "char"))
                }
            }
            qtype::SYMBOL_LIST => {
                if let Some(symbol) = element.downcast_ref::<String>() {
                    self.increment();
                    self.as_mut_vec::<S>().unwrap().push(symbol.clone());
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::SYMBOL_LIST,
                        "String",
                    ))
                }
            }
            qtype::TIMESTAMP_LIST => {
                if let Some(timestamp) = element.downcast_ref::<DateTime<Utc>>() {
                    self.increment();
                    self.as_mut_vec::<J>()
                        .unwrap()
                        .push(datetime_to_q_timestamp(*timestamp));
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::TIMESTAMP_LIST,
                        "DateTime<Utc>",
                    ))
                }
            }
            qtype::MONTH_LIST => {
                if let Some(month) = element.downcast_ref::<NaiveDate>() {
                    self.increment();
                    self.as_mut_vec::<I>()
                        .unwrap()
                        .push(date_to_q_month(*month));
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::MONTH_LIST,
                        "Date<Utc>",
                    ))
                }
            }
            qtype::DATE_LIST => {
                if let Some(date) = element.downcast_ref::<NaiveDate>() {
                    self.increment();
                    self.as_mut_vec::<I>().unwrap().push(date_to_q_date(*date));
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::DATE_LIST,
                        "Date<Utc>",
                    ))
                }
            }
            qtype::DATETIME_LIST => {
                if let Some(datetime) = element.downcast_ref::<DateTime<Utc>>() {
                    self.increment();
                    self.as_mut_vec::<F>()
                        .unwrap()
                        .push(datetime_to_q_datetime(*datetime));
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::DATETIME_LIST,
                        "DateTime<Utc>",
                    ))
                }
            }
            qtype::TIMESPAN_LIST => {
                if let Some(timespan) = element.downcast_ref::<Duration>() {
                    self.increment();
                    self.as_mut_vec::<J>()
                        .unwrap()
                        .push(timespan.num_nanoseconds().expect("duration overflow"));
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::TIMESPAN_LIST,
                        "Duration",
                    ))
                }
            }
            qtype::MINUTE_LIST => {
                if let Some(minute) = element.downcast_ref::<Duration>() {
                    self.increment();
                    self.as_mut_vec::<I>()
                        .unwrap()
                        .push(minute.num_minutes() as i32);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::MINUTE_LIST,
                        "Duration",
                    ))
                }
            }
            qtype::SECOND_LIST => {
                if let Some(second) = element.downcast_ref::<Duration>() {
                    self.increment();
                    self.as_mut_vec::<I>()
                        .unwrap()
                        .push(second.num_seconds() as i32);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::SECOND_LIST,
                        "Duration",
                    ))
                }
            }
            qtype::TIME_LIST => {
                if let Some(time) = element.downcast_ref::<Duration>() {
                    self.increment();
                    self.as_mut_vec::<I>()
                        .unwrap()
                        .push(time.num_milliseconds() as i32);
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::TIME_LIST,
                        "Duration",
                    ))
                }
            }
            qtype::COMPOUND_LIST => {
                if let Some(k) = element.downcast_ref::<K>() {
                    self.increment();
                    self.as_mut_vec::<K>().unwrap().push(k.clone());
                    Ok(())
                } else {
                    Err(Error::insert_wrong_element(
                        false,
                        qtype::COMPOUND_LIST,
                        "K",
                    ))
                }
            }
            _ => Err(Error::invalid_operation("push", self.0.qtype, None)),
        }
    }

    /// Insert an element to the underlying q list at the location specified location by an index.
    /// # Parameters
    /// - `index`: Index of the location where the new element is inserted.
    /// - `element`: An element to insert. The type needs to be a one used for atom constructor `K::new_*`. For example,
    ///   int element must be a `i32` type and timestamp element must be a `DateTime<Utc>` type.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let mut q_symbol_list = K::new_symbol_list(vec![String::from("almond")], qattribute::NONE);
    /// q_symbol_list.push(&String::from("hazel")).unwrap();
    /// q_symbol_list.insert(1, &String::from("macadamia")).unwrap();
    /// assert_eq!(
    ///     *q_symbol_list.as_vec::<S>().unwrap(),
    ///     vec![
    ///         String::from("almond"),
    ///         String::from("macadamia"),
    ///         String::from("hazel")
    ///     ]
    /// );
    ///
    /// let mut q_minute_list = K::new_minute_list(vec![Duration::minutes(1024)], qattribute::NONE);
    /// q_minute_list.insert(0, &Duration::minutes(12)).unwrap();
    /// assert_eq!(*q_minute_list.as_vec::<I>().unwrap(), vec![12, 1024]);
    /// ```
    pub fn insert(&mut self, index: usize, element: &dyn Any) -> Result<()> {
        if index > self.len() {
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::BOOL_LIST => {
                    if let Some(boolean) = element.downcast_ref::<bool>() {
                        self.increment();
                        self.as_mut_vec::<G>()
                            .unwrap()
                            .insert(index, *boolean as u8);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(true, qtype::BOOL_LIST, "bool"))
                    }
                }
                qtype::GUID_LIST => {
                    if let Some(guid) = element.downcast_ref::<U>() {
                        self.increment();
                        self.as_mut_vec::<U>().unwrap().insert(index, *guid);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            true,
                            qtype::GUID_LIST,
                            "[u8; 16]",
                        ))
                    }
                }
                qtype::BYTE_LIST => {
                    if let Some(byte) = element.downcast_ref::<u8>() {
                        self.increment();
                        self.as_mut_vec::<G>().unwrap().insert(index, *byte);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(true, qtype::BYTE_LIST, "u8"))
                    }
                }
                qtype::SHORT_LIST => {
                    if let Some(short) = element.downcast_ref::<i16>() {
                        self.increment();
                        self.as_mut_vec::<H>().unwrap().insert(index, *short);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(false, qtype::SHORT_LIST, "i16"))
                    }
                }
                qtype::INT_LIST => {
                    if let Some(int) = element.downcast_ref::<i32>() {
                        self.increment();
                        self.as_mut_vec::<I>().unwrap().insert(index, *int);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(false, qtype::INT_LIST, "i32"))
                    }
                }
                qtype::LONG_LIST => {
                    if let Some(long) = element.downcast_ref::<i64>() {
                        self.increment();
                        self.as_mut_vec::<J>().unwrap().insert(index, *long);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(false, qtype::LONG_LIST, "i64"))
                    }
                }
                qtype::REAL_LIST => {
                    if let Some(real) = element.downcast_ref::<f32>() {
                        self.increment();
                        self.as_mut_vec::<E>().unwrap().insert(index, *real);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(false, qtype::REAL_LIST, "f32"))
                    }
                }
                qtype::FLOAT_LIST => {
                    if let Some(float) = element.downcast_ref::<f64>() {
                        self.increment();
                        self.as_mut_vec::<F>().unwrap().insert(index, *float);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(false, qtype::FLOAT_LIST, "f64"))
                    }
                }
                qtype::STRING => {
                    if let Some(ch) = element.downcast_ref::<char>() {
                        self.as_mut_string().unwrap().insert(index, *ch);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(false, qtype::STRING, "char"))
                    }
                }
                qtype::SYMBOL_LIST => {
                    if let Some(symbol) = element.downcast_ref::<String>() {
                        self.increment();
                        self.as_mut_vec::<S>()
                            .unwrap()
                            .insert(index, symbol.clone());
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::SYMBOL_LIST,
                            "String",
                        ))
                    }
                }
                qtype::TIMESTAMP_LIST => {
                    if let Some(timestamp) = element.downcast_ref::<DateTime<Utc>>() {
                        self.increment();
                        self.as_mut_vec::<J>()
                            .unwrap()
                            .insert(index, datetime_to_q_timestamp(*timestamp));
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::TIMESTAMP_LIST,
                            "DateTime<Utc>",
                        ))
                    }
                }
                qtype::MONTH_LIST => {
                    if let Some(month) = element.downcast_ref::<NaiveDate>() {
                        self.increment();
                        self.as_mut_vec::<I>()
                            .unwrap()
                            .insert(index, date_to_q_month(*month));
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::MONTH_LIST,
                            "Date<Utc>",
                        ))
                    }
                }
                qtype::DATE_LIST => {
                    if let Some(date) = element.downcast_ref::<NaiveDate>() {
                        self.increment();
                        self.as_mut_vec::<I>()
                            .unwrap()
                            .insert(index, date_to_q_date(*date));
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::DATE_LIST,
                            "Date<Utc>",
                        ))
                    }
                }
                qtype::DATETIME_LIST => {
                    if let Some(datetime) = element.downcast_ref::<DateTime<Utc>>() {
                        self.increment();
                        self.as_mut_vec::<F>()
                            .unwrap()
                            .insert(index, datetime_to_q_datetime(*datetime));
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::DATETIME_LIST,
                            "DateTime<Utc>",
                        ))
                    }
                }
                qtype::TIMESPAN_LIST => {
                    if let Some(timespan) = element.downcast_ref::<Duration>() {
                        self.increment();
                        self.as_mut_vec::<J>().unwrap().insert(
                            index,
                            timespan.num_nanoseconds().expect("duration overflow"),
                        );
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::TIMESPAN_LIST,
                            "Duration",
                        ))
                    }
                }
                qtype::MINUTE_LIST => {
                    if let Some(minute) = element.downcast_ref::<Duration>() {
                        self.increment();
                        self.as_mut_vec::<I>()
                            .unwrap()
                            .insert(index, minute.num_minutes() as i32);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::MINUTE_LIST,
                            "Duration",
                        ))
                    }
                }
                qtype::SECOND_LIST => {
                    if let Some(second) = element.downcast_ref::<Duration>() {
                        self.increment();
                        self.as_mut_vec::<I>()
                            .unwrap()
                            .insert(index, second.num_seconds() as i32);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::SECOND_LIST,
                            "Duration",
                        ))
                    }
                }
                qtype::TIME_LIST => {
                    if let Some(time) = element.downcast_ref::<Duration>() {
                        self.increment();
                        self.as_mut_vec::<I>()
                            .unwrap()
                            .insert(index, time.num_milliseconds() as i32);
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::TIME_LIST,
                            "Duration",
                        ))
                    }
                }
                qtype::COMPOUND_LIST => {
                    if let Some(k) = element.downcast_ref::<K>() {
                        self.increment();
                        self.as_mut_vec::<K>().unwrap().insert(index, k.clone());
                        Ok(())
                    } else {
                        Err(Error::insert_wrong_element(
                            false,
                            qtype::COMPOUND_LIST,
                            "K",
                        ))
                    }
                }
                _ => Err(Error::invalid_operation("insert", self.0.qtype, None)),
            }
        }
    }

    /// Pop a `bool` object from q bool list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_bool_list = K::new_bool_list(vec![false, true], qattribute::NONE);
    /// let tail = q_bool_list.pop_bool().unwrap();
    /// assert_eq!(tail, true);
    /// ```
    pub fn pop_bool(&mut self) -> Result<bool> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::BOOL_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<G>().unwrap().pop().unwrap() != 0)
                }
                _ => Err(Error::invalid_operation(
                    "pop_bool",
                    self.0.qtype,
                    Some(qtype::BOOL_LIST),
                )),
            }
        }
    }

    /// Pop a `[u8; 16]` object from q GUID list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_guid_list = K::new_guid_list(
    ///     vec![[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_guid_list.pop_guid().unwrap();
    /// assert_eq!(
    ///     tail,
    ///     [0_u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    /// );
    /// ```
    pub fn pop_guid(&mut self) -> Result<[u8; 16]> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::GUID_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<U>().unwrap().pop().unwrap())
                }
                _ => Err(Error::invalid_operation(
                    "pop_guid",
                    self.0.qtype,
                    Some(qtype::GUID_LIST),
                )),
            }
        }
    }

    /// Pop a `u8` object from q byte list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_byte_list = K::new_byte_list(vec![0x77, 0x99, 0xae], qattribute::NONE);
    /// let tail = q_byte_list.pop_byte().unwrap();
    /// assert_eq!(tail, 0xae_u8);
    /// ```
    pub fn pop_byte(&mut self) -> Result<u8> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::BYTE_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<G>().unwrap().pop().unwrap())
                }
                _ => Err(Error::invalid_operation(
                    "pop_byte",
                    self.0.qtype,
                    Some(qtype::BYTE_LIST),
                )),
            }
        }
    }

    /// Pop a `i16` object from q short list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_short_list = K::new_short_list(vec![12, 50], qattribute::NONE);
    /// let tail = q_short_list.pop_short().unwrap();
    /// assert_eq!(tail, 50_i16);
    /// ```
    pub fn pop_short(&mut self) -> Result<i16> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::SHORT_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<H>().unwrap().pop().unwrap())
                }
                _ => Err(Error::invalid_operation(
                    "pop_short",
                    self.0.qtype,
                    Some(qtype::SHORT_LIST),
                )),
            }
        }
    }

    /// Pop a `i32` object from q int list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_int_list = K::new_int_list(vec![144000, -1, 888], qattribute::NONE);
    /// let tail = q_int_list.pop_int().unwrap();
    /// assert_eq!(tail, 888);
    /// ```
    pub fn pop_int(&mut self) -> Result<i32> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::INT_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<I>().unwrap().pop().unwrap())
                }
                _ => Err(Error::invalid_operation(
                    "pop_int",
                    self.0.qtype,
                    Some(qtype::INT_LIST),
                )),
            }
        }
    }

    /// Pop a `i64` object from q long list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_long_list = K::new_long_list(vec![-86400_i64, 13800000000], qattribute::NONE);
    /// let tail = q_long_list.pop_long().unwrap();
    /// assert_eq!(tail, 13800000000_i64);
    /// ```
    pub fn pop_long(&mut self) -> Result<i64> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::LONG_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<J>().unwrap().pop().unwrap())
                }
                _ => Err(Error::invalid_operation(
                    "pop_long",
                    self.0.qtype,
                    Some(qtype::LONG_LIST),
                )),
            }
        }
    }

    /// Pop a `f32` object from q real list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_real_list = K::new_real_list(vec![9.22_f32, -0.1], qattribute::NONE);
    /// let tail = q_real_list.pop_real().unwrap();
    /// assert_eq!(tail, -0.1_f32);
    /// ```
    pub fn pop_real(&mut self) -> Result<f32> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::REAL_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<E>().unwrap().pop().unwrap())
                }
                _ => Err(Error::invalid_operation(
                    "pop_real",
                    self.0.qtype,
                    Some(qtype::REAL_LIST),
                )),
            }
        }
    }

    /// Pop a `f64` object from q float list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_float_list = K::new_float_list(vec![5634.7666, 120.45, 1001.3], qattribute::NONE);
    /// let tail = q_float_list.pop_float().unwrap();
    /// assert_eq!(tail, 1001.3);
    /// ```
    pub fn pop_float(&mut self) -> Result<f64> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::FLOAT_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<F>().unwrap().pop().unwrap())
                }
                _ => Err(Error::invalid_operation(
                    "pop_float",
                    self.0.qtype,
                    Some(qtype::FLOAT_LIST),
                )),
            }
        }
    }

    /// Pop a `char` object from q string.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_string = K::new_string(String::from("speedy"), qattribute::NONE);
    /// let tail = q_string.pop_char().unwrap();
    /// assert_eq!(tail, 'y');
    /// ```
    pub fn pop_char(&mut self) -> Result<char> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::STRING => Ok(self.as_mut_string().unwrap().pop().unwrap()),
                _ => Err(Error::invalid_operation(
                    "pop_char",
                    self.0.qtype,
                    Some(qtype::STRING),
                )),
            }
        }
    }

    /// Pop a `String` object from q symbol list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    ///let mut q_symbol_list=K::new_symbol_list(vec![String::from("almond"), String::from("macadamia"), String::from("hazel")], qattribute::NONE);
    ///let tail=q_symbol_list.pop_symbol().unwrap();
    ///assert_eq!(tail, String::from("hazel"));
    /// ```
    pub fn pop_symbol(&mut self) -> Result<String> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::SYMBOL_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<S>().unwrap().pop().unwrap())
                }
                _ => Err(Error::invalid_operation(
                    "pop_symbol",
                    self.0.qtype,
                    Some(qtype::SYMBOL_LIST),
                )),
            }
        }
    }

    /// Pop a `DateTime<Utc>` object from q timestamp list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut q_timestamp_list = K::new_timestamp_list(
    ///     vec![Utc.ymd(2019, 8, 9).and_hms_nano(16, 28, 2, 468276775)],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_timestamp_list.pop_timestamp().unwrap();
    /// assert_eq!(tail, Utc.ymd(2019, 8, 9).and_hms_nano(16, 28, 2, 468276775));
    /// ```
    pub fn pop_timestamp(&mut self) -> Result<DateTime<Utc>> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::TIMESTAMP_LIST => {
                    self.decrement();
                    Ok(q_timestamp_to_datetime(
                        self.as_mut_vec::<J>().unwrap().pop().unwrap(),
                    ))
                }
                _ => Err(Error::invalid_operation(
                    "pop_timestamp",
                    self.0.qtype,
                    Some(qtype::TIMESTAMP_LIST),
                )),
            }
        }
    }

    /// Pop a `Date<Utc>` object from q month list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut q_month_list = K::new_month_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2011, 5, 1).unwrap(),
    ///         NaiveDate::from_ymd_opt(2004, 8, 1).unwrap(),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_month_list.pop_month().unwrap();
    /// assert_eq!(tail, NaiveDate::from_ymd_opt(2004, 8, 1).unwrap());
    /// ```
    pub fn pop_month(&mut self) -> Result<NaiveDate> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::MONTH_LIST => {
                    self.decrement();
                    Ok(q_month_to_date(
                        self.as_mut_vec::<I>().unwrap().pop().unwrap(),
                    ))
                }
                _ => Err(Error::invalid_operation(
                    "pop_month",
                    self.0.qtype,
                    Some(qtype::MONTH_LIST),
                )),
            }
        }
    }

    /// Pop a `Date<Utc>` object from q date list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut q_date_list = K::new_date_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2021, 3, 19).unwrap(),
    ///         NaiveDate::from_ymd_opt(2004, 8, 1).unwrap(),
    ///         NaiveDate::from_ymd_opt(2014, 6, 4).unwrap(),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_date_list.pop_date().unwrap();
    /// assert_eq!(tail, NaiveDate::from_ymd_opt(2014, 6, 4).unwrap());
    /// ```
    pub fn pop_date(&mut self) -> Result<NaiveDate> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::DATE_LIST => {
                    self.decrement();
                    Ok(q_date_to_date(
                        self.as_mut_vec::<I>().unwrap().pop().unwrap(),
                    )?)
                }
                _ => Err(Error::invalid_operation(
                    "pop_date",
                    self.0.qtype,
                    Some(qtype::DATE_LIST),
                )),
            }
        }
    }

    /// Pop a `DateTime<Utc>` object from q datetime list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut q_datetime_list = K::new_datetime_list(
    ///     vec![
    ///         Utc.ymd(2018, 9, 22).and_hms_milli(4, 58, 30, 204),
    ///         Utc.ymd(2003, 12, 9).and_hms_milli(19, 58, 30, 326),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_datetime_list.pop_datetime().unwrap();
    /// assert_eq!(tail, Utc.ymd(2003, 12, 9).and_hms_milli(19, 58, 30, 326));
    /// ```
    pub fn pop_datetime(&mut self) -> Result<DateTime<Utc>> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::DATETIME_LIST => {
                    self.decrement();
                    Ok(q_datetime_to_datetime(
                        self.as_mut_vec::<F>().unwrap().pop().unwrap(),
                    ))
                }
                _ => Err(Error::invalid_operation(
                    "pop_datetime",
                    self.0.qtype,
                    Some(qtype::DATETIME_LIST),
                )),
            }
        }
    }

    /// Pop a `Duration` object from q timespan list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let mut q_timespan_list = K::new_timespan_list(
    ///     vec![
    ///         Duration::nanoseconds(6782392639932),
    ///         Duration::nanoseconds(219849398328832),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_timespan_list.pop_timespan().unwrap();
    /// assert_eq!(tail, Duration::nanoseconds(219849398328832));
    /// ```
    pub fn pop_timespan(&mut self) -> Result<Duration> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::TIMESPAN_LIST => {
                    self.decrement();
                    Ok(Duration::nanoseconds(
                        self.as_mut_vec::<J>().unwrap().pop().unwrap(),
                    ))
                }
                _ => Err(Error::invalid_operation(
                    "pop_timespan",
                    self.0.qtype,
                    Some(qtype::TIMESPAN_LIST),
                )),
            }
        }
    }

    /// Pop a `Duration` object from q minute list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let mut q_minute_list = K::new_minute_list(
    ///     vec![Duration::minutes(1024), Duration::minutes(-503)],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_minute_list.pop_minute().unwrap();
    /// assert_eq!(tail, Duration::minutes(-503));
    /// ```
    pub fn pop_minute(&mut self) -> Result<Duration> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::MINUTE_LIST => {
                    self.decrement();
                    Ok(Duration::minutes(
                        self.as_mut_vec::<I>().unwrap().pop().unwrap() as i64,
                    ))
                }
                _ => Err(Error::invalid_operation(
                    "pop_minute",
                    self.0.qtype,
                    Some(qtype::MINUTE_LIST),
                )),
            }
        }
    }

    /// Pop a `Duration` object from q second list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let mut q_second_list = K::new_second_list(
    ///     vec![Duration::seconds(-32467), Duration::seconds(73984)],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_second_list.pop_second().unwrap();
    /// assert_eq!(tail, Duration::seconds(73984));
    /// ```
    pub fn pop_second(&mut self) -> Result<Duration> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::SECOND_LIST => {
                    self.decrement();
                    Ok(Duration::seconds(
                        self.as_mut_vec::<I>().unwrap().pop().unwrap() as i64,
                    ))
                }
                _ => Err(Error::invalid_operation(
                    "pop_second",
                    self.0.qtype,
                    Some(qtype::SECOND_LIST),
                )),
            }
        }
    }

    /// Pop a `Duration` object from q time list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let mut q_time_list = K::new_time_list(
    ///     vec![
    //             Duration::milliseconds(902467),
    ///         Duration::milliseconds(-23587934),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_time_list.pop_time().unwrap();
    /// assert_eq!(tail, Duration::milliseconds(-23587934));
    /// ```
    pub fn pop_time(&mut self) -> Result<Duration> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::TIME_LIST => {
                    self.decrement();
                    Ok(Duration::milliseconds(
                        self.as_mut_vec::<I>().unwrap().pop().unwrap() as i64,
                    ))
                }
                _ => Err(Error::invalid_operation(
                    "pop_time",
                    self.0.qtype,
                    Some(qtype::TIME_LIST),
                )),
            }
        }
    }

    /// Pop an element as `K` from the tail of the underlying list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    /// use chrono::Duration;
    ///
    /// let mut q_time_list = K::new_time_list(
    ///     vec![
    ///         Duration::milliseconds(902467),
    ///         Duration::milliseconds(-23587934),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let mut tail = q_time_list.pop().unwrap();
    /// assert_eq!(format!("{}", tail), String::from("-06:33:07.934"));
    /// let mut q_compound_list = K::new_compound_list(vec![
    ///     K::new_long_list(vec![10000324_i64, -43890], qattribute::NONE),
    ///     K::new_symbol(String::from("fire")),
    ///     K::new_timestamp_list(
    ///         vec![
    ///             Utc.ymd(2018, 4, 10).and_hms_nano(15, 47, 39, 758934332),
    ///             Utc.ymd(2008, 12, 4).and_hms_nano(14, 12, 7, 548932080),
    ///         ],
    ///         qattribute::NONE,
    ///     ),
    /// ]);
    /// tail = q_compound_list.pop().unwrap();
    /// assert_eq!(
    ///     format!("{}", tail),
    ///     String::from("2018.04.10D15:47:39.758934332 2008.12.04D14:12:07.548932080")
    /// );
    /// ```
    pub fn pop(&mut self) -> Result<K> {
        if self.is_empty() {
            // 0 length
            Err(Error::pop_from_empty_list())
        } else {
            match self.0.qtype {
                qtype::BOOL_LIST => {
                    self.decrement();
                    Ok(K::new_bool(
                        self.as_mut_vec::<G>().unwrap().pop().unwrap() != 0,
                    ))
                }
                qtype::GUID_LIST => {
                    self.decrement();
                    Ok(K::new_guid(self.as_mut_vec::<U>().unwrap().pop().unwrap()))
                }
                qtype::BYTE_LIST => {
                    self.decrement();
                    Ok(K::new_byte(self.as_mut_vec::<G>().unwrap().pop().unwrap()))
                }
                qtype::SHORT_LIST => {
                    self.decrement();
                    Ok(K::new_short(self.as_mut_vec::<H>().unwrap().pop().unwrap()))
                }
                qtype::INT_LIST => {
                    self.decrement();
                    Ok(K::new_int(self.as_mut_vec::<I>().unwrap().pop().unwrap()))
                }
                qtype::LONG_LIST => {
                    self.decrement();
                    Ok(K::new_long(self.as_mut_vec::<J>().unwrap().pop().unwrap()))
                }
                qtype::REAL_LIST => {
                    self.decrement();
                    Ok(K::new_real(self.as_mut_vec::<E>().unwrap().pop().unwrap()))
                }
                qtype::FLOAT_LIST => {
                    self.decrement();
                    Ok(K::new_float(self.as_mut_vec::<F>().unwrap().pop().unwrap()))
                }
                qtype::STRING => Ok(K::new_char(self.as_mut_string().unwrap().pop().unwrap())),
                qtype::SYMBOL_LIST => {
                    self.decrement();
                    Ok(K::new_symbol(
                        self.as_mut_vec::<S>().unwrap().pop().unwrap(),
                    ))
                }
                qtype::TIMESTAMP_LIST => {
                    self.decrement();
                    Ok(K::new(
                        qtype::TIMESTAMP_ATOM,
                        qattribute::NONE,
                        k0_inner::long(self.as_mut_vec::<J>().unwrap().pop().unwrap()),
                    ))
                }
                qtype::MONTH_LIST => {
                    self.decrement();
                    Ok(K::new(
                        qtype::MONTH_ATOM,
                        qattribute::NONE,
                        k0_inner::int(self.as_mut_vec::<I>().unwrap().pop().unwrap()),
                    ))
                }
                qtype::DATE_LIST => {
                    self.decrement();
                    Ok(K::new(
                        qtype::DATE_ATOM,
                        qattribute::NONE,
                        k0_inner::int(self.as_mut_vec::<I>().unwrap().pop().unwrap()),
                    ))
                }
                qtype::DATETIME_LIST => {
                    self.decrement();
                    Ok(K::new(
                        qtype::DATETIME_ATOM,
                        qattribute::NONE,
                        k0_inner::float(self.as_mut_vec::<F>().unwrap().pop().unwrap()),
                    ))
                }
                qtype::TIMESPAN_LIST => {
                    self.decrement();
                    Ok(K::new(
                        qtype::TIMESPAN_ATOM,
                        qattribute::NONE,
                        k0_inner::long(self.as_mut_vec::<J>().unwrap().pop().unwrap()),
                    ))
                }
                qtype::MINUTE_LIST => {
                    self.decrement();
                    Ok(K::new(
                        qtype::MINUTE_ATOM,
                        qattribute::NONE,
                        k0_inner::int(self.as_mut_vec::<I>().unwrap().pop().unwrap()),
                    ))
                }
                qtype::SECOND_LIST => {
                    self.decrement();
                    Ok(K::new(
                        qtype::SECOND_ATOM,
                        qattribute::NONE,
                        k0_inner::int(self.as_mut_vec::<I>().unwrap().pop().unwrap()),
                    ))
                }
                qtype::TIME_LIST => {
                    self.decrement();
                    Ok(K::new(
                        qtype::TIME_ATOM,
                        qattribute::NONE,
                        k0_inner::int(self.as_mut_vec::<I>().unwrap().pop().unwrap()),
                    ))
                }
                qtype::COMPOUND_LIST => {
                    self.decrement();
                    Ok(self.as_mut_vec::<K>().unwrap().pop().unwrap())
                }
                _ => Err(Error::invalid_operation("pop", self.0.qtype, None)),
            }
        }
    }

    /// Remove a `bool` object from the underlying q bool list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_bool_list = K::new_bool_list(vec![false, true], qattribute::NONE);
    /// let tail = q_bool_list.remove_bool(0).unwrap();
    /// assert_eq!(tail, false);
    /// ```
    pub fn remove_bool(&mut self, index: usize) -> Result<bool> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::BOOL_LIST => Ok(self.as_mut_vec::<G>().unwrap().remove(index) != 0),
                _ => Err(Error::invalid_operation(
                    "remove_bool",
                    self.0.qtype,
                    Some(qtype::BOOL_LIST),
                )),
            }
        }
    }

    /// Remove a `[u8;16]` object from the underlying q GUID list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    ///let mut q_guid_list=K::new_guid_list(vec![[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15], [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16]], qattribute::NONE);
    ///let tail=q_guid_list.remove_guid(1).unwrap();
    ///assert_eq!(tail, [1_u8,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16]);
    /// ```
    pub fn remove_guid(&mut self, index: usize) -> Result<[u8; 16]> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::GUID_LIST => Ok(self.as_mut_vec::<U>().unwrap().remove(index)),
                _ => Err(Error::invalid_operation(
                    "remove_guid",
                    self.0.qtype,
                    Some(qtype::GUID_LIST),
                )),
            }
        }
    }

    /// Remove a `u8` object from the underlying q byte list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_byte_list = K::new_byte_list(vec![0x77, 0x99, 0xae], qattribute::NONE);
    /// let tail = q_byte_list.remove_byte(1).unwrap();
    /// assert_eq!(tail, 0x99_u8);
    /// ```
    pub fn remove_byte(&mut self, index: usize) -> Result<u8> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::BYTE_LIST => Ok(self.as_mut_vec::<G>().unwrap().remove(index)),
                _ => Err(Error::invalid_operation(
                    "remove_byte",
                    self.0.qtype,
                    Some(qtype::BYTE_LIST),
                )),
            }
        }
    }

    /// Remove a `i16` object from the underlying q short list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_short_list = K::new_short_list(vec![12, 50], qattribute::NONE);
    /// let tail = q_short_list.remove_short(0).unwrap();
    /// assert_eq!(tail, 12_i16);
    /// ```
    pub fn remove_short(&mut self, index: usize) -> Result<i16> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::SHORT_LIST => Ok(self.as_mut_vec::<H>().unwrap().remove(index)),
                _ => Err(Error::invalid_operation(
                    "remove_short",
                    self.0.qtype,
                    Some(qtype::SHORT_LIST),
                )),
            }
        }
    }

    /// Remove a `i32` object from the underlying q int list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_int_list = K::new_int_list(vec![144000, -1, 888], qattribute::NONE);
    /// let tail = q_int_list.remove_int(1).unwrap();
    /// assert_eq!(tail, -1);
    /// ```
    pub fn remove_int(&mut self, index: usize) -> Result<i32> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::INT_LIST => Ok(self.as_mut_vec::<I>().unwrap().remove(index)),
                _ => Err(Error::invalid_operation(
                    "remove_int",
                    self.0.qtype,
                    Some(qtype::INT_LIST),
                )),
            }
        }
    }

    /// Remove a `i64` object from the underlying q long list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_long_list = K::new_long_list(vec![-86400_i64, 13800000000], qattribute::NONE);
    /// let tail = q_long_list.remove_long(0).unwrap();
    /// assert_eq!(tail, -86400_i64);
    /// ```
    pub fn remove_long(&mut self, index: usize) -> Result<i64> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::LONG_LIST => Ok(self.as_mut_vec::<J>().unwrap().remove(index)),
                _ => Err(Error::invalid_operation(
                    "remove_long",
                    self.0.qtype,
                    Some(qtype::LONG_LIST),
                )),
            }
        }
    }

    /// Remove a `f32` object from the underlying q real list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_real_list = K::new_real_list(vec![9.22_f32, -0.1], qattribute::NONE);
    /// let tail = q_real_list.remove_real(1).unwrap();
    /// assert_eq!(tail, -0.1_f32);
    /// ```
    pub fn remove_real(&mut self, index: usize) -> Result<f32> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::REAL_LIST => Ok(self.as_mut_vec::<E>().unwrap().remove(index)),
                _ => Err(Error::invalid_operation(
                    "remove_real",
                    self.0.qtype,
                    Some(qtype::REAL_LIST),
                )),
            }
        }
    }

    /// Remove a `f64` object from the underlying q float list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_float_list = K::new_float_list(vec![5634.7666, 120.45, 1001.3], qattribute::NONE);
    /// let tail = q_float_list.remove_float(0).unwrap();
    /// assert_eq!(tail, 5634.7666);
    /// ```
    pub fn remove_float(&mut self, index: usize) -> Result<f64> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::FLOAT_LIST => Ok(self.as_mut_vec::<F>().unwrap().remove(index)),
                _ => Err(Error::invalid_operation(
                    "remove_float",
                    self.0.qtype,
                    Some(qtype::FLOAT_LIST),
                )),
            }
        }
    }

    /// Remove a `char` object from the underlying q string.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_string = K::new_string(String::from("speedy"), qattribute::NONE);
    /// let tail = q_string.remove_char(2).unwrap();
    /// assert_eq!(tail, 'e');
    /// ```
    pub fn remove_char(&mut self, index: usize) -> Result<char> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::STRING => Ok(self.as_mut_string().unwrap().remove(index)),
                _ => Err(Error::invalid_operation(
                    "remove_char",
                    self.0.qtype,
                    Some(qtype::STRING),
                )),
            }
        }
    }

    /// Remove a `String` object from the underlying q symbol list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let mut q_symbol_list = K::new_symbol_list(
    ///     vec![
    ///         String::from("almond"),
    ///         String::from("macadamia"),
    ///         String::from("hazel"),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_symbol_list.remove_symbol(2).unwrap();
    /// assert_eq!(tail, String::from("hazel"));
    /// ```
    pub fn remove_symbol(&mut self, index: usize) -> Result<String> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::SYMBOL_LIST => Ok(self.as_mut_vec::<S>().unwrap().remove(index)),
                _ => Err(Error::invalid_operation(
                    "remove_symbol",
                    self.0.qtype,
                    Some(qtype::SYMBOL_LIST),
                )),
            }
        }
    }

    /// Remove a `DateTime<Utc>` object from the underlying q timestamp list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut q_timestamp_list = K::new_timestamp_list(
    ///     vec![
    ///         Utc.ymd(2019, 8, 9).and_hms_nano(16, 28, 2, 468276775),
    ///         Utc.ymd(2015, 3, 28).and_hms_nano(14, 2, 41, 46827329),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_timestamp_list.remove_timestamp(0).unwrap();
    /// assert_eq!(tail, Utc.ymd(2019, 8, 9).and_hms_nano(16, 28, 2, 468276775));
    /// ```
    pub fn remove_timestamp(&mut self, index: usize) -> Result<DateTime<Utc>> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::TIMESTAMP_LIST => Ok(q_timestamp_to_datetime(
                    self.as_mut_vec::<J>().unwrap().remove(index),
                )),
                _ => Err(Error::invalid_operation(
                    "remove_timestamp",
                    self.0.qtype,
                    Some(qtype::TIMESTAMP_LIST),
                )),
            }
        }
    }

    /// Remove a `Date<Utc>` object from the underlying q month list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut q_month_list = K::new_month_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2011, 5, 1).unwrap(),
    ///         NaiveDate::from_ymd_opt(2004, 8, 1).unwrap(),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_month_list.remove_month(0).unwrap();
    /// assert_eq!(tail, NaiveDate::from_ymd_opt(2011, 5, 1).unwrap());
    /// ```
    pub fn remove_month(&mut self, index: usize) -> Result<NaiveDate> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::MONTH_LIST => Ok(q_month_to_date(
                    self.as_mut_vec::<I>().unwrap().remove(index),
                )),
                _ => Err(Error::invalid_operation(
                    "remove_month",
                    self.0.qtype,
                    Some(qtype::MONTH_LIST),
                )),
            }
        }
    }

    /// Remove a `Date<Utc>` object from the underlying q date list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut q_date_list = K::new_date_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2021, 3, 19).unwrap(),
    ///         NaiveDate::from_ymd_opt(2004, 8, 1).unwrap(),
    ///         NaiveDate::from_ymd_opt(2014, 6, 4).unwrap(),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_date_list.remove_date(1).unwrap();
    /// assert_eq!(tail, NaiveDate::from_ymd_opt(2004, 8, 1).unwrap());
    /// ```
    pub fn remove_date(&mut self, index: usize) -> Result<NaiveDate> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::DATE_LIST => Ok(q_date_to_date(
                    self.as_mut_vec::<I>().unwrap().remove(index),
                )?),
                _ => Err(Error::invalid_operation(
                    "remove_date",
                    self.0.qtype,
                    Some(qtype::DATE_LIST),
                )),
            }
        }
    }

    /// Remove a `DateTime<Utc>` object from the underlying q datetime list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let mut q_datetime_list = K::new_datetime_list(
    ///     vec![
    ///         Utc.ymd(2018, 9, 22).and_hms_milli(4, 58, 30, 204),
    ///         Utc.ymd(2003, 12, 9).and_hms_milli(19, 58, 30, 326),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_datetime_list.remove_datetime(1).unwrap();
    /// assert_eq!(tail, Utc.ymd(2003, 12, 9).and_hms_milli(19, 58, 30, 326));
    /// ```
    pub fn remove_datetime(&mut self, index: usize) -> Result<DateTime<Utc>> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::DATETIME_LIST => Ok(q_datetime_to_datetime(
                    self.as_mut_vec::<F>().unwrap().remove(index),
                )),
                _ => Err(Error::invalid_operation(
                    "remove_datetime",
                    self.0.qtype,
                    Some(qtype::DATETIME_LIST),
                )),
            }
        }
    }

    /// Remove a `Duration` object from the underlying q timespan list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let mut q_timespan_list = K::new_timespan_list(
    ///     vec![
    ///         Duration::nanoseconds(6782392639932),
    ///         Duration::nanoseconds(219849398328832),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_timespan_list.remove_timespan(0).unwrap();
    /// assert_eq!(tail, Duration::nanoseconds(6782392639932));
    /// ```
    pub fn remove_timespan(&mut self, index: usize) -> Result<Duration> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::TIMESPAN_LIST => Ok(Duration::nanoseconds(
                    self.as_mut_vec::<J>().unwrap().remove(index),
                )),
                _ => Err(Error::invalid_operation(
                    "remove_timespan",
                    self.0.qtype,
                    Some(qtype::TIMESPAN_LIST),
                )),
            }
        }
    }

    /// Remove a `Duration` object from the underlying q minute list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let mut q_minute_list = K::new_minute_list(
    ///     vec![Duration::minutes(1024), Duration::minutes(-503)],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_minute_list.remove_minute(1).unwrap();
    /// assert_eq!(tail, Duration::minutes(-503));
    /// ```
    pub fn remove_minute(&mut self, index: usize) -> Result<Duration> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::MINUTE_LIST => Ok(Duration::minutes(
                    self.as_mut_vec::<I>().unwrap().remove(index) as i64,
                )),
                _ => Err(Error::invalid_operation(
                    "remove_minute",
                    self.0.qtype,
                    Some(qtype::MINUTE_LIST),
                )),
            }
        }
    }

    /// Remove a `Duration` object from the underlying q second list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let mut q_second_list = K::new_second_list(
    ///     vec![Duration::seconds(-32467), Duration::seconds(73984)],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_second_list.remove_second(0).unwrap();
    /// assert_eq!(tail, Duration::seconds(-32467));
    /// ```
    pub fn remove_second(&mut self, index: usize) -> Result<Duration> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::SECOND_LIST => Ok(Duration::seconds(
                    self.as_mut_vec::<I>().unwrap().remove(index) as i64,
                )),
                _ => Err(Error::invalid_operation(
                    "remove_second",
                    self.0.qtype,
                    Some(qtype::SECOND_LIST),
                )),
            }
        }
    }

    /// Remove a `Duration` object from the underlying q time list.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::Duration;
    ///
    /// let mut q_time_list = K::new_time_list(
    ///     vec![
    ///         Duration::milliseconds(902467),
    ///         Duration::milliseconds(-23587934),
    ///         Duration::milliseconds(278958528),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let tail = q_time_list.remove_time(2).unwrap();
    /// assert_eq!(tail, Duration::milliseconds(278958528));
    /// ```
    pub fn remove_time(&mut self, index: usize) -> Result<Duration> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::TIME_LIST => Ok(Duration::milliseconds(
                    self.as_mut_vec::<I>().unwrap().remove(index) as i64,
                )),
                _ => Err(Error::invalid_operation(
                    "remove_time",
                    self.0.qtype,
                    Some(qtype::TIME_LIST),
                )),
            }
        }
    }

    /// Remove an element as `K` object from the underlying q list.
    ///  # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    /// use chrono::Duration;
    ///
    /// let mut q_time_list = K::new_time_list(
    ///     vec![
    ///         Duration::milliseconds(902467),
    ///         Duration::milliseconds(-23587934),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let mut tail = q_time_list.remove(1).unwrap();
    /// assert_eq!(format!("{}", tail), String::from("-06:33:07.934"));
    ///
    /// let mut q_compound_list = K::new_compound_list(vec![
    ///     K::new_long_list(vec![10000324_i64, -43890], qattribute::UNIQUE),
    ///     K::new_symbol(String::from("fire")),
    ///     K::new_timestamp_list(
    ///         vec![
    ///             Utc.ymd(2018, 4, 10).and_hms_nano(15, 47, 39, 758934332),
    ///             Utc.ymd(2008, 12, 4).and_hms_nano(14, 12, 7, 548932080),
    ///         ],
    ///         qattribute::NONE,
    ///     ),
    /// ]);
    /// tail = q_compound_list.remove(0).unwrap();
    /// assert_eq!(format!("{}", tail), String::from("`u#10000324 -43890"));
    /// ```
    pub fn remove(&mut self, index: usize) -> Result<K> {
        if index >= self.len() {
            // 0 length
            Err(Error::index_out_of_bounds(self.len(), index))
        } else {
            match self.0.qtype {
                qtype::BOOL_LIST => Ok(K::new_bool(
                    self.as_mut_vec::<G>().unwrap().remove(index) != 0,
                )),
                qtype::GUID_LIST => Ok(K::new_guid(self.as_mut_vec::<U>().unwrap().remove(index))),
                qtype::BYTE_LIST => Ok(K::new_byte(self.as_mut_vec::<G>().unwrap().remove(index))),
                qtype::SHORT_LIST => {
                    Ok(K::new_short(self.as_mut_vec::<H>().unwrap().remove(index)))
                }
                qtype::INT_LIST => Ok(K::new_int(self.as_mut_vec::<I>().unwrap().remove(index))),
                qtype::LONG_LIST => Ok(K::new_long(self.as_mut_vec::<J>().unwrap().remove(index))),
                qtype::REAL_LIST => Ok(K::new_real(self.as_mut_vec::<E>().unwrap().remove(index))),
                qtype::FLOAT_LIST => {
                    Ok(K::new_float(self.as_mut_vec::<F>().unwrap().remove(index)))
                }
                qtype::STRING => Ok(K::new_char(self.as_mut_string().unwrap().remove(index))),
                qtype::SYMBOL_LIST => {
                    Ok(K::new_symbol(self.as_mut_vec::<S>().unwrap().remove(index)))
                }
                qtype::TIMESTAMP_LIST => Ok(K::new_timestamp(q_timestamp_to_datetime(
                    self.as_mut_vec::<J>().unwrap().remove(index),
                ))),
                qtype::MONTH_LIST => Ok(K::new_month(q_month_to_date(
                    self.as_mut_vec::<I>().unwrap().remove(index),
                ))),
                qtype::DATE_LIST => Ok(K::new_date(q_date_to_date(
                    self.as_mut_vec::<I>().unwrap().remove(index),
                )?)),
                qtype::DATETIME_LIST => Ok(K::new_datetime(q_datetime_to_datetime(
                    self.as_mut_vec::<F>().unwrap().remove(index),
                ))),
                qtype::TIMESPAN_LIST => Ok(K::new_timespan(Duration::nanoseconds(
                    self.as_mut_vec::<J>().unwrap().remove(index),
                ))),
                qtype::MINUTE_LIST => Ok(K::new_minute(Duration::minutes(
                    self.as_mut_vec::<I>().unwrap().remove(index) as i64,
                ))),
                qtype::SECOND_LIST => Ok(K::new_second(Duration::seconds(
                    self.as_mut_vec::<I>().unwrap().remove(index) as i64,
                ))),
                qtype::TIME_LIST => Ok(K::new_time(Duration::milliseconds(
                    self.as_mut_vec::<I>().unwrap().remove(index) as i64,
                ))),
                qtype::COMPOUND_LIST => Ok(self.as_mut_vec::<K>().unwrap().remove(index)),
                _ => Err(Error::invalid_operation("remove", self.0.qtype, None)),
            }
        }
    }

    /// Add a pair of key-value to a q dictionary.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let keys = K::new_int_list(vec![0, 1, 2], qattribute::NONE);
    /// let values = K::new_date_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2000, 1, 9).unwrap(),
    ///         NaiveDate::from_ymd_opt(2001, 4, 10).unwrap(),
    ///         NaiveDate::from_ymd_opt(2015, 3, 16).unwrap(),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let mut q_dictionary = K::new_dictionary(keys, values).unwrap();
    ///
    /// q_dictionary
    ///     .push_pair(&3, &NaiveDate::from_ymd_opt(2020, 8, 9).unwrap())
    ///     .unwrap();
    /// assert_eq!(
    ///     format!("{}", q_dictionary),
    ///     String::from("0 1 2 3i!2000.01.09 2001.04.10 2015.03.16 2020.08.09")
    /// );
    /// ```
    pub fn push_pair(&mut self, key: &dyn Any, value: &dyn Any) -> Result<()> {
        match self.0.qtype {
            qtype::DICTIONARY => {
                let dictionary = self.as_mut_vec::<K>().unwrap();
                match dictionary[0].push(key) {
                    Ok(_) => match dictionary[1].push(value) {
                        Ok(_) => Ok(()),
                        Err(error) => {
                            // Revert the change to the key
                            dictionary[0].pop().unwrap();
                            Err(error)
                        }
                    },
                    Err(error) => Err(error),
                }
            }
            _ => Err(Error::invalid_operation(
                "push_pair",
                self.0.qtype,
                Some(qtype::DICTIONARY),
            )),
        }
    }

    /// Pop the last key-vaue pair from a q dictionary.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let keys = K::new_int_list(vec![0, 1, 2], qattribute::NONE);
    /// let values = K::new_date_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2000, 1, 9).unwrap(),
    ///         NaiveDate::from_ymd_opt(2001, 4, 10).unwrap(),
    ///         NaiveDate::from_ymd_opt(2015, 3, 16).unwrap(),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let mut q_dictionary = K::new_dictionary(keys, values).unwrap();
    ///
    /// q_dictionary.pop_pair().unwrap();
    /// assert_eq!(
    ///     format!("{}", q_dictionary),
    ///     String::from("0 1i!2000.01.09 2001.04.10")
    /// );
    /// ```
    pub fn pop_pair(&mut self) -> Result<(K, K)> {
        match self.0.qtype {
            qtype::DICTIONARY => {
                let dictionary = self.as_mut_vec::<K>().unwrap();
                if let (Ok(key), Ok(value)) = (dictionary[0].pop(), dictionary[1].pop()) {
                    Ok((key, value))
                } else {
                    // Dictionary type assures the result is one of failure for both key and value, or success for both key and value.
                    Err(Error::pop_from_empty_list())
                }
            }
            _ => Err(Error::invalid_operation(
                "pop_pair",
                self.0.qtype,
                Some(qtype::DICTIONARY),
            )),
        }
    }

    /// Get the length of q object. The meaning of the returned value varies according to the type:
    /// - atom: 1
    /// - list: The number of elements in the list.
    /// - table: The number of rows.
    /// - dictionary: The number of keys.
    /// - general null: 1
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    /// use chrono::prelude::*;
    ///
    /// let q_symbol_list = K::new_symbol_list(
    ///     vec![
    ///         String::from("almond"),
    ///         String::from("macadamia"),
    ///         String::from("hazel"),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// assert_eq!(q_symbol_list.len(), 3);
    ///
    /// let keys = K::new_int_list(vec![0, 1, 2], qattribute::NONE);
    /// let values = K::new_date_list(
    ///     vec![
    ///         NaiveDate::from_ymd_opt(2000, 1, 9).unwrap(),
    ///         NaiveDate::from_ymd_opt(2001, 4, 10).unwrap(),
    ///         NaiveDate::from_ymd_opt(2015, 3, 16).unwrap(),
    ///     ],
    ///     qattribute::NONE,
    /// );
    /// let mut q_dictionary = K::new_dictionary(keys, values).unwrap();
    /// assert_eq!(q_dictionary.len(), 3);
    /// ```
    pub fn len(&self) -> usize {
        match self.0.qtype {
            _t @ qtype::COMPOUND_LIST..=qtype::TIME_LIST => {
                // List
                match &self.0.value {
                    // string is stored as symbol (`String`).
                    k0_inner::symbol(string) => string.len(),
                    // The other lists.
                    k0_inner::list(list) => list.n as usize,
                    _ => unreachable!(),
                }
            }
            qtype::TABLE => {
                // Table
                match &self.0.value {
                    k0_inner::table(dictionary) => {
                        // Dictionary is a vector of [K (keys), K (values)]
                        // values is assured to be a list of K as this is a table type.
                        // Retrieve the first column and get its length.
                        match &dictionary.as_vec::<K>().unwrap()[1].as_vec::<K>().unwrap()[0]
                            .0
                            .value
                        {
                            k0_inner::list(column) => {
                                // Return the number of rows
                                column.n as usize
                            }
                            k0_inner::symbol(column) => {
                                // char column
                                // Return the number of rows
                                column.len()
                            }
                            _ => unreachable!(),
                        }
                    }
                    _ => unreachable!(),
                }
            }
            qtype::DICTIONARY | qtype::SORTED_DICTIONARY => {
                // Dictionary is a vector of [K (keys), K (values)]
                // Get keys and return its length.
                match &self.as_vec::<K>().unwrap()[0].0.value {
                    k0_inner::list(list) => list.n as usize,
                    // Keyed table
                    // Get the number of rows by deligating it to table.len()
                    k0_inner::table(_) => self.as_vec::<K>().unwrap()[0].len(),
                    _ => unreachable!(),
                }
            }
            // Atom and general null
            _ => 1,
        }
    }

    /// Check if the underlying value is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Create a table object from a dictionary object. Return value is either of:
    /// - `Err(original value)`: If the argument is not a dictionary. The returned object
    ///   is wrapped in error enum and can be retrieved by [`into_inner`](error/enum.Error.html#method.into_inner).
    /// - `Ok(table)`: In case of successful conversion.
    /// # Note
    /// - Key type must be a symbol.
    /// - This function does not check if lengths of columns are same.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_dictionary = K::new_dictionary(
    ///     K::new_symbol_list(
    ///         vec![String::from("a"), String::from("b"), String::from("c")],
    ///         qattribute::NONE,
    ///     ),
    ///     K::new_compound_list(vec![
    ///         K::new_int_list(vec![10, 20, 30], qattribute::NONE),
    ///         K::new_symbol_list(
    ///             vec![
    ///                 String::from("honey"),
    ///                 String::from("sugar"),
    ///                 String::from("maple"),
    ///             ],
    ///             qattribute::NONE,
    ///         ),
    ///         K::new_bool_list(vec![false, false, true], qattribute::NONE),
    ///     ]),
    /// )
    /// .unwrap();
    ///
    /// let q_table = q_dictionary.flip().unwrap();
    /// assert_eq!(
    ///     format!("{}", q_table),
    ///     String::from("+`a`b`c!(10 20 30i;`honey`sugar`maple;001b)")
    /// );
    /// ```
    pub fn flip(self) -> Result<Self> {
        match self.0.qtype {
            qtype::DICTIONARY | qtype::SORTED_DICTIONARY => {
                let keys_values = self.as_vec::<K>().unwrap();
                if keys_values[0].0.qtype == qtype::SYMBOL_LIST
                    && keys_values[1].0.qtype == qtype::COMPOUND_LIST
                {
                    Ok(K::new(
                        qtype::TABLE,
                        qattribute::NONE,
                        k0_inner::table(self),
                    ))
                } else {
                    Err(Error::object(self))
                }
            }
            // Failed to convert. Return the original argument.
            _ => Err(Error::object(self)),
        }
    }

    /// Convert a table into a keyed table with the first `n` columns ebing keys.
    ///  In case of error for type mismatch the original object is returned wrapped
    ///  in error enum and can be retrieved by [`into_inner`](error/enum.Error.html#method.into_inner).
    ///  # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_dictionary = K::new_dictionary(
    ///     K::new_symbol_list(
    ///         vec![String::from("a"), String::from("b"), String::from("c")],
    ///         qattribute::NONE,
    ///     ),
    ///     K::new_compound_list(vec![
    ///         K::new_int_list(vec![10, 20, 30], qattribute::NONE),
    ///         K::new_symbol_list(
    ///             vec![
    ///                 String::from("honey"),
    ///                 String::from("sugar"),
    ///                 String::from("maple"),
    ///             ],
    ///             qattribute::NONE,
    ///         ),
    ///         K::new_bool_list(vec![false, false, true], qattribute::NONE),
    ///     ]),
    /// )
    /// .unwrap();
    ///
    /// let q_table = q_dictionary.flip().unwrap();
    /// let q_keyed_table = q_table.enkey(1).unwrap();
    /// assert_eq!(
    ///     format!("{}", q_keyed_table),
    ///     String::from("(+,`a!,10 20 30i)!(+`b`c!(`honey`sugar`maple;001b))")
    /// );
    /// ```
    pub fn enkey(self, mut n: usize) -> Result<Self> {
        match self.0.value {
            k0_inner::table(mut dictionary) => {
                let headers_columns = dictionary.as_mut_vec::<K>().unwrap();
                if headers_columns[0].len() <= n {
                    // Maximum number of keys are #columns - 1
                    n = headers_columns[0].len() - 1;
                }
                let value_heders = K::new_symbol_list(
                    headers_columns[0].as_mut_vec::<S>().unwrap().split_off(n),
                    qattribute::NONE,
                );
                let value_columns = K::new_compound_list(
                    headers_columns[1].as_mut_vec::<K>().unwrap().split_off(n),
                );
                // Build value table
                let value_table = K::new_dictionary(value_heders, value_columns)
                    .unwrap()
                    .flip()
                    .unwrap();
                Ok(K::new_dictionary(dictionary.flip().unwrap(), value_table)
                    .expect("failed to build keyed table"))
            }
            // Not a table. Return the original argument.
            _ => Err(Error::object(self)),
        }
    }

    /// Convert a keyed table into an ordinary table. In case of error for type mismatch
    ///  the original object is returned wrapped in error enum and can be retrieved by [`into_inner`](error/enum.Error.html#method.into_inner).
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// let q_dictionary = K::new_dictionary(
    ///     K::new_symbol_list(
    ///         vec![String::from("a"), String::from("b"), String::from("c")],
    ///         qattribute::NONE,
    ///     ),
    ///     K::new_compound_list(vec![
    ///         K::new_int_list(vec![10, 20, 30], qattribute::NONE),
    ///         K::new_symbol_list(
    ///             vec![
    ///                 String::from("honey"),
    ///                 String::from("sugar"),
    ///                 String::from("maple"),
    ///             ],
    ///             qattribute::NONE,
    ///         ),
    ///         K::new_bool_list(vec![false, false, true], qattribute::NONE),
    ///     ]),
    /// )
    /// .unwrap();
    ///
    /// let q_table = q_dictionary.flip().unwrap();
    /// let q_keyed_table = q_table.enkey(1).unwrap();
    /// assert_eq!(
    ///     format!("{}", q_keyed_table),
    ///     String::from("(+,`a!,10 20 30i)!(+`b`c!(`honey`sugar`maple;001b))")
    /// );
    /// let revived_table = q_keyed_table.unkey().unwrap();
    /// assert_eq!(
    ///     format!("{}", revived_table),
    ///     String::from("+`a`b`c!(10 20 30i;`honey`sugar`maple;001b)")
    /// );
    /// ```
    pub fn unkey(mut self) -> Result<Self> {
        match self.0.qtype {
            qtype::DICTIONARY => {
                // Key table and value table
                let value_table = self.as_mut_vec::<K>().unwrap().pop().unwrap();
                let key_table = self.as_mut_vec::<K>().unwrap().pop().unwrap();
                match (key_table.0.value, value_table.0.value) {
                    (
                        k0_inner::table(mut key_dictionary),
                        k0_inner::table(mut value_dictionary),
                    ) => {
                        // Key dictionary and value dictionary
                        key_dictionary
                            .as_mut_vec::<K>()
                            .unwrap()
                            .iter_mut()
                            .zip(&mut *value_dictionary.as_mut_vec::<K>().unwrap())
                            .enumerate()
                            .for_each(|(i, (key, value))| {
                                // Merge key component and value component
                                if i == 0 {
                                    // Header
                                    key.as_mut_vec::<S>()
                                        .unwrap()
                                        .append(value.as_mut_vec::<S>().unwrap());
                                } else {
                                    // Column
                                    key.as_mut_vec::<K>()
                                        .unwrap()
                                        .append(value.as_mut_vec::<K>().unwrap());
                                }
                            });
                        // Flip joint dictionary to table
                        key_dictionary.flip()
                    }
                    _ => unreachable!(),
                }
            }
            // Not a keyed table. Return the original argument.
            _ => Err(Error::object(self)),
        }
    }
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Private Functions
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% Constructors //%%vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Convert `DateTime<Utc>` into `i64`. The returned value is an elapsed time in nanoseconds since `2000.01.01D00:00:00`.
fn datetime_to_q_timestamp(timestamp: DateTime<Utc>) -> i64 {
    // q          |----------------------------------------|
    // Rust  |----------------------------------------------------|

    if timestamp <= *qnull::TIMESTAMP {
        // 0Np
        qnull_base::J
    } else if timestamp == *qninf::TIMESTAMP {
        // -0Wp
        qninf_base::J
    } else if timestamp >= *qinf::TIMESTAMP {
        // 0Wp
        qinf_base::J
    } else {
        timestamp
            .timestamp_nanos_opt()
            .unwrap()
            .saturating_sub(KDB_TIMESTAMP_OFFSET)
    }
}

/// Convert `Date<Utc>` into `i32`. The returned value is an elapsed time in months since `2000.01.01`.
fn date_to_q_month(month: NaiveDate) -> i32 {
    // q     |------------------------------------------------------|
    // Rust        |----------------------------------------|

    if month == qnull::MONTH {
        // 0Nm
        qnull_base::I
    } else if month == *qninf::MONTH {
        // -0Wm
        qninf_base::I
    } else if month >= *qinf::MONTH {
        // 0Wm
        qinf_base::I
    } else {
        let months = (month.year() - 1970) * 12 + month.month0() as i32;
        months.saturating_sub(KDB_MONTH_OFFSET)
    }
}

/// Convert `Date<Utc>` into `i32`. The returned value is an elapsed time in days since `2000.01.01`.
fn date_to_q_date(date: NaiveDate) -> i32 {
    // q     |------------------------------------------------------|
    // Rust        |-----------------------------------------|

    if date == qnull::DATE {
        // 0Nd
        qnull_base::I
    } else if date == *qninf::DATE {
        // -0Wd
        qninf_base::I
    } else if date == qinf::DATE {
        // 0Wd
        qinf_base::I
    } else {
        let days =
            NaiveDate::signed_duration_since(date, NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
                .num_days() as i32;
        days.saturating_sub(KDB_DAY_OFFSET)
    }
}

/// Convert `Date<Utc>` into `i32`. The returned value is an elapsed time in days since `2000.01.01`.
fn datetime_to_q_datetime(datetime: DateTime<Utc>) -> f64 {
    // q     |------------------------------------------------------|
    // Rust        |-----------------------------------------|

    if datetime == qnull::DATETIME {
        // 0Nz
        qnull_base::F
    } else if datetime <= *qninf::DATETIME {
        // -0Wz
        qninf_base::F
    } else if datetime >= *qinf::DATETIME {
        // 0Wz
        qinf_base::F
    } else {
        let millis = datetime.timestamp_millis() as f64 / ONE_DAY_MILLIS as f64;
        millis - KDB_DAY_OFFSET as f64
    }
}

//%% Getter //%%vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Convert q timestamp (elapsed time in nanoseconds since `2000.01.01D00:00:00`) into `DateTime<Utc>`.
pub(crate) fn q_timestamp_to_datetime(nanos: i64) -> DateTime<Utc> {
    // q          |----------------------------------------|
    // Rust  |----------------------------------------------------|

    // Add duration to avoid overflow
    Utc.timestamp_nanos(nanos) + Duration::nanoseconds(KDB_TIMESTAMP_OFFSET)
}

/// Convert q month (elapsed time in months since `2000.01.01`) into `Date<Utc>`.
pub(crate) fn q_month_to_date(months: i32) -> NaiveDate {
    // q     |------------------------------------------------------|
    // Rust        |-----------------------------------------|

    if months == qnull_base::I {
        qnull::MONTH
    } else if months <= -3171072 {
        // Consider pulling month value from q, not only reverse Rust->q.
        // Convert Date::signed_duration_since(chrono::MIN_DATE, Utc.ymd(2000, 1,1)).num_days()) into months
        //  with 1461 as 4 years, 36525 as 100 years and 146097 as 400 years
        *qninf::MONTH
    } else if months >= 3121728 {
        // Consider pulling month value from q, not only reverse Rust->q.
        // Convert Date::signed_duration_since(chrono::MAX_DATE - Duration::days(30), Utc.ymd(2000, 1,1)).num_days()) into months
        //  with 1461 as 4 years, 36525 as 100 years and 146097 as 400 years
        *qinf::MONTH
    } else {
        NaiveDate::from_ymd_opt(2000 + months / 12, 1 + (months % 12) as u32, 1).unwrap()
    }
}

/// Convert q month (elapsed time in days since `2000.01.01`) into `Date<Utc>`.
pub(crate) fn q_date_to_date(days: i32) -> Result<NaiveDate> {
    // q     |------------------------------------------------------|
    // Rust        |-----------------------------------------|

    if days == qnull_base::I {
        Ok(qnull::DATE)
    } else if days <= -96476615 {
        // Consider pulling date value from q, not only reverse Rust->q.
        // Date::signed_duration_since(chrono::MIN_DATE, Utc.ymd(2000, 1,1)).num_days())
        Ok(*qninf::DATE)
    } else if days >= 95015644 {
        // Consider pulling date value from q, not only reverse Rust->q.
        // Date::signed_duration_since(chrono::MAX_DATE, Utc.ymd(2000, 1,1)).num_days())
        Ok(qinf::DATE)
    } else {
        Ok((NaiveDate::from_ymd_opt(2000, 1, 1)
            .ok_or_else(|| Error::InvalidDateTime)?
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| Error::InvalidDateTime)?
            .and_local_timezone(Utc)
            .unwrap()
            + Duration::days(days as i64))
        .date_naive())
    }
}

/// Convert q datetime (elapsed time in days with glanularity of milliseconds since `2000.01.01T00:00:00`) into `DateTime<Utc>`.
pub(crate) fn q_datetime_to_datetime(days: f64) -> DateTime<Utc> {
    // q     |------------------------------------------------------|
    // Rust        |-----------------------------------------|

    if days.is_nan() {
        qnull::DATETIME
    } else if days <= -96476615_f64 {
        // Consider pulling datetime value from q, not only reverse Rust->q.
        // DateTime::signed_duration_since(chrono::MIN_DATETIME, Utc.ymd(2000,1,1).and_hms_nano(0, 0, 0, 0)).num_days())
        *qninf::DATETIME
    } else if days >= 95015644_f64 {
        // Consider pulling datetime value from q, not only reverse Rust->q.
        // DateTime::signed_duration_since(chrono::MAX_DATETIME, Utc.ymd(2000,1,1).and_hms_nano(0, 0, 0, 0)).num_days())
        *qinf::DATETIME
    } else {
        Utc.timestamp_millis_opt((ONE_DAY_MILLIS as f64 * (days + KDB_DAY_OFFSET as f64)) as i64)
            .unwrap()
    }
}

/// Convert q timespan into `Duration`.
pub(crate) fn q_timespan_to_duration(nanos: i64) -> Duration {
    Duration::nanoseconds(nanos)
}

/// Convert q minute into `Duration`.
pub(crate) fn q_minute_to_duration(minutes: i32) -> Duration {
    Duration::minutes(minutes as i64)
}

/// Convert q second into `Duration`.
pub(crate) fn q_second_to_duration(seconds: i32) -> Duration {
    Duration::seconds(seconds as i64)
}

/// Convert q time into `Duration`.
pub(crate) fn q_time_to_duration(millis: i32) -> Duration {
    Duration::milliseconds(millis as i64)
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Load Modules
//++++++++++++++++++++++++++++++++++++++++++++++++++//

mod connection;
mod deserialize;
mod format;
mod serialize;
// Inject into `ipc` namespace.
pub use connection::*;
