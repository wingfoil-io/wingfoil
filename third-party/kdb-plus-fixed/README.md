
# kdb-plus-fixed

This a fork of [kdbplus](https://github.com/diamondrod/kdbplus), but with some critical fixes and without it's latest commits, which introduced some defects.   There is also [kxkdb](https://github.com/KxSystems/kxkdb), which is also a fork of kdbplus and has the same defects.  Neither of these project appear to be maintained any more.

## Fork Hierarchy

```
diamondrod/kdbplus          (original)
├── KxSystems/kxkdb         (KX fork — unmaintained)
└── rollo-b2c2/kdbplus      (intermediate fork)
    └── wingfoil-io/kdb-plus-fixed  (this repo — actively maintained)
```

# Rust Dual Interface for q/kdb+

As Rust is becoming a popular programming language for its performance and type safety, the desire to use it with still a maniac time-series database kdb+ is brewing. The aspiration is understandable since we know kdb+ is fast and its interface or a shared library should be fast as well. This interface was created to satisfy such a natural demand, furthermore, in a manner users do not feel any pain to use. The notrious esoteric function names of the q/kdb+ C API is not an interest of Rust developers.

  *"Give us a **Rust** interface!!"*

Here is your choice.

This interface provides two features:

- IPC interface (Rust client of q/kdb+ process)
- API (build a shared library for q/kdb+)

You can find the detail descriptions of each feature below.

## Rust IPC Interface of q/kdb+

As Rust was conceived to address type unsafety of C/C++, replacing C/C++ with Rust can happen if possible. This interface is purposed to be used as a Rust client of q/kdb+ process that sends a query and receives its response. Query to kdb+ is supported in two ways:

- text query
- functional query which is represented by a compound list of kdb+ ([See detail of IPC](https://code.kx.com/q4m3/11_IO/#116-interprocess-communication)).

Compression/decompression of messages is also implemented following [kdb+ implementation](https://code.kx.com/q/basics/ipc/#compression).

As for connect method, usually client interfaces of q/kdb+ do not provide a listener due to its protocol. However, sometimes Rust process is connecting to an upstream and q/kdb+ starts afterward or is restarted more frequently. Then providing a listener method is a natural direction and it was achieved here. Following ways are supported to connect to kdb+:

- TCP
- TLS
- Unix domain socket

Furthermore, in order to improve inter-operatability some casting, getter and setter methods are provided.

### Environmental Variables

This crate uses q-native or crate-specific environmental variables.

- `KDBPLUS_ACCOUNT_FILE`: A file path to a credential file which an acceptor loads in order to manage access from a q client. This file contains a user name and SHA-1 hashed password in each line which are delimited by `':'` without any space. For example, a file containing two credentials `"mattew:oracle"` and `"reluctant:slowday"` looks like this:

      mattew:431364b6450fc47ccdbf6a2205dfdb1baeb79412
      reluctant:d03f5cc1cdb11a77410ee34e26ca1102e67a893c

      
    The hashed password can be generated with q using a function `.Q.sha1`:
 
      q).Q.sha1 "slowday"
      0xd03f5cc1cdb11a77410ee34e26ca1102e67a893c
 
- `KDBPLUS_TLS_KEY_FILE` and `KDBPLUS_TLS_KEY_FILE_SECRET`: The pkcs12 file and its password which TLS acceptor uses.
- `QUDSPATH` (optional): q-native environmental variable to define an astract namespace. This environmental variable is used by UDS acceptor too. The abstract nameapace will be `@${QUDSPATH}/kx.[server process port]` if this environmental variable is defined; otherwise it will be `@/tmp/kx.[server process port]`.

*Notes:*

- Messages will be sent with OS native endian.
- When using this crate for a TLS client you need to set two environmental variables `KX_SSL_CERT_FILE` and `KX_SSL_KEY_FILE` on q side to make q/kdb+ to work as a TLS server. For details, see [the KX website](https://code.kx.com/q/kb/ssl/).

### Type Mapping

All types are expressed as `K` struct which is quite similar to the `K` struct of `api` module but its structure is optimized for IPC
usage and for the convenience to interact with. The table below shows the input types of each q type which is used to construct `K` object.
Note that the input type can be different from the inner type. For example, timestamp has an input type of `chrono::DateTime<Utc>` but
the inner type is `i64` denoting an elapsed time in nanoseconds since `2000.01.01D00:00:00`.

| q                | Rust                                              |
|------------------|---------------------------------------------------|
| `bool`           | `bool`                                            |
| `GUID`           | `[u8; 16]`                                        |
| `byte`           | `u8`                                              |
| `short`          | `i16`                                             |
| `int`            | `i32`                                             |
| `long`           | `i64`                                             |
| `real`           | `f32`                                             |
| `float`          | `f64`                                             |
| `char`           | `char`                                            |
| `symbol`         | `String`                                          |
| `timestamp`      | `chrono::DateTime<Utc>`                           |
| `month`          | `chrono::NaiveDate`                               |
| `date`           | `chrono::NaiveDate`                               |
| `datetime`       | `chrono::DateTime<Utc>`                           |
| `timespan`       | `chrono::Duration`                                |
| `minute`         | `chrono::Duration`                                |
| `second`         | `chrono::Duration`                                |
| `time`           | `chrono::Duration`                                |
| `list`           | `Vec<Item>` (`Item` is a corrsponding type above) |
| `compound list`  | `Vec<K>`                                          |
| `table`          | `Vec<K>`                                          |
| `dictionary`     | `Vec<K>`                                          |
| `null`           | `()`                                              |
 
### Examples

#### Client

```rust
use kdbplus::ipc::*;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {

    // Connect to qprocess running on localhost:5000 via UDS
    let mut socket = QStream::connect(ConnectionMethod::UDS, "", 5000_u16, "ideal:person").await?;
    println!("Connection type: {}", socket.get_connection_type());

    // Set remote function with asynchronous message
    socket.send_async_message(&"collatz:{[n] seq:enlist n; while[not n = 1; seq,: n:$[n mod 2; 1 + 3 * n; `long$n % 2]]; seq}").await?;

    // Send a query synchronously
    let mut result = socket.send_sync_message(&"collatz[12]").await?;
    println!("collatz[12]: {}", result);

    result = socket.send_sync_message(&"collatz[`a]").await?;
    println!("collatz[`a]: {}", result);

    // Send a functional query.
    let mut message = K::new_compound_list(vec![K::new_symbol(String::from("collatz")), K::new_long(100)]);
    result = socket.send_sync_message(&message).await?;
    println!("collatz[100]: {}", result);

    // Modify query to (`collatz; 20)
    message.pop().unwrap();
    message.push(&K::new_long(20)).unwrap();
    result=socket.send_sync_message(&message).await?;
    println!("collatz[20]: {}", result);

    // Send a functional asynchronous query.
    message = K::new_compound_list(vec![K::new_string(String::from("show"), qattribute::NONE), K::new_symbol(String::from("goodbye"))]);
    socket.send_async_message(&message).await?;

    socket.shutdown().await?;

    Ok(())
}
```

#### Listener

```rust
use kdbplus::ipc::*;

#[tokio::main]
async fn main() -> Result<()> {

  // Start listenening over TCP at the port 7000 with authentication enabled.
  let mut socket_tcp = QStream::accept(ConnectionMethod::TCP, "127.0.0.1", 7000).await?;

  // Send a query with the socket.
  let greeting = socket_tcp.send_sync_message(&"string `Hello").await?;
  println!("Greeting: {}", greeting);

  socket_tcp.shutdown().await?;

  Ok(())
}
```

Then q client can connect to this acceptor with the acceptor's host, port and the credential configured in `KDBPLUS_ACCOUNT_FILE`:

```q
q)h:hopen `::7000:reluctant:slowday
```

### Installation

Use `kdbplus` as a library name in `Cargo.toml` with `"ipc"` feature.

```toml
[dependencies]
kdbplus={version="^0.3", features=["ipc"]}
```

## Rust Wrapper of q/kdb+ C API

Programming language q (kdb+ is a database written in q) is providing only C API but sometimes an external library provides Rust interface but not C/C++ interface. From the fame of its performance, Rust still should be feasible to build a shared library for kdb+. This library is provided to address such a natural demand (desire, if you will). Since there is no way for everyone but creating a wrapper like this to write a shared library for kdb+, it probably make sense for someone to provide the wrapper, and it was done here.

In order to avoid writing too large `unsafe` block which leads to poor optimization, most of native C API functions are provided with a wrapper funtion with a bit of ergonomic safety and with intuitive implementation as a trait method. The only exceptions are `knk` and `k` which are using elipsis (`...`) as its argument. These functions are provided under `native` namespace with the other C API functions.

**Note:** This library is purposed to be used to build a sared library; therefore some unrelated functions are removed. For example, connection functions to kdb+ like `khpu` are not included.

### Installation

Use `kdbplus` as a library name in `Cargo.toml` with `"api"` feature.

```toml
[dependencies]
kdbplus={version="^0.3", features=["api"]}
```

### Examples

The examples of using C API wrapper are included in `api_examples` folder. The examples are mirroring the examples in the document of `kdbplus::api` module and the functions are also used for simple tests of the library. The test is conducted in the `test.q` under `tests/` by loading the functions defined in a shared library built from the examples.

Here are some examples:

#### C API Style

```rust
use kdbplus::qtype;
use kdbplus::api::*;
use kdbplus::api::native::*;

#[no_mangle]
pub extern "C" fn create_symbol_list(_: K) -> K {
    unsafe{
        let mut list=ktn(qtype::SYMBOL_LIST as i32, 0);
        js(&mut list, ss(str_to_S!("Abraham")));
        js(&mut list, ss(str_to_S!("Isaac")));
        js(&mut list, ss(str_to_S!("Jacob")));
        js(&mut list, sn(str_to_S!("Josephine"), 6));
        list
    }
}
 
#[no_mangle]
pub extern "C" fn catchy(func: K, args: K) -> K {
    unsafe{
        let result=ee(dot(func, args));
        if (*result).qtype == qtype::ERROR{
            println!("error: {}", S_to_str((*result).value.symbol));
            // Decrement reference count of the error object
            r0(result);
            KNULL
        } else {
            result
        }
    }
}

#[no_mangle]
pub extern "C" fn dictionary_list_to_table() -> K {
    unsafe{
        let dicts = knk(3);
        let dicts_slice = dicts.as_mut_slice::<K>();
        for i in 0..3 {
            let keys = ktn(qtype::SYMBOL_LIST as i32, 2);
            let keys_slice = keys.as_mut_slice::<S>();
            keys_slice[0] = ss(str_to_S!("a"));
            keys_slice[1] = ss(str_to_S!("b"));
            let values = ktn(qtype::INT_LIST as i32, 2);
            values.as_mut_slice::<I>()[0..2].copy_from_slice(&[i*10, i*100]);
            dicts_slice[i as usize] = xD(keys, values);
        }
        // Format list of dictionary as a table.
        // ([] a: 0 10 20i; b: 0 100 200i)
        k(0, str_to_S!("{[dicts] -1 _ dicts, (::)}"), dicts, KNULL)
    } 
}
```

q can use these functions like this:

```q
q)summon:`libc_api_examples 2: (`create_symbol_list; 1)
q)summon[]
`Abraham`Isaac`Jacob`Joseph
q)`Abraham`Isaac`Jacob`Joseph ~ summon[]
q)catchy: `libc_api_examples 2: (`catchy; 2);
q)catchy[$; ("J"; "42")]
42
q)catchy[+; (1; `a)]
error: type
q)behold: `libc_api_examples 2: (`dictionary_list_to_table; 1);
q)behold[]
a  b  
------
0  0  
10 100
20 200
```

#### Rust Style

The examples below are written without `unsafe` code. You can see how comfortably breathing are the wrapped functions in the code.

```rust
use kdbplus::qtype;
use kdbplus::api::*;
use kdbplus::api::native::*;

#[no_mangle]
pub extern "C" fn create_symbol_list2(_: K) -> K {
    let mut list = new_list(qtype::SYMBOL_LIST, 0);
    list.push_symbol("Abraham").unwrap();
    list.push_symbol("Isaac").unwrap();
    list.push_symbol("Jacob").unwrap();
    list.push_symbol_n("Josephine", 6).unwrap();
    list
}

#[no_mangle]
fn no_panick(func: K, args: K) -> K {
    let result = error_to_string(apply(func, args));
    if let Ok(error) = result.get_error_string() {
        println!("FYI: {}", error);
        // Decrement reference count of the error object which is no longer used.
        decrement_reference_count(result);
        KNULL
    }
    else{
        println!("success!");
        result
    }
}

#[no_mangle]
pub extern "C" fn create_table2(_: K) -> K {
    // Build keys
    let keys = new_list(qtype::SYMBOL_LIST, 2);
    let keys_slice = keys.as_mut_slice::<S>();
    keys_slice[0] = enumerate(str_to_S!("time"));
    keys_slice[1] = enumerate_n(str_to_S!("temperature_and_humidity"), 11);

    // Build values
    let values = new_list(qtype::COMPOUND_LIST, 2);
    let time = new_list(qtype::TIMESTAMP_LIST, 3);
    // 2003.10.10D02:24:19.167018272 2006.05.24D06:16:49.419710368 2008.08.12D23:12:24.018691392
    time.as_mut_slice::<J>().copy_from_slice(&[119067859167018272_i64, 201766609419710368, 271897944018691392]);
    let temperature = new_list(qtype::FLOAT_LIST, 3);
    temperature.as_mut_slice::<F>().copy_from_slice(&[22.1_f64, 24.7, 30.5]);
    values.as_mut_slice::<K>().copy_from_slice(&[time, temperature]);
    
    flip(new_dictionary(keys, values))
}
```

And q code is here:

```q
q)summon:`libc_api_examples 2: (`create_symbol_list2; 1)
q)summon[]
`Abraham`Isaac`Jacob`Joseph
q)chill: `libc_api_examples 2: (`no_panick; 2);
q)chill[$; ("J"; "42")]
success!
42
q)chill[+; (1; `a)]
FYI: type
q)climate_change: libc_api_examples 2: (`create_table2; 1);
q)climate_change[]
time                          temperature
-----------------------------------------
2003.10.10D02:24:19.167018272 22.1       
2006.05.24D06:16:49.419710368 24.7       
2008.08.12D23:12:24.018691392 30.5  
```

#### Test

Test is conducted in two ways:

1. Using cargo
2. Running a q test script

##### 1. Using Cargo

Before starting the test, you need to start a q process on the port 5000:

```bash
kdbplus]$ q -p 5000
q)
```

Then fire the cargo test:

```bash
kdbplus]$ cargo test
```

**Note:** Currently 20 tests fails for `api` examples in document. This is because the examples do not have `main` function by nature of `api` but still use `#[macro_use]`.

##### 2. Running a q Test Script

Tests are conducted with `tests/test.q` by loading the example functions built in `api_examples`.

```bash
kdbplus]$ cargo build
kdbplus]$ cp target/debug/libapi_examples.so tests/
kdbplus]$ cd tests
tests]$ q test.q
Initialized something, probably it is your mindset.
bool: true
bool: false
byte: 0xc4
GUID: 8c6b-8b-64-68-156084
short: 10
int: 42
int: 122
int: 7336
int: 723
int: 14240
int: 2056636
long: -109210
long: 43200123456789
long: -325389000000021
long: 0
real: 193810.31
float: -37017.09330000
float: 742.41927468
char: "k"
symbol: `locust
string: "gnat"
string: "grasshopper"
error: type
What do you see, son of man?: a basket of summer fruit
What do you see, son of man?: boiling pot, facing away from the north
symbol: `rust
success!
FYI: type
this is KNULL
Planet { name: "earth", population: 7500000000, water: true }
Planet { name: "earth", population: 7500000000, water: true }
おいしい！
おいしい！
おいしい！
おいしい！
おいしい！
おいしい！
おいしい！
おいしい！
おいしい！
おいしい！
"Collect the clutter of apples!"
test result: ok. 147 passed; 0 failed
q)What are the three largest elements?: `belief`love`hope
```

### Projects Using This Library

- [qrpc](https://github.com/diamondrod/qrpc) (gRPC client)
- [q_comtrade](https://github.com/diamondrod/q_comtrade) (COMTRADE file parser)

## Document

The document of this crate itself is on the [crates.io page](https://docs.rs/kdbplus/).

For details of C API itself, check the documents of KX website.

- [Refernce](https://code.kx.com/q/interfaces/capiref/)
- [Memory management](https://code.kx.com/q/interfaces/c-client-for-q/#managing-memory-and-reference-counting)
