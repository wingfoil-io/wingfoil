//! This module exposes bare C API functions. As most of them are provided with a "safe" wrapper
//!  function with an intuitive name and intuitive implementation for Rust, there is no gain to
//!  darely use these functions.
//!
//! The only exceptions are `k` and `knk` which are not provided with a "safe" wrapper because
//!  these functions are using elipsis (`...`) as their argument.

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Load Libraries
//++++++++++++++++++++++++++++++++++++++++++++++++++//

use super::{const_S, F, I, J, K, S, U, V};

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> External C Functions
//++++++++++++++++++++++++++++++++++++++++++++++++++//

extern "C" {

    //%% Constructors %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

    /// Creates an atom of the specified type.
    pub fn ka(qtype: I) -> K;

    /// Constructor of q bool object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_bool(_: K) -> K{
    ///   unsafe{kb(1)}
    /// }
    /// ```
    /// ```q
    /// q)yes: libc_api_examples 2: (`create_bool; 1);
    /// q)yes[]
    /// 1b
    /// ```
    pub fn kb(boolean: I) -> K;

    /// Constructor of q GUID object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_guid(_: K) -> K{
    ///   unsafe{ku(U::new([0x1e_u8, 0x11, 0x17, 0x0c, 0x42, 0x24, 0x25, 0x2c, 0x1c, 0x14, 0x1e, 0x22, 0x4d, 0x3d, 0x46, 0x24]))}
    /// }
    /// ```
    /// ```q
    /// q)create_guid: libc_api_examples 2: (`create_guid; 1);
    /// q)create_guid[]
    /// 1e11170c-4224-252c-1c14-1e224d3d4624
    /// ```
    pub fn ku(array: U) -> K;

    /// Constructor of q byte object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_byte(_: K) -> K{
    ///   unsafe{kg(0x3c)}
    /// }
    /// ```
    /// ```q
    /// q)create_byte: libc_api_examples 2: (`create_byte; 1);
    /// q)create_byte[]
    /// 0x3c
    /// ```
    pub fn kg(byte: I) -> K;

    /// Constructor of q short object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_short(_: K) -> K{
    ///   unsafe{kh(-144)}
    /// }
    /// ```
    /// ```q
    /// q)shortage: libc_api_examples 2: (`create_short; 1);
    /// q)shortage[]
    /// -144h
    /// ```
    pub fn kh(short: I) -> K;

    /// Constructor of q int object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_int(_: K) -> K{
    ///   unsafe{ki(86400000)}
    /// }
    /// ```
    /// ```q
    /// q)trvial: libc_api_examples 2: (`create_int; 1);
    /// q)trivial[]
    /// 86400000i
    /// ```
    pub fn ki(int: I) -> K;

    /// Constructor of q long object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_long(_: K) -> K{
    ///   unsafe{kj(-668541276001729000)}
    /// }
    /// ```
    /// ```q
    /// q)lengthy: libc_api_examples 2: (`create_long; 1);
    /// q)lengthy[]
    /// -668541276001729000
    /// ```
    pub fn kj(long: J) -> K;

    /// Constructor of q real object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_real(_: K) -> K{
    ///   unsafe{ke(0.00324)}
    /// }
    /// ```
    /// ```q
    /// q)reality: libc_api_examples 2: (`create_real; 1);
    /// q)reality[]
    /// 0.00324e
    /// ```
    pub fn ke(real: F) -> K;

    /// Constructor of q float object.
    /// # Example
    /// ```
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_float(_: K) -> K{
    ///   unsafe{kf(-6302.620)}
    /// }
    /// ```
    /// ```q
    /// q)coffee_float: libc_api_examples 2: (`create_float; 1);
    /// q)coffee_float[]
    /// -6302.62
    /// ```
    pub fn kf(float: F) -> K;

    ///  Constructor of q char object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_char(_: K) -> K{
    ///   unsafe{kc('q' as I)}
    /// }
    /// ```
    /// ```q
    /// q)quiz: libc_api_examples 2: (`create_char; 1);
    /// q)quiz[]
    /// "q"
    /// ```
    pub fn kc(character: I) -> K;

    /// Constructor of q symbol object.
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_symbol(_: K) -> K{
    ///   unsafe{ks(str_to_S!("symbolism"))}
    /// }
    /// ```
    /// ```q
    /// q)formal: libc_api_examples 2: (`create_symbol; 1);
    /// q)formal[]
    /// `symbolism
    /// q)`symbolism ~ formal[]
    /// 1b
    /// ```
    pub fn ks(symbol: S) -> K;

    /// Constructor of q timestamp from elapsed time in nanoseconds since kdb+ epoch (`2000.01.01`) or timespan object from nanoseconds.
    /// ```no_run
    /// use kdbplus::qtype;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_timestamp(_: K) -> K{
    ///   // 2015.03.16D00:00:00:00.000000000
    ///   unsafe{ktj(qtype::TIMESTAMP_ATOM as I, 479779200000000000)}
    /// }
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_timespan(_: K) -> K{
    ///   // -1D01:30:00.001234567
    ///   unsafe{ktj(qtype::TIMESPAN_ATOM as I, -91800001234567)}
    /// }
    /// ```
    /// ```q
    /// q)hanko: libc_api_examples 2: (`create_timestamp; 1);
    /// q)hanko[]
    /// 2015.03.16D00:00:00.000000000
    /// q)duration: libc_api_examples 2: (`create_timespan; 1);
    /// q)duration[]
    /// -1D01:30:00.001234567
    /// ```
    pub fn ktj(qtype: I, nanoseconds: J) -> K;

    /// Constructor of q date object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_date(_: K) -> K{
    ///   // 1999.12.25
    ///   unsafe{kd(-7)}
    /// }
    /// ```
    /// ```q
    /// q)christmas_at_the_END: libc_api_examples 2: (`create_date; 1);
    /// q)christmas_at_the_END[]
    /// 1999.12.25
    /// ```
    pub fn kd(date: I) -> K;

    /// Constructor of q datetime object from the number of days since kdb+ epoch (`2000.01.01`).
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_datetime(_: K) -> K{
    ///   // 2015.03.16T12:00:00:00.000
    ///   unsafe{kz(5553.5)}
    /// }
    /// ```
    /// ```q
    /// q)omega_date: libc_api_examples 2: (`create_datetime; 1);
    /// q)omega_date[]
    /// 2015.03.16T12:00:00.000
    /// ```
    pub fn kz(datetime: F) -> K;

    /// Constructor of q time object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_time(_: K) -> K{
    ///   // -01:30:00.123
    ///   unsafe{kt(-5400123)}
    /// }
    /// ```
    /// ```q
    /// q)ancient: libc_api_examples 2: (`create_time; 1);
    /// q)ancient[]
    /// -01:30:00.123
    /// ```
    pub fn kt(milliseconds: I) -> K;

    /// Constructor of q compound list.
    /// # Example
    /// See the example of [`xD`](fn.xD.html).
    pub fn knk(qtype: I, ...) -> K;

    /// Constructor of q simple list.
    /// # Example
    /// See the example of [`xD`](fn.xD.html).
    pub fn ktn(qtype: I, length: J) -> K;

    /// Constructor of q string object.
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_string(_: K) -> K{
    ///   unsafe{kp(str_to_S!("this is a text."))}
    /// }
    /// ```
    /// ```q
    /// q)text: libc_api_examples 2: (`create_string; 1);
    /// q)text[]
    /// "this is a text."
    /// ```
    pub fn kp(chararray: S) -> K;

    /// Constructor if q string object with a fixed length.
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_string2(_: K) -> K{
    ///   unsafe{kpn(str_to_S!("The meeting was too long and I felt it s..."), 24)}
    /// }
    /// ```
    /// ```q
    /// q)speak_inwardly: libc_api_examples 2: (`create_string2; 1);
    /// q)speak_inwardly[]
    /// "The meeting was too long"
    /// ```
    pub fn kpn(chararray: S, length: J) -> K;

    /// Constructor of q table object from q dictionary object.
    /// # Note
    /// Basically this is a `flip` command of q. Hence the value of the dictionary must have
    ///  lists as its elements.
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::qtype;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_table(_: K) -> K{
    ///   let keys=unsafe{ktn(qtype::SYMBOL_LIST as I, 2)};
    ///   let keys_slice=keys.as_mut_slice::<S>();
    ///   keys_slice[0]=unsafe{ss(str_to_S!("time"))};
    ///   keys_slice[1]=unsafe{ss(str_to_S!("temperature"))};
    ///   let values=unsafe{knk(2)};
    ///   let time=unsafe{ktn(qtype::TIMESTAMP_LIST as I, 3)};
    ///   // 2003.10.10D02:24:19.167018272 2006.05.24D06:16:49.419710368 2008.08.12D23:12:24.018691392
    ///   time.as_mut_slice::<J>().copy_from_slice(&[119067859167018272_i64, 201766609419710368, 271897944018691392]);
    ///   let temperature=unsafe{ktn(qtype::FLOAT_LIST as I, 3)};
    ///   temperature.as_mut_slice::<F>().copy_from_slice(&[22.1_f64, 24.7, 30.5]);
    ///   values.as_mut_slice::<K>().copy_from_slice(&[time, temperature]);
    ///   unsafe{xT(xD(keys, values))}
    /// }
    /// ```
    /// ```q
    /// q)climate_change: libc_api_examples 2: (`create_table; 1);
    /// q)climate_change[]
    /// time                          temperature
    /// -----------------------------------------
    /// 2003.10.10D02:24:19.167018272 22.1       
    /// 2006.05.24D06:16:49.419710368 24.7       
    /// 2008.08.12D23:12:24.018691392 30.5    
    /// ```
    pub fn xT(dictionary: K) -> K;

    /// Constructor of simple q table object from q keyed table object.
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::qtype;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_table(_: K) -> K{
    ///   let keys=unsafe{ktn(qtype::SYMBOL_LIST as I, 2)};
    ///   let keys_slice=keys.as_mut_slice::<S>();
    ///   keys_slice[0]=unsafe{ss(str_to_S!("time"))};
    ///   keys_slice[1]=unsafe{ss(str_to_S!("temperature"))};
    ///   let values=unsafe{knk(2)};
    ///   let time=unsafe{ktn(qtype::TIMESTAMP_LIST as I, 3)};
    ///   // 2003.10.10D02:24:19.167018272 2006.05.24D06:16:49.419710368 2008.08.12D23:12:24.018691392
    ///   time.as_mut_slice::<J>().copy_from_slice(&[119067859167018272_i64, 201766609419710368, 271897944018691392]);
    ///   let temperature=unsafe{ktn(qtype::FLOAT_LIST as I, 3)};
    ///   temperature.as_mut_slice::<F>().copy_from_slice(&[22.1_f64, 24.7, 30.5]);
    ///   values.as_mut_slice::<K>().copy_from_slice(&[time, temperature]);
    ///   unsafe{xT(xD(keys, values))}
    /// }
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_keyed_table(dummy: K) -> K{
    ///   unsafe{knt(1, create_table(dummy))}
    /// }
    ///
    /// #[no_mangle]
    /// pub extern "C" fn keyed_to_simple_table(dummy: K) -> K{
    ///   unsafe{ktd(create_keyed_table(dummy))}
    /// }
    /// ```
    /// ```q
    /// q)unkey: libc_api_examples 2: (`keyed_to_simple_table; 1);
    /// q)unkey[]
    /// time                          temperature
    /// -----------------------------------------
    /// 2003.10.10D02:24:19.167018272 22.1       
    /// 2006.05.24D06:16:49.419710368 24.7       
    /// 2008.08.12D23:12:24.018691392 30.5    
    /// ```
    pub fn ktd(keyedtable: K) -> K;

    /// Constructor of q keyed table object.
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::qtype;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_table(_: K) -> K{
    ///   let keys=unsafe{ktn(qtype::SYMBOL_LIST as I, 2)};
    ///   let keys_slice=keys.as_mut_slice::<S>();
    ///   keys_slice[0]=unsafe{ss(str_to_S!("time"))};
    ///   keys_slice[1]=unsafe{ss(str_to_S!("temperature"))};
    ///   let values=unsafe{knk(2)};
    ///   let time=unsafe{ktn(qtype::TIMESTAMP_LIST as I, 3)};
    ///   // 2003.10.10D02:24:19.167018272 2006.05.24D06:16:49.419710368 2008.08.12D23:12:24.018691392
    ///   time.as_mut_slice::<J>().copy_from_slice(&[119067859167018272_i64, 201766609419710368, 271897944018691392]);
    ///   let temperature=unsafe{ktn(qtype::FLOAT_LIST as I, 3)};
    ///   temperature.as_mut_slice::<F>().copy_from_slice(&[22.1_f64, 24.7, 30.5]);
    ///   values.as_mut_slice::<K>().copy_from_slice(&[time, temperature]);
    ///   unsafe{xT(xD(keys, values))}
    /// }
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_keyed_table(dummy: K) -> K{
    ///   unsafe{knt(1, create_table(dummy))}
    /// }
    /// ```
    /// ```q
    /// q)locker: libc_api_examples 2: (`create_keyed_table; 1);
    /// q)locker[]
    /// time                         | temperature
    /// -----------------------------| -----------
    /// 2003.10.10D02:24:19.167018272| 22.1       
    /// 2006.05.24D06:16:49.419710368| 24.7       
    /// 2008.08.12D23:12:24.018691392| 30.5  
    /// ```
    pub fn knt(keynum: J, table: K) -> K;

    /// Constructor of q dictionary object.
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::qtype;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_dictionary() -> K{
    ///   let keys=unsafe{ktn(qtype::INT_LIST as I, 2)};
    ///   keys.as_mut_slice::<I>()[0..2].copy_from_slice(&[0, 1]);
    ///   let values=unsafe{knk(2)};
    ///   let date_list=unsafe{ktn(qtype::DATE_LIST as I, 3)};
    ///   // 2000.01.01 2000.01.02 2000.01.03
    ///   date_list.as_mut_slice::<I>()[0..3].copy_from_slice(&[0, 1, 2]);
    ///   let string=unsafe{kp(str_to_S!("I'm afraid I would crash the application..."))};
    ///   values.as_mut_slice::<K>()[0..2].copy_from_slice(&[date_list, string]);
    ///   unsafe{xD(keys, values)}
    /// }
    /// ```
    /// ```q
    /// q)create_dictionary: `libc_api_examples 2: (`create_dictionary; 1);
    /// q)create_dictionary[]
    /// 0| 2000.01.01 2000.01.02 2000.01.03
    /// 1| "I'm afraid I would crash the application..."
    /// ```
    pub fn xD(keys: K, values: K) -> K;

    /// Constructor of q error.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// pub extern "C" fn thai_kick(_: K) -> K{
    ///   unsafe{
    ///     krr(null_terminated_str_to_const_S("Thai kick unconditionally!!\0"))
    ///   }
    /// }
    /// ```
    /// ```q
    /// q)monstrous: `libc_api_examples 2: (`thai_kick; 1);
    /// q)monstrous[]
    /// 'Thai kick unconditionally!!
    /// [0]  monstrous[]
    ///      ^
    /// ```
    pub fn krr(message: const_S) -> K;

    /// Similar to krr but this function appends a system-error message to string S before passing it to `krr`.
    pub fn orr(message: const_S) -> K;

    /// Add a raw value to a q simple list and returns a pointer to the (potentially reallocated) `K` object.
    /// # Example
    /// ```no_run
    /// use kdbplus::*;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_simple_list(_: K) -> K{
    ///   let mut list=unsafe{ktn(qtype::TIMESTAMP_LIST as I, 0)};
    ///   for i in 0..5{
    ///     let mut timestamp=86400000000000 * i as J;
    ///     unsafe{ja(&mut list, std::mem::transmute::<*mut J, *mut V>(&mut timestamp))};
    ///   }
    ///   list
    /// }
    /// ```
    /// # Note
    /// For symbol list, use [`js`](#fn.js).
    pub fn ja(list: *mut K, value: *mut V) -> K;

    /// Append a q list object to a q list.
    ///  Returns a pointer to the (potentially reallocated) `K` object.
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn concat_list(mut list1: K, list2: K) -> K{
    ///   unsafe{
    ///     jv(&mut list1, list2);
    ///     r1(list1)
    ///   }
    /// }
    /// ```
    /// ```q
    /// q)glue: `libc_api_examples 2: (`concat_list; 2);
    /// q)glue[(::; `metals; `fire); ("clay"; 316)]
    /// ::
    /// `metals
    /// `fire
    /// "clay"
    /// 316
    /// q)glue[1 2 3; 4 5]
    /// 1 2 3 4 5
    /// q)glue[`a`b`c; `d`e]
    /// `a`b`c`d`e
    /// ```
    pub fn jv(list1: *mut K, list2: K) -> K;

    /// Add a q object to a q compound list.
    ///  Returns a pointer to the (potentially reallocated) `K` object.
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_compound_list(_: K) -> K{
    ///   unsafe{
    ///     let mut list=knk(0);
    ///     jk(&mut list, ks(str_to_S!("1st")));
    ///     jk(&mut list, ki(2));
    ///     jk(&mut list, kpn(str_to_S!("3rd"), "3rd".chars().count() as i64));
    ///     list
    ///   }
    /// }
    /// ```
    /// ```q
    /// q)ranks: `libc_api_examples 2: (`create_compound_list; 1);
    /// q)ranks[]
    /// `1st
    /// 2i
    /// "3rd"
    /// ```
    /// # Note
    /// In this example we did not allocate an array as `knk(0)` to use `jk`. As `knk` initializes the
    ///  internal list size `n` with its argument, preallocating memory with `knk` and then using `jk` will crash.
    ///  If you want to allocate a memory in advance, you can substitute a value after converting
    ///  the q list object into a slice with [`as_mut_slice`](../trait.KUtility.html#tymethod.as_mut_slice).
    pub fn jk(list: *mut K, value: K) -> K;

    /// Add an enumerated character array to a symbol list.
    ///  Returns a pointer to the (potentially reallocated) `K` object.
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::qtype;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn create_symbol_list(_: K) -> K{
    ///   unsafe{
    ///     let mut list=ktn(qtype::SYMBOL_LIST as I, 0);
    ///     js(&mut list, ss(str_to_S!("Abraham")));
    ///     js(&mut list, ss(str_to_S!("Isaac")));
    ///     js(&mut list, ss(str_to_S!("Jacob")));
    ///     js(&mut list, sn(str_to_S!("Josephine"), 6));
    ///     list
    ///   }
    /// }
    /// ```
    /// ```q
    /// q)summon:`libc_api_examples 2: (`create_symbol_list; 1)
    /// q)summon[]
    /// `Abraham`Isaac`Jacob`Joseph
    /// q)`Abraham`Isaac`Jacob`Joseph ~ summon[]
    /// 1b
    /// ```
    /// # Note
    /// In this example we did not allocate an array as `ktn(qtype::SYMBOL_LIST as I, 0)` to use `js`. As `ktn` initializes
    ///  the internal list size `n` with its argument, preallocating memory with `ktn` and then using `js` will crash.
    ///  If you want to allocate a memory in advance, you can substitute a value after converting the q list object
    ///  into a slice with [`as_mut_slice`](../trait.KUtility.html#tymethod.as_mut_slice).
    pub fn js(list: *mut K, symbol: S) -> K;

    /// Enumerate  the first `n` chars from a character array.
    ///  Returns the same character array as an input and must be used to add character array to a symbol list.
    /// # Example
    /// See the example of [`js`](fn.js.html).
    pub fn sn(string: S, n: I) -> S;

    /// Enuemrate a null-terminated character array.
    ///  Returns the same character array as an input and must be used to add character array to a symbol list.
    /// # Example
    /// See the example of [`js`](fn.js.html).
    pub fn ss(string: S) -> S;

    /// Capture (and reset) error string into usual error object.
    /// # Example
    /// ```no_run
    /// use kdbplus::qtype;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// extern "C" fn catchy(func: K, args: K) -> K{
    ///   unsafe{
    ///     let result=ee(dot(func, args));
    ///     if (*result).qtype == qtype::ERROR{
    ///       println!("error: {}", S_to_str((*result).value.symbol));
    ///       // Decrement reference count of the error object
    ///       r0(result);
    ///       KNULL
    ///     }
    ///     else{
    ///       result
    ///     }
    ///   }
    /// }
    /// ```
    /// ```q
    /// q)catchy: `libc_api_examples 2: (`catchy; 2);
    /// q)catchy[$; ("J"; "42")]
    /// 42
    /// q)catchy[+; (1; `a)]
    /// error: type
    /// ```
    pub fn ee(result: K) -> K;

    //%% IPC Functions %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvv/

    /// Send a text query or evaluate the text query in a process which are loading the shared library.
    ///  As this library is purposed to build shared object, the only choice of `socket` is `0`. This
    ///  executes against the kdb+ process in which it is loaded.
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::qtype;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn dictionary_list_to_table() -> K{
    ///   let dicts=unsafe{knk(3)};
    ///   let dicts_slice=dicts.as_mut_slice::<K>();
    ///   for i in 0..3{
    ///     let keys=unsafe{ktn(qtype::SYMBOL_LIST as I, 2)};
    ///     let keys_slice=keys.as_mut_slice::<S>();
    ///     keys_slice[0]=unsafe{ss(str_to_S!("a"))};
    ///     keys_slice[1]=unsafe{ss(str_to_S!("b"))};
    ///     let values=unsafe{ktn(qtype::INT_LIST as I, 4)};
    ///     values.as_mut_slice::<I>()[0..2].copy_from_slice(&[i*10, i*100]);
    ///     dicts_slice[i as usize]=unsafe{xD(keys, values)};
    ///   }
    ///    // Format list of dictionary as a table.
    ///    // ([] a: 0 10 20i; b: 0 100 200i)
    ///    unsafe{k(0, str_to_S!("{[dicts] -1 _ dicts, (::)}"), dicts, KNULL)}
    /// }
    /// ```
    /// ```q
    /// q)unfortunate_fact: `libc_api_examples 2: (`dictionary_list_to_table; 1);
    /// q)unfortunate_fact[]
    /// a  b  
    /// ------
    /// 0  0  
    /// 10 100
    /// 20 200
    /// ```
    pub fn k(socket: I, query: const_S, ...) -> K;

    /// Serialize q object and return serialized q byte list object on success: otherwise null.
    ///  Mode is either of:
    /// - -1: Serialize within the same process.
    /// - 1: retain enumerations, allow serialization of timespan and timestamp: Useful for passing data between threads
    /// - 2: unenumerate, allow serialization of timespan and timestamp
    /// - 3: unenumerate, compress, allow serialization of timespan and timestamp
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn conceal(object: K)->K{
    ///   unsafe{b9(3, object)}
    /// }
    /// ```
    /// ```q
    /// q)jamming: `libc_api_examples 2: (`conceal; 1);
    /// q)jamming til 3
    /// 0x0100000026000000070003000000000000000000000001000000000000000200000000000000
    /// q)-9!jamming "Look! HE has come!!"
    /// "Look! HE has come!!"
    /// ```
    pub fn b9(mode: I, qobject: K) -> K;

    /// Deserialize a bytes into q object.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn reveal(bytes: K)->K{
    ///   unsafe{d9(bytes)}
    /// }
    /// ```
    /// ```q
    /// q)cancelling: `libc_api_examples 2: (`reveal; 1);
    /// q)cancelling -8!(`contact`from; "space"; 12);
    /// `contact`from
    /// "space"
    /// 12
    /// ```
    /// # Note
    /// On success, returns deserialized `K` object. On error, `(K) 0` is returned; use [`ee`](#fn.ee) to retrieve the error string.
    ///
    pub fn d9(bytes: K) -> K;

    /// Remove callback from the associated kdb+ socket and call `kclose`.
    ///  Return null if the socket is invalid or not the one which had been registered by `sd1`.
    /// # Note
    /// A function which calls this function must be executed at the exit of the process.
    pub fn sd0(socket: I) -> V;

    /// Remove callback from the associated kdb+ socket and call `kclose` if the given condition is satisfied.
    ///  Return null if the socket is invalid or not the one which had been registered by `sd1`.
    /// # Note
    /// A function which calls this function must be executed at the exit of the process.
    pub fn sd0x(socket: I, condition: I) -> V;

    /// Register callback to the associated kdb+ socket.
    /// ```no_run
    /// use kdbplus::qtype;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    /// use std::ffi::c_void;
    ///
    /// // Send asynchronous query to the q process which sent a query to the caller of this function.
    /// extern "C" fn counter(socket: I) -> K{
    ///   let extra_query="show `$\"Counter_punch!!\"".as_bytes();
    ///   let query_length=extra_query.len();
    ///   // header (8) + list header (6) + data length
    ///   let total_length=8+6+query_length;
    ///   // Buffer
    ///   let mut message: Vec<u8>=Vec::with_capacity(total_length);
    ///   // Little endian, async, uncompress, reserved
    ///   message.extend_from_slice(&[1_u8, 0, 0, 0]);
    ///   // Total message length
    ///   message.extend_from_slice(&(total_length as i32).to_le_bytes());
    ///   // Data type, attribute
    ///   message.extend_from_slice(&[10_u8, 0]);
    ///   // Length of data
    ///   message.extend_from_slice(&(query_length as i32).to_le_bytes());
    ///   // Data
    ///   message.extend_from_slice(extra_query);
    ///   // Send
    ///   unsafe{libc::send(socket, message.as_slice().as_ptr() as *const c_void, total_length, 0)};
    ///   KNULL
    /// }
    ///
    /// #[no_mangle]
    /// pub extern "C" fn enable_counter(socket: K) -> K{
    ///   unsafe{
    ///     let result=sd1(socket.get_int().expect("oh no"), counter);
    ///     if result.get_type()== qtype::NULL || result.get_type()== qtype::ERROR{
    ///       return krr(null_terminated_str_to_const_S("Failed to hook\0"));
    ///     }
    ///     else{
    ///       KNULL
    ///     }
    ///   }
    /// }
    /// ```
    /// ```q
    /// q)// process1
    /// q)enable_counter: `libc_api_examples 2: (`enable_counter; 1)
    /// q)\p 5000
    /// ```
    /// ```q
    /// q)// process2
    /// q)h:hopen `:unix://5000
    /// ```
    /// ```q
    /// q)// process1
    /// q).z.W
    /// 5|
    /// q)enable_counter[5i]
    /// ```
    /// ```q
    /// q)// process2
    /// q)h "1+2"
    /// `Counter_punch!!
    /// 3
    /// q)neg[h] "1+2"
    /// `Counter_punch!!
    /// ```
    pub fn sd1(socket: I, function: extern "C" fn(I) -> K) -> K;

    //%% Reference Count %%//vvvvvvvvvvvvvvvvvvvvvvvvvv/

    /// Decrement reference count of the q object. The decrement must be done when `k` function gets an error
    ///  object whose type is `qtype::ERROR` and when you created an object but do not intend to return it to
    ///  q side. See details on [the reference page](https://code.kx.com/q/interfaces/c-client-for-q/#managing-memory-and-reference-counting).
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn idle_man(_: K)->K{
    ///   unsafe{
    ///     // Creare an int object.
    ///     let int=ki(777);
    ///     // Changed the mind. Discard it.
    ///     r0(int);
    ///   }
    ///   // Return null.
    ///   KNULL
    /// }
    /// ```
    /// ```q
    /// q)idle_man: libc_api_examples 2: (`idle_man; 1);
    /// q)idle_man[]
    /// q)
    /// ```
    pub fn r0(qobject: K) -> V;

    /// Increment reference count of the q object. Increment must be done when you passed arguments
    ///  to Rust function and intends to return it to q side or when you pass some `K` objects to `k`
    ///  function and intend to use the parameter after the call.
    ///  See details on [the reference page](https://code.kx.com/q/interfaces/c-client-for-q/#managing-memory-and-reference-counting).
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn pass_through_cave(pedestrian: K) -> K{
    ///   let item=unsafe{k(0, str_to_S!("get_item1"), r1(pedestrian), KNULL)};
    ///   println!("What do you see, son of man?: {}", item.get_str().expect("oh no"));
    ///   unsafe{r0(item)};
    ///   let item=unsafe{k(0, str_to_S!("get_item2"), r1(pedestrian), KNULL)};
    ///   println!("What do you see, son of man?: {}", item.get_str().expect("oh no"));
    ///   unsafe{
    ///     r0(item);
    ///     r1(pedestrian)
    ///   }
    /// }
    /// ```
    /// ```q
    /// q)get_item1:{[man] "a basket of summer fruit"};
    /// q)get_item2:{[man] "boiling pot, facing away from the north"}
    /// q).capi.pass_through_cave[`son_of_man]
    /// What do you see, son of man?: a basket of summer fruit
    /// What do you see, son of man?: boiling pot, facing away from the north
    /// `son_of_man
    /// ```
    pub fn r1(qobject: K) -> K;

    //%% Miscellaneous %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvv/

    /// Apply a function to q list object `.[func; args]`.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn rust_parse(dollar: K, type_and_text: K) -> K{
    ///   unsafe{
    ///     dot(dollar, type_and_text)
    ///   }
    /// }
    /// ```
    /// ```q
    /// q)rust_parse:`libc_api_examples 2: (`rust_parse; 2);
    /// q)rust_parse[$; ("S"; "text")]
    /// `text
    /// ```
    pub fn dot(func: K, args: K) -> K;

    /// Release the memory allocated for the thread's pool.
    ///  Call when the thread is about to complete, releasing the memory allocated for that thread's pool.
    pub fn m9() -> V;

    /// Set whether interning symbols uses a lock: `lock` is either 0 or 1.
    ///  Returns the previously set value.
    /// # Example
    /// ```no_run
    /// #[macro_use]
    /// extern crate kdbplus;
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// #[no_mangle]
    /// pub extern "C" fn parallel_sym_change(list: K) -> K{
    ///   unsafe{
    ///     // `K` cannot have `Send` because it is a pointer but `k0` does.
    ///     let mut inner=*list;
    ///     // Lock symbol before creating an internal symbol on another thread.
    ///     setm(1);
    ///     let task=std::thread::spawn(move || {
    ///        inner.as_mut_slice::<S>()[0]=ss(str_to_S!("replaced"));
    ///        inner
    ///     });
    ///     list.as_mut_slice::<S>()[1]=ss(str_to_S!("symbolbol"));
    ///     match task.join(){
    ///       Err(_) => {
    ///         // Unlock.
    ///         setm(0);
    ///         krr(null_terminated_str_to_const_S("oh no"))
    ///       },
    ///       Ok(l) => {
    ///         // Unlock.
    ///         setm(0);
    ///         (*list)=l;
    ///         // Increment reference count for copy.
    ///         r1(list)
    ///       }
    ///     }
    ///   }
    /// }
    /// ```
    /// ```q
    /// q)alms_with_left_hand: `libc_api_examples 2: (`parallel_sym_change; 2);
    /// q)alms_with_left_hand[`a`b];
    /// `replaced`symbolbol
    /// ```
    pub fn setm(lock: I) -> I;

    /// Load C function as q function (`K` object).
    /// # Parameters
    /// - `func`: A function takes a C function that would take `n` `K` objects as arguments and returns a `K` object.
    /// - `n`: The number of arguments for the function.
    pub fn dl(func: *const V, n: J) -> K;

    /// Convert ymd to the number of days from `2000.01.01`.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// let days=unsafe{ymd(2020, 4, 1)};
    /// assert_eq!(days, 7396);
    /// ```
    pub fn ymd(year: I, month: I, date: I) -> I;

    /// Convert days from `2000.01.01` to a number expressed as `yyyymmdd`.
    /// # Example
    /// ```no_run
    /// use kdbplus::api::*;
    /// use kdbplus::api::native::*;
    ///
    /// let number=unsafe{dj(7396)};
    /// assert_eq!(number, 20200401);
    /// ```
    pub fn dj(days: I) -> I;

    /* Unsupported

    /// Connect with timeout (millisecond) and capability. The value of capability is:
    /// - 1: 1TB limit
    /// - 2: use TLS
    /// Return value is either of:
    /// - 0   Authentication error
    /// - -1   Connection error
    /// - -2   Timeout error
    /// - -3   OpenSSL initialization failed
    /// # Note
    /// Standalone application only. Not for a shared library.
    pub fn khpunc(host: S, port: I, credential: S, timeout_millis: I, capability: I) -> I;

    /// Connect with timeout (millisecond).
    ///  Return value is either of:
    /// - 0   Authentication error
    /// - -1   Connection error
    /// - -2   Timeout error
    /// # Note
    /// Standalone application only. Not for a shared library.
    pub fn khpun(host: const_S, port: I, credential: const_S, timeout_millis: I) -> I;

    /// Connect with no timeout.
    pub fn khpu(host: const_S, port: I, credential: const_S) -> I;

    /// Connect anonymously.
    pub fn khp(host: const_S, port: I) -> I;

    /// Close the socket to a q process.
    /// # Note
    /// Standalone application only. Not for a shared library.
    pub fn kclose(socket: I) -> V;

    /// Verify that the received bytes is a valid IPC message.
    ///  The message is not modified.
    ///  Returns `0` if not valid.
    /// # Note
    /// Decompressed data only.
    pub fn okx(bytes: K) -> I;

    /// Return a dictionary of TLS setting. See `-26!`.
    /// # Note
    /// As this library is purposed to build shared object, this function will not add a value.
    pub fn sslInfo(_: K) -> K;

    /// Return kdb+ release date.
    /// # Note
    /// This function seems not exist (`undefined symbol`).
    pub fn ver() -> I;

    /// Variadic version of `knk`.
    fn vaknk(qtype: I, args: va_list) -> K;

    /// Variadic version of `k`.
    fn vak(qtype: I, query: const_S, args: va_list) -> K;

    */
}
