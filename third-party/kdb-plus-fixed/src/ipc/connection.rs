//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Load Libraries
//++++++++++++++++++++++++++++++++++++++++++++++++++//

use super::serialize::ENCODING;
use super::Result;
use super::{qtype, K};
use async_trait::async_trait;
use io::BufRead;
use once_cell::sync::Lazy;
use sha1_smol::Sha1;
use std::collections::HashMap;
use std::convert::TryInto;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::{env, fs, io, str};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio_native_tls::native_tls::{
    Identity, TlsAcceptor as TlsAcceptorInner, TlsConnector as TlsConnectorInner,
};
use tokio_native_tls::{TlsAcceptor, TlsConnector, TlsStream};
use hickory_resolver::TokioResolver;

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Global Variable
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% QStream %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

pub mod qmsg_type {
    //! This module provides a list of q message type used for IPC.
    //!  The motivation to contain them in a module is to tie them up as related items rather
    //!  than scattered values. Hence user should use these indicators with `qmsg_type::` prefix, e.g., `qmsg_type::asynchronous`.
    //!
    //! # Example
    //! ```no_run
    //! use kdbplus::ipc::*;
    //!
    //! // Print `K` object.
    //! fn print(obj: &K) {
    //!     println!("{}", obj);
    //! }
    //!
    //! // Calculate something from two long arguments.
    //! fn nonsense(arg1: i64, arg2: i64) -> i64 {
    //!     arg1 * arg2
    //! }
    //!
    //! #[tokio::main]
    //! async fn main() -> Result<()> {
    //!     // Connect to qprocess running on localhost:5000 via TCP
    //!     let mut socket =
    //!         QStream::connect(ConnectionMethod::TCP, "localhost", 5000_u16, "ideal:person").await?;
    //!
    //!     // Set a function which sends back a non-response message during its execution.
    //!     socket
    //!         .send_async_message(
    //!             &"complex:{neg[.z.w](`print; \"counter\"); what: .z.w (`nonsense; 1; 2); what*100}",
    //!         )
    //!         .await?;
    //!
    //!     // Send a query `(`complex; ::)` without waiting for a response.
    //!     socket
    //!         .send_message(
    //!             &K::new_compound_list(vec![K::new_symbol(String::from("complex")), K::new_null()]),
    //!             qmsg_type::synchronous,
    //!         )
    //!         .await?;
    //!
    //!     // Receive an asynchronous call from the function.
    //!     match socket.receive_message().await {
    //!         Ok((qmsg_type::asynchronous, message)) => {
    //!             println!("asynchronous call: {}", message);
    //!             let list = message.as_vec::<K>().unwrap();
    //!             if list[0].get_symbol().unwrap() == "print" {
    //!                 print(&list[1])
    //!             }
    //!         }
    //!         _ => unreachable!(),
    //!     }
    //!
    //!     // Receive a synchronous call from the function.
    //!     match socket.receive_message().await {
    //!         Ok((qmsg_type::synchronous, message)) => {
    //!             println!("synchronous call: {}", message);
    //!             let list = message.as_vec::<K>().unwrap();
    //!             if list[0].get_symbol().unwrap() == "nonsense" {
    //!                 let res = nonsense(list[1].get_long().unwrap(), list[2].get_long().unwrap());
    //!                 // Send bach a response.
    //!                 socket
    //!                     .send_message(&K::new_long(res), qmsg_type::response)
    //!                     .await?;
    //!             }
    //!         }
    //!         _ => unreachable!(),
    //!     }
    //!
    //!     // Receive a final result.
    //!     match socket.receive_message().await {
    //!         Ok((qmsg_type::response, message)) => {
    //!             println!("final: {}", message);
    //!         }
    //!         _ => unreachable!(),
    //!     }
    //!
    //!     Ok(())
    //! }
    //!```
    /// Used to send a message to q/kdb+ asynchronously.
    pub const asynchronous: u8 = 0;
    /// Used to send a message to q/kdb+ synchronously.
    pub const synchronous: u8 = 1;
    /// Used by q/kdb+ to identify a response for a synchronous query.
    pub const response: u8 = 2;
}

//%% QStream Acceptor %%//vvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Map from user name to password hashed with SHA1.
#[allow(clippy::declare_interior_mutable_const)]
const ACCOUNTS: Lazy<HashMap<String, String>> = Lazy::new(|| {
    // Map from user to password
    let mut map: HashMap<String, String> = HashMap::new();
    // Open credential file
    let file = fs::OpenOptions::new()
        .read(true)
        .open(env::var("KDBPLUS_ACCOUNT_FILE").expect("KDBPLUS_ACCOUNT_FILE is not set"))
        .expect("failed to open account file");
    let mut reader = io::BufReader::new(file);
    let mut line = String::new();
    loop {
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let credential = line.as_str().split(':').collect::<Vec<&str>>();
                let mut password = credential[1];
                if password.ends_with('\n') {
                    password = &password[0..password.len() - 1];
                }
                map.insert(credential[0].to_string(), password.to_string());
                line.clear();
            }
            _ => unreachable!(),
        }
    }
    map
});

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Structs
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% ConnectionMethod %%//vvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Connection method to q/kdb+.
pub enum ConnectionMethod {
    TCP = 0,
    TLS = 1,
    /// Unix domanin socket.
    UDS = 2,
}

//%% Query %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Feature of query object.
#[async_trait]
pub trait Query: Send + Sync {
    /// Serialize into q IPC bytes including a header (encoding, message type, compresssion flag and total message length).
    ///   If the connection is within the same host, the message is not compressed under any conditions.
    /// # Parameters
    /// - `message_type`: Message type. One of followings:
    ///   - `qmsg_type::asynchronous`
    ///   - `qmsg_type::synchronous`
    ///   - `qmsg_type::response`
    /// - `is_local`: Flag of whether the connection is within the same host.
    async fn serialize(&self, message_type: u8, is_local: bool) -> Vec<u8>;
}

//%% QStreamInner %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Features which streams communicating with q must have.
#[async_trait]
trait QStreamInner: Send + Sync {
    /// Shutdown underlying stream.
    async fn shutdown(&mut self, is_server: bool) -> Result<()>;
    /// Send a message with a specified message type without waiting for a response.
    /// # Parameters
    /// - `message`: q command to execute on the remote q process.
    /// - `message_type`: Asynchronous or synchronous.
    /// - `is_local`: Flag of whether the connection is within the same host.
    async fn send_message(
        &mut self,
        message: &dyn Query,
        message_type: u8,
        is_local: bool,
    ) -> Result<()>;
    /// Send a message asynchronously.
    /// # Parameters
    /// - `message`: q command in two ways:
    ///   - `&str`: q command in a string form.
    ///   - `K`: Query in a functional form.
    /// - `is_local`: Flag of whether the connection is within the same host.
    async fn send_async_message(&mut self, message: &dyn Query, is_local: bool) -> Result<()>;
    /// Send a message asynchronously.
    /// # Parameters
    /// - `message`: q command in two ways:
    ///   - `&str`: q command in a string form.
    ///   - `K`: Query in a functional form.
    /// - `is_local`: Flag of whether the connection is within the same host.
    async fn send_sync_message(&mut self, message: &dyn Query, is_local: bool) -> Result<K>;
    /// Receive a message from a remote q process. The received message is parsed as `K` and message type is
    ///  stored in the first returned value.
    async fn receive_message(&mut self) -> Result<(u8, K)>;
}

//%% QStream %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Stream to communicate with q/kdb+.
pub struct QStream {
    /// Actual stream to communicate.
    stream: Box<dyn QStreamInner>,
    /// Connection method. One of followings:
    /// - TCP
    /// - TLS
    /// - UDS
    method: ConnectionMethod,
    /// Indicator of whether the stream is an acceptor or client.
    /// - `true`: Acceptor
    /// - `false`: Client
    listener: bool,
    /// Indicator of whether the connection is within the same host.
    /// - `true`: Connection within the same host.
    /// - `false`: Connection with outseide.
    local: bool,
}

//%% MessageHeader %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Header of q IPC data frame.
#[derive(Clone, Copy, Debug)]
struct MessageHeader {
    /// Ennoding.
    /// - 0: Big Endian
    /// - 1: Little Endian
    encoding: u8,
    /// Message type. One of followings:
    /// - 0: Asynchronous
    /// - 1: Synchronous
    /// - 2: Response
    message_type: u8,
    /// Indicator of whether the message is compressed or not.
    /// - 0: Uncompressed
    /// - 1: Compressed
    compressed: u8,
    /// Reserved byte.
    _unused: u8,
    /// Total length of the uncompressed message.
    length: u32,
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Implementation
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% Query %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Text query.
#[async_trait]
impl Query for &str {
    async fn serialize(&self, message_type: u8, _: bool) -> Vec<u8> {
        //  Build header //--------------------------------/
        // Message header + (type indicator of string + header of string type) + string length
        let byte_message = self.as_bytes();
        let message_length = byte_message.len() as u32;
        let total_length = MessageHeader::size() as u32 + 6 + message_length;

        let total_length_bytes = match ENCODING {
            0 => total_length.to_be_bytes(),
            _ => total_length.to_le_bytes(),
        };

        // encode, message type, 0x00 for compression and 0x00 for reserved.
        // Do not compress string data because it is highly unlikely that the length of the string query
        //  is greater than 2000.
        let mut message = Vec::with_capacity(message_length as usize + MessageHeader::size());
        message.extend_from_slice(&[ENCODING, message_type, 0, 0]);
        // total body length
        message.extend_from_slice(&total_length_bytes);
        // vector type and 0x00 for attribute
        message.extend_from_slice(&[qtype::STRING as u8, 0]);

        //  Build body //---------------------------------/
        let length_info = match ENCODING {
            0 => message_length.to_be_bytes(),
            _ => message_length.to_le_bytes(),
        };

        // length of vector(message)
        message.extend_from_slice(&length_info);
        // message
        message.extend_from_slice(byte_message);

        message
    }
}

/// Functional query.
#[async_trait]
impl Query for K {
    async fn serialize(&self, message_type: u8, is_local: bool) -> Vec<u8> {
        //  Build header //--------------------------------/
        // Message header + encoded data size
        let mut byte_message = self.q_ipc_encode();
        let message_length = byte_message.len();
        let total_length = (MessageHeader::size() + message_length) as u32;

        let total_length_bytes = match ENCODING {
            0 => total_length.to_be_bytes(),
            _ => total_length.to_le_bytes(),
        };

        // Compression is trigerred when entire message size is more than 2000 bytes
        //  and the connection is with outseide.
        if message_length > 1992 && !is_local {
            // encode, message type, 0x00 for compression, 0x00 for reserved and 0x00000000 for total size
            let mut message = Vec::with_capacity(message_length + 8);
            message.extend_from_slice(&[ENCODING, message_type, 0, 0, 0, 0, 0, 0]);
            message.append(&mut byte_message);
            // Try to encode entire message.
            match compress(message).await {
                (true, compressed) => {
                    // Message was compressed
                    return compressed;
                }
                (false, mut uncompressed) => {
                    // Message was not compressed.
                    // Write original total data size.
                    uncompressed[4..8].copy_from_slice(&total_length_bytes);
                    return uncompressed;
                }
            }
        } else {
            // encode, message type, 0x00 for compression and 0x00 for reserved
            let mut message = Vec::with_capacity(message_length + MessageHeader::size());
            message.extend_from_slice(&[ENCODING, message_type, 0, 0]);
            // Total length of body
            message.extend_from_slice(&total_length_bytes);
            message.append(&mut byte_message);
            return message;
        }
    }
}

//%% QStream %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl QStream {
    /// General constructor of `QStream`.
    fn new(
        stream: Box<dyn QStreamInner>,
        method: ConnectionMethod,
        is_listener: bool,
        is_local: bool,
    ) -> Self {
        QStream {
            stream,
            method,
            listener: is_listener,
            local: is_local,
        }
    }

    /// Connect to q/kdb+ specifying a connection method, destination host, destination port and access credential.
    /// # Parameters
    /// - `method`: Connection method. One of followings:
    ///   - TCP
    ///   - TLS
    ///   - UDS
    /// - `host`: Hostname or IP address of the target q process. Empty `str` for Unix domain socket.
    /// - `port`: Port of the target q process.
    /// - `credential`: Credential in the form of `username:password` to connect to the target q process.
    /// # Example
    /// ```
    /// use kdbplus::qattribute;
    /// use kdbplus::ipc::*;
    ///
    /// #[tokio::main(flavor = "multi_thread", worker_threads = 2)]
    /// async fn main() -> Result<()> {
    ///     let mut socket =
    ///         QStream::connect(ConnectionMethod::UDS, "", 5000_u16, "ideal:person").await?;
    ///     println!("Connection type: {}", socket.get_connection_type());
    ///
    ///     // Set remote function with asynchronous message
    ///     socket.send_async_message(&"collatz:{[n] seq:enlist n; while[not n = 1; seq,: n:$[n mod 2; 1 + 3 * n; `long$n % 2]]; seq}").await?;
    ///
    ///     // Send a query synchronously
    ///     let mut result = socket.send_sync_message(&"collatz[12]").await?;
    ///     println!("collatz[12]: {}", result);
    ///
    ///     // Send a functional query.
    ///     let mut message = K::new_compound_list(vec![
    ///         K::new_symbol(String::from("collatz")),
    ///         K::new_long(100),
    ///     ]);
    ///     result = socket.send_sync_message(&message).await?;
    ///     println!("collatz[100]: {}", result);
    ///
    ///     // Send a functional asynchronous query.
    ///     message = K::new_compound_list(vec![
    ///         K::new_string(String::from("show"), qattribute::NONE),
    ///         K::new_symbol(String::from("goodbye")),
    ///     ]);
    ///     socket.send_async_message(&message).await?;
    ///
    ///     socket.shutdown().await?;
    ///
    ///     Ok(())
    /// }
    /// ```
    pub async fn connect(
        method: ConnectionMethod,
        host: &str,
        port: u16,
        credential: &str,
    ) -> Result<Self> {
        match method {
            ConnectionMethod::TCP => {
                let stream = connect_tcp(host, port, credential).await?;
                let is_local = matches!(host, "localhost" | "127.0.0.1");
                Ok(QStream::new(
                    Box::new(stream),
                    ConnectionMethod::TCP,
                    false,
                    is_local,
                ))
            }
            ConnectionMethod::TLS => {
                let stream = connect_tls(host, port, credential).await?;
                Ok(QStream::new(
                    Box::new(stream),
                    ConnectionMethod::TLS,
                    false,
                    false,
                ))
            }
            #[cfg(unix)]
            ConnectionMethod::UDS => {
                let stream = connect_uds(port, credential).await?;
                Ok(QStream::new(
                    Box::new(stream),
                    ConnectionMethod::UDS,
                    false,
                    true,
                ))
            }
            #[cfg(not(unix))]
            ConnectionMethod::UDS => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix Domain Socket is not supported on this platform",
            )
            .into()),
        }
    }

    /// Accept connection and does handshake.
    /// # Parameters
    /// - `method`: Connection method. One of followings:
    ///   - TCP
    ///   - TLS
    ///   - UDS
    /// - host: Hostname or IP address of this listener. Empty `str` for Unix domain socket.
    /// - port: Listening port.
    /// # Example
    /// ```no_run
    /// use kdbplus::ipc::*;
    ///  
    /// #[tokio::main]
    /// async fn main() -> Result<()> {
    ///     // Start listenening over UDS at the port 7000 with authentication enabled.
    ///     while let Ok(mut socket) = QStream::accept(ConnectionMethod::UDS, "", 7000).await {
    ///         tokio::task::spawn(async move {
    ///             loop {
    ///                 match socket.receive_message().await {
    ///                     Ok((_, message)) => {
    ///                         println!("request: {}", message);
    ///                     }
    ///                     _ => {
    ///                         socket.shutdown().await.unwrap();
    ///                         break;
    ///                     }
    ///                 }
    ///             }
    ///         });
    ///     }
    ///
    ///     Ok(())
    /// }
    /// ```
    /// q processes can connect and send messages to this acceptor.
    /// ```q
    /// q)// Process1
    /// q)h:hopen `:unix://7000:reluctant:slowday
    /// q)neg[h] (`monalizza; 3.8)
    /// q)neg[h] (`pizza; 125)
    /// ```
    /// ```q
    /// q)// Process2
    /// q)h:hopen `:unix://7000:mattew:oracle
    /// q)neg[h] (`teddy; "bear")
    /// ```
    /// # Note
    /// - TLS acceptor sets `.kdbplus.close_tls_connection_` on q clien via an asynchronous message. This function is necessary to close
    ///   the socket from the server side without crashing server side application.
    /// - TLS acceptor and UDS acceptor use specific environmental variables to work. See the [Environmental Variable](../ipc/index.html#environmentl-variables) section for details.
    pub async fn accept(method: ConnectionMethod, host: &str, port: u16) -> Result<Self> {
        match method {
            ConnectionMethod::TCP => {
                // Bind to the endpoint.
                let listener = TcpListener::bind(&format!("{}:{}", host, port)).await?;
                // Listen to the endpoint.
                let (mut socket, ip_address) = listener.accept().await?;
                // Read untill null bytes and send back capacity.
                while (read_client_input(&mut socket).await).is_err() {
                    // Continue to listen in case of error.
                    socket = listener.accept().await?.0;
                }
                // Check if the connection is local
                Ok(QStream::new(
                    Box::new(socket),
                    ConnectionMethod::TCP,
                    true,
                    ip_address.ip() == IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                ))
            }
            ConnectionMethod::TLS => {
                // Bind to the endpoint.
                let listener = TcpListener::bind(&format!("{}:{}", host, port)).await?;
                // Check if key exists and decode an identity with a given password.
                let identity = build_identity_from_cert().await?;
                // Build TLS acceptor.
                let tls_acceptor = TlsAcceptor::from(TlsAcceptorInner::new(identity).unwrap());
                // Listen to the endpoint.
                let (mut socket, _) = listener.accept().await?;
                // TLS processing.
                let mut tls_socket = tls_acceptor
                    .accept(socket)
                    .await
                    .expect("failed to accept TLS connection");
                // Read untill null bytes and send back a capacity.
                while (read_client_input(&mut tls_socket).await).is_err() {
                    // Continue to listen in case of error.
                    socket = listener.accept().await?.0;
                    tls_socket = tls_acceptor
                        .accept(socket)
                        .await
                        .expect("failed to accept TLS connection");
                }
                // TLS is always a remote connection
                let mut qstream =
                    QStream::new(Box::new(tls_socket), ConnectionMethod::TCP, true, false);
                // In order to close the connection from the server side, it needs to tell a client to close the connection.
                // The `kdbplus_close_tls_connection_` will be called from the server at shutdown.
                qstream
                    .send_async_message(&".kdbplus.close_tls_connection_:{[] hclose .z.w;}")
                    .await?;
                Ok(qstream)
            }
            #[cfg(unix)]
            ConnectionMethod::UDS => {
                // uild a sockt file path.
                let uds_path = create_sockfile_path(port)?;
                let abstract_sockfile_ = format!("\x00{}", uds_path);
                let abstract_sockfile = Path::new(&abstract_sockfile_);
                // Bind to the file
                let listener = UnixListener::bind(abstract_sockfile).unwrap();
                // Listen to the endpoint
                let (mut socket, _) = listener.accept().await?;
                // Read untill null bytes and send back capacity.
                while (read_client_input(&mut socket).await).is_err() {
                    // Continue to listen in case of error.
                    socket = listener.accept().await?.0;
                }
                // UDS is always a local connection
                Ok(QStream::new(Box::new(socket), method, true, true))
            }
            #[cfg(not(unix))]
            ConnectionMethod::UDS => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix Domain Socket is not supported on this platform",
            )
            .into()),
        }
    }

    /// Shutdown the socket for a q process.
    /// # Example
    /// See the example of [`connect`](#method.connect).
    pub async fn shutdown(mut self) -> Result<()> {
        self.stream.shutdown(self.listener).await
    }

    /// Send a message with a specified message type without waiting for a response even for a synchronous message.
    ///  If you need to receive a response you need to use [`receive_message`](#method.receive_message).
    /// # Note
    /// The usage of this function for a synchronous message is to handle an asynchronous message or a synchronous message
    ///   sent by a remote function during its execution.
    /// # Parameters
    /// - `message`: q command to execute on the remote q process.
    ///   - `&str`: q command in a string form.
    ///   - `K`: Query in a functional form.
    /// - `message_type`: Asynchronous or synchronous.
    /// # Example
    /// See the example of [`connect`](#method.connect).
    pub async fn send_message(&mut self, message: &dyn Query, message_type: u8) -> Result<()> {
        self.stream
            .send_message(message, message_type, self.local)
            .await
    }

    /// Send a message asynchronously.
    /// # Parameters
    /// - `message`: q command to execute on the remote q process.
    ///   - `&str`: q command in a string form.
    ///   - `K`: Query in a functional form.
    /// # Example
    /// See the example of [`connect`](#method.connect).
    pub async fn send_async_message(&mut self, message: &dyn Query) -> Result<()> {
        self.stream.send_async_message(message, self.local).await
    }

    /// Send a message synchronously.
    /// # Note
    /// Remote function must NOT send back a message of asynchronous or synchronous type durning execution of the function.
    /// # Parameters
    /// - `message`: q command to execute on the remote q process.
    ///   - `&str`: q command in a string form.
    ///   - `K`: Query in a functional form.
    /// # Example
    /// See the example of [`connect`](#method.connect).
    pub async fn send_sync_message(&mut self, message: &dyn Query) -> Result<K> {
        self.stream.send_sync_message(message, self.local).await
    }

    /// Receive a message from a remote q process. The received message is parsed as `K` and message type is
    ///  stored in the first returned value.
    /// # Example
    /// See the example of [`accept`](#method.accept).
    pub async fn receive_message(&mut self) -> Result<(u8, K)> {
        self.stream.receive_message().await
    }

    /// Return underlying connection type. One of `TCP`, `TLS` or `UDS`.
    /// # Example
    /// See the example of [`connect`](#method.connect).
    pub fn get_connection_type(&self) -> &str {
        match self.method {
            ConnectionMethod::TCP => "TCP",
            ConnectionMethod::TLS => "TLS",
            ConnectionMethod::UDS => "UDS",
        }
    }

    /// Enforce compression if the size of a message exceeds 2000 regardless of locality of the connection.
    ///  This flag is not revertible intentionally.
    pub fn enforce_compression(&mut self) {
        self.local = false;
    }
}

//%% QStreamInner %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

#[async_trait]
impl QStreamInner for TcpStream {
    async fn shutdown(&mut self, _: bool) -> Result<()> {
        AsyncWriteExt::shutdown(self).await?;
        Ok(())
    }

    async fn send_message(
        &mut self,
        message: &dyn Query,
        message_type: u8,
        is_local: bool,
    ) -> Result<()> {
        // Serialize a message
        let byte_message = message.serialize(message_type, is_local).await;
        // Send the message
        self.write_all(&byte_message).await?;
        Ok(())
    }

    async fn send_async_message(&mut self, message: &dyn Query, is_local: bool) -> Result<()> {
        // Serialize a message
        let byte_message = message.serialize(qmsg_type::asynchronous, is_local).await;
        // Send the message
        self.write_all(&byte_message).await?;
        Ok(())
    }

    async fn send_sync_message(&mut self, message: &dyn Query, is_local: bool) -> Result<K> {
        // Serialize a message
        let byte_message = message.serialize(qmsg_type::synchronous, is_local).await;
        // Send the message
        self.write_all(&byte_message).await?;
        // Receive a response. If message type is not response it returns an error.
        match receive_message(self).await {
            Ok((qmsg_type::response, response)) => Ok(response),
            Err(error) => Err(error),
            Ok((_, message)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected a response: {}", message),
            )
            .into()),
        }
    }

    async fn receive_message(&mut self) -> Result<(u8, K)> {
        receive_message(self).await
    }
}

#[async_trait]
impl QStreamInner for TlsStream<TcpStream> {
    async fn shutdown(&mut self, is_listener: bool) -> Result<()> {
        if is_listener {
            // Closing the handle from the server side by `self.get_mut().shutdown()` crashes due to 'assertion failed: !self.context.is_null()'.
            // No reason to compress.
            self.send_async_message(&".kdbplus.close_tls_connection_[]", false)
                .await
        } else {
            self.get_mut().shutdown()?;
            Ok(())
        }
    }

    async fn send_message(
        &mut self,
        message: &dyn Query,
        message_type: u8,
        is_local: bool,
    ) -> Result<()> {
        // Serialize a message
        let byte_message = message.serialize(message_type, is_local).await;
        // Send the message
        self.write_all(&byte_message).await?;
        Ok(())
    }

    async fn send_async_message(&mut self, message: &dyn Query, is_local: bool) -> Result<()> {
        // Serialize a message
        let byte_message = message.serialize(qmsg_type::asynchronous, is_local).await;
        // Send the message
        self.write_all(&byte_message).await?;
        Ok(())
    }

    async fn send_sync_message(&mut self, message: &dyn Query, is_local: bool) -> Result<K> {
        // Serialize a message
        let byte_message = message.serialize(qmsg_type::synchronous, is_local).await;
        // Send the message
        self.write_all(&byte_message).await?;
        // Receive a response. If message type is not response it returns an error.
        match receive_message(self).await {
            Ok((qmsg_type::response, response)) => Ok(response),
            Err(error) => Err(error),
            Ok((_, message)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected a response: {}", message),
            )
            .into()),
        }
    }

    async fn receive_message(&mut self) -> Result<(u8, K)> {
        receive_message(self).await
    }
}

#[cfg(unix)]
#[async_trait]
impl QStreamInner for UnixStream {
    /// Close a handle to a q process which is connected with Unix Domain Socket.
    ///  Socket file is removed.
    async fn shutdown(&mut self, _: bool) -> Result<()> {
        AsyncWriteExt::shutdown(self).await?;
        Ok(())
    }

    async fn send_message(
        &mut self,
        message: &dyn Query,
        message_type: u8,
        is_local: bool,
    ) -> Result<()> {
        // Serialize a message
        let byte_message = message.serialize(message_type, is_local).await;
        // Send the message
        self.write_all(&byte_message).await?;
        Ok(())
    }

    async fn send_async_message(&mut self, message: &dyn Query, is_local: bool) -> Result<()> {
        // Serialize a message
        let byte_message = message.serialize(qmsg_type::asynchronous, is_local).await;
        // Send the message
        self.write_all(&byte_message).await?;
        Ok(())
    }

    async fn send_sync_message(&mut self, message: &dyn Query, is_local: bool) -> Result<K> {
        // Serialize a message
        let byte_message = message.serialize(qmsg_type::synchronous, is_local).await;
        // Send the message
        self.write_all(&byte_message).await?;
        // Receive a response. If message type is not response it returns an error.
        match receive_message(self).await {
            Ok((qmsg_type::response, response)) => Ok(response),
            Err(error) => Err(error),
            Ok((_, message)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected a response: {}", message),
            )
            .into()),
        }
    }

    async fn receive_message(&mut self) -> Result<(u8, K)> {
        receive_message(self).await
    }
}

//%% MessageHeader %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

impl MessageHeader {
    /// Constructor.
    fn new(encoding: u8, message_type: u8, compressed: u8, length: u32) -> Self {
        MessageHeader {
            encoding,
            message_type,
            compressed,
            _unused: 0,
            length,
        }
    }

    /// Constructor from bytes.
    fn from_bytes(bytes: [u8; 8]) -> Self {
        let encoding = bytes[0];

        let length = match encoding {
            0 => u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
            _ => u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        };

        // Build header
        MessageHeader::new(encoding, bytes[1], bytes[2], length)
    }

    /// Length of bytes for a header.
    fn size() -> usize {
        8
    }
}

//++++++++++++++++++++++++++++++++++++++++++++++++++//
// >> Private Functions
//++++++++++++++++++++++++++++++++++++++++++++++++++//

//%% QStream Connector %%//vvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Inner function of `connect_tcp` and `connect_tls` to establish a TCP connection with the sepcified
///  endpoint. The hostname is resolved to an IP address with a system DNS resolver or parsed directly
///  as an IP address.
///
/// Tries to connect to multiple resolved IP addresses until the first successful connection. Error is
///  returned if none of them are valid.
/// # Parameters
/// - `host`: Hostname or IP address of the target q/kdb+ process.
/// - `port`: Port of the target q process
async fn connect_tcp_impl(host: &str, port: u16) -> Result<TcpStream> {
    // DNS system resolver (should not fail)
    let resolver =
        TokioResolver::builder_tokio()
            .expect("failed to create resolver builder")
            .build()
            .expect("failed to build resolver");

    // Check if we were given an IP address
    let ips;
    if let Ok(ip) = host.parse::<IpAddr>() {
        ips = vec![ip.to_string()]
    } else {
        // Resolve the given hostname
        ips = resolver
            .lookup_ip(format!("{}.", host).as_str())
            .await
            .unwrap()
            .iter()
            .filter(|ip| ip.is_ipv4())
            .map(|ip| ip.to_string())
            .collect()
    };

    for answer in ips {
        let host_port = format!("{}:{}", answer, port);
        // Return if this IP address is valid
        match TcpStream::connect(&host_port).await {
            Ok(socket) => {
                //println!("connected: {}", host_port);
                return Ok(socket);
            }
            Err(_) => {
                eprintln!("connection refused: {}. try next.", host_port);
            }
        }
    }
    // All addresses failed.
    Err(io::Error::new(io::ErrorKind::ConnectionRefused, "failed to connect").into())
}

/// Send a credential and receive a common capacity.
async fn handshake<S>(socket: &mut S, credential_: &str, method_bytes: &str) -> Result<()>
where
    S: Unpin + AsyncWriteExt + AsyncReadExt,
{
    // Send credential
    let credential = credential_.to_string() + method_bytes;
    socket.write_all(credential.as_bytes()).await?;

    // Placeholder of common capablility
    let mut cap = [0u8; 1];
    if (socket.read_exact(&mut cap).await).is_err() {
        // Connection is closed in case of authentication failure
        Err(io::Error::new(io::ErrorKind::ConnectionAborted, "authentication failure").into())
    } else {
        Ok(())
    }
}

/// Connect to q process running on a specified `host` and `port` via TCP with a credential `username:password`.
/// # Parameters
/// - `host`: Hostname or IP address of the target q process.
/// - `port`: Port of the target q process.
/// - `credential`: Credential in the form of `username:password` to connect to the target q process.
async fn connect_tcp(host: &str, port: u16, credential: &str) -> Result<TcpStream> {
    // Connect via TCP
    let mut socket = connect_tcp_impl(host, port).await?;
    // Handshake
    handshake(&mut socket, credential, "\x03\x00").await?;
    Ok(socket)
}

/// TLS version of `connect_tcp`.
/// # Parameters
/// - `host`: Hostname or IP address of the target q process.
/// - `port`: Port of the target q process.
/// - `credential`: Credential in the form of `username:password` to connect to the target q process.
async fn connect_tls(host: &str, port: u16, credential: &str) -> Result<TlsStream<TcpStream>> {
    // Connect via TCP
    let socket_ = connect_tcp_impl(host, port).await?;
    // Use TLS
    let connector = TlsConnector::from(TlsConnectorInner::new().unwrap());
    let mut socket = connector
        .connect(host, socket_)
        .await
        .expect("failed to create TLS session");
    // Handshake
    handshake(&mut socket, credential, "\x03\x00").await?;
    Ok(socket)
}

/// Build a path of a socket file.
fn create_sockfile_path(port: u16) -> Result<String> {
    // Create file path
    let udspath = match env::var("QUDSPATH") {
        Ok(dir) => format!("{}/kx.{}", dir, port),
        Err(_) => format!("/tmp/kx.{}", port),
    };

    Ok(udspath)
}

/// Connect to q process running on the specified `port` via Unix domain socket with a credential `username:password`.
/// # Parameters
/// - `port`: Port of the target q process.
/// - `credential`: Credential in the form of `username:password` to connect to the target q process.
#[cfg(unix)]
async fn connect_uds(port: u16, credential: &str) -> Result<UnixStream> {
    // Create a file path.
    let uds_path = create_sockfile_path(port)?;
    let abstract_sockfile_ = format!("\x00{}", uds_path);
    let abstract_sockfile = Path::new(&abstract_sockfile_);
    // Connect to kdb+.
    let mut socket = UnixStream::connect(&abstract_sockfile).await?;
    // Handshake
    handshake(&mut socket, credential, "\x06\x00").await?;

    Ok(socket)
}

//%% QStream Acceptor %%//vvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Read username, password, capacity and null byte from q client at the connection and does authentication.
///  Close the handle if the authentication fails.
async fn read_client_input<S>(socket: &mut S) -> Result<()>
where
    S: Unpin + AsyncWriteExt + AsyncReadExt,
{
    // Buffer to read inputs.
    let mut client_input = [0u8; 32];
    // credential will be built from small fractions of bytes.
    let mut passed_credential = String::new();
    loop {
        // Read a client credential input.
        match socket.read(&mut client_input).await {
            Ok(0) => {
                // No bytes were read
            }
            Ok(_) => {
                // Locate a byte denoting a capacity
                if let Some(index) = client_input
                    .iter()
                    .position(|byte| *byte == 0x03 || *byte == 0x06)
                {
                    let capacity = client_input[index];
                    passed_credential
                        .push_str(str::from_utf8(&client_input[0..index]).expect("invalid bytes"));
                    let credential = passed_credential.as_str().split(':').collect::<Vec<&str>>();
                    #[allow(clippy::borrow_interior_mutable_const)]
                    if let Some(encoded) = ACCOUNTS.get(&credential[0].to_string()) {
                        // User exists
                        let mut hasher = Sha1::new();
                        hasher.update(credential[1].as_bytes());
                        let encoded_password = hasher.digest().to_string();
                        if encoded == &encoded_password {
                            // Client passed correct credential
                            socket.write_all(&[capacity; 1]).await?;
                            return Ok(());
                        } else {
                            // Authentication failure.
                            // Close connection.
                            socket.shutdown().await?;
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "authentication failed",
                            )
                            .into());
                        }
                    } else {
                        // Authentication failure.
                        // Close connection.
                        socket.shutdown().await?;
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "authentication failed",
                        )
                        .into());
                    }
                } else {
                    // Append a fraction of credential
                    passed_credential
                        .push_str(str::from_utf8(&client_input).expect("invalid bytes"));
                }
            }
            Err(error) => {
                return Err(error.into());
            }
        }
    }
}

/// Check if server key exists and return teh contents.
async fn build_identity_from_cert() -> Result<Identity> {
    // Check if server key exists.
    if let Ok(path) = env::var("KDBPLUS_TLS_KEY_FILE") {
        if let Ok(password) = env::var("KDBPLUS_TLS_KEY_FILE_SECRET") {
            let cert_file = tokio::fs::File::open(Path::new(&path)).await.unwrap();
            let mut reader = BufReader::new(cert_file);
            let mut der: Vec<u8> = Vec::new();
            // Read the key file.
            reader.read_to_end(&mut der).await?;
            // Create identity.
            if let Ok(identity) = Identity::from_pkcs12(&der, &password) {
                Ok(identity)
            } else {
                Err(io::Error::new(io::ErrorKind::InvalidData, "authentication failed").into())
            }
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "KDBPLUS_TLS_KEY_FILE_SECRET is not set",
            )
            .into())
        }
    } else {
        Err(io::Error::new(io::ErrorKind::NotFound, "KDBPLUS_TLS_KEY_FILE is not set").into())
    }
}

//%% QStream Query %%//vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/

/// Receive a message from q process with decompression if necessary. The received message is parsed as `K` and message type is
///  stored in the first returned value.
/// # Parameters
/// - `socket`: Socket to communicate with a q process. Either of `TcpStream`, `TlsStream<TcpStream>` or `UnixStream`.
async fn receive_message<S>(socket: &mut S) -> Result<(u8, K)>
where
    S: Unpin + AsyncReadExt,
{
    // Read header
    let mut header_buffer = [0u8; 8];
    if let Err(err) = socket.read_exact(&mut header_buffer).await {
        // The expected message is header or EOF (close due to q process failure resulting from a bad query)
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("Connection dropped: {}", err),
        )
        .into());
    }

    // Parse message header
    let header = MessageHeader::from_bytes(header_buffer);

    // Read body
    let body_length = header.length as usize - MessageHeader::size();
    let mut body: Vec<u8> = vec![0; body_length];
    if let Err(err) = socket.read_exact(&mut body).await {
        // Fails if q process fails before reading the body
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("Failed to read body of message: {}", err),
        )
        .into());
    }

    // Decompress if necessary
    if header.compressed == 0x01 {
        body = decompress(body, header.encoding).await;
    }

    Ok((
        header.message_type,
        K::q_ipc_decode(&body, header.encoding).await,
    ))
}

/// Compress body. The combination of serializing the data and compressing will result in
/// the same output as shown in the q language by using the -18! function e.g.
/// serializing 2000 bools set to true, then compressing, will have the same output as `-18!2000#1b`.
/// # Parameter
/// - `raw`: Serialized message.
/// - `encode`: `0` if Big Endian; `1` if Little Endian.
async fn compress(raw: Vec<u8>) -> (bool, Vec<u8>) {
    let mut i = 0_u8;
    let mut f = 0_u8;
    let mut h0 = 0_usize;
    let mut h = 0_usize;
    let mut g: bool;
    let mut compressed: Vec<u8> = vec![0; (raw.len()) / 2];

    // Start index of compressed body
    // 12 bytes are reserved for the header + size of raw bytes
    let mut c = 12;
    let mut d = c;
    let e = compressed.len();
    let mut p = 0_usize;
    let mut q: usize;
    let mut r: usize;
    let mut s0 = 0_usize;

    // Body starts from index 8
    let mut s = 8_usize;
    let t = raw.len();
    let mut a = [0_i32; 256];

    // Copy encode, message type, compressed and reserved
    compressed[0..4].copy_from_slice(&raw[0..4]);
    // Set compressed flag
    compressed[2] = 1;

    // Write size of raw bytes including a header
    let raw_size = match ENCODING {
        0 => (t as u32).to_be_bytes(),
        _ => (t as u32).to_le_bytes(),
    };
    compressed[8..12].copy_from_slice(&raw_size);

    while s < t {
        if i == 0 {
            if d > e - 17 {
                // Early return when compressing to less than half failed
                return (false, raw);
            }
            i = 1;
            compressed[c] = f;
            c = d;
            d += 1;
            f = 0;
        }
        g = s > t - 3;
        if !g {
            h = (raw[s] ^ raw[s + 1]) as usize;
            p = a[h] as usize;
            g = (0 == p) || (0 != (raw[s] ^ raw[p]));
        }
        if 0 < s0 {
            a[h0] = s0 as i32;
            s0 = 0;
        }
        if g {
            h0 = h;
            s0 = s;
            compressed[d] = raw[s];
            d += 1;
            s += 1;
        } else {
            a[h] = s as i32;
            f |= i;
            p += 2;
            s += 2;
            r = s;
            q = if s + 255 > t { t } else { s + 255 };
            while (s < q) && (raw[p] == raw[s]) {
                s += 1;
                if s < q {
                    p += 1;
                }
            }
            compressed[d] = h as u8;
            d += 1;
            compressed[d] = (s - r) as u8;
            d += 1;
        }
        i = i.wrapping_mul(2);
    }
    compressed[c] = f;
    // Final compressed data size
    let compressed_size = match ENCODING {
        0 => (d as u32).to_be_bytes(),
        _ => (d as u32).to_le_bytes(),
    };
    compressed[4..8].copy_from_slice(&compressed_size);
    let _ = compressed.split_off(d);
    (true, compressed)
}

/// Decompress body. The combination of decompressing and deserializing the data
///  will result in the same output as shown in the q language by using the `-19!` function.
/// # Parameter
/// - `compressed`: Compressed serialized message.
/// - `encoding`:
///   - `0`: Big Endian
///   - `1`: Little Endian.
async fn decompress(compressed: Vec<u8>, encoding: u8) -> Vec<u8> {
    let mut n = 0;
    let mut r: usize;
    let mut f = 0_usize;

    // Header has already been removed.
    // Start index of decompressed bytes is 0
    let mut s = 0_usize;
    let mut p = s;
    let mut i = 0_usize;

    // Subtract 8 bytes from decoded bytes size as 8 bytes have already been taken as header
    let size = match encoding {
        0 => {
            i32::from_be_bytes(
                compressed[0..4]
                    .try_into()
                    .expect("slice does not have length 4"),
            ) - 8
        }
        _ => {
            i32::from_le_bytes(
                compressed[0..4]
                    .try_into()
                    .expect("slice does not have length 4"),
            ) - 8
        }
    };
    let mut decompressed: Vec<u8> = vec![0; size as usize];

    // Start index of compressed body.
    // 8 bytes have already been removed as header
    let mut d = 4;
    let mut aa = [0_i32; 256];
    while s < decompressed.len() {
        if i == 0 {
            f = compressed[d] as usize;
            d += 1;
            i = 1;
        }
        if (f & i) != 0 {
            r = aa[compressed[d] as usize] as usize;
            d += 1;
            decompressed[s] = decompressed[r];
            s += 1;
            r += 1;
            decompressed[s] = decompressed[r];
            s += 1;
            r += 1;
            n = compressed[d] as usize;
            d += 1;
            for m in 0..n {
                decompressed[s + m] = decompressed[r + m];
            }
        } else {
            decompressed[s] = compressed[d];
            s += 1;
            d += 1;
        }
        while p < s - 1 {
            aa[(decompressed[p] ^ decompressed[p + 1]) as usize] = p as i32;
            p += 1;
        }
        if (f & i) != 0 {
            s += n;
            p = s;
        }
        i *= 2;
        if i == 256 {
            i = 0;
        }
    }
    decompressed
}
