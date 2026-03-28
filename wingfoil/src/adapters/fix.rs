//! FIX (Financial Information eXchange) protocol adapter.
//!
//! Enable with the `fix` feature flag.
//!
//! Two poll modes are available:
//! - [`FixPollMode::AlwaysSpin`] — non-blocking socket polled by the graph spin loop (~1–5 µs)
//! - [`FixPollMode::Threaded`] — background thread + channel (~10–100 µs)
//!
//! Both [`fix_connect`] (initiator) and [`fix_accept`] (acceptor) return the same
//! `(Stream<Burst<FixMessage>>, Stream<FixSessionStatus>)` pair.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::channel::{ChannelSender, channel_pair};
use crate::{
    Burst, ChannelReceiverStream, GraphState, IntoNode, IntoStream, MapFilterStream, MutableNode,
    NanoTime, Node, RunMode, Stream, StreamPeekRef, UpStreams,
};
use tinyvec::TinyVec;

// ── FIX constants ────────────────────────────────────────────────────────────

const TAG_BODY_LENGTH: u32 = 9;
const TAG_MSG_TYPE: u32 = 35;
const TAG_SENDER_COMP_ID: u32 = 49;
const TAG_TARGET_COMP_ID: u32 = 56;
const TAG_MSG_SEQ_NUM: u32 = 34;
const TAG_SENDING_TIME: u32 = 52;
const TAG_CHECKSUM: u32 = 10;
const TAG_BEGIN_STRING: u32 = 8;
const TAG_HEARTBT_INT: u32 = 108;
const TAG_TEST_REQ_ID: u32 = 112;
const TAG_ENCRYPT_METHOD: u32 = 98;

const MSG_HEARTBEAT: &str = "0";
const MSG_TEST_REQUEST: &str = "1";
const MSG_LOGON: &str = "A";
const MSG_LOGOUT: &str = "5";

const SOH: u8 = 0x01;
const BEGIN_STRING: &str = "FIX.4.4";
const HEARTBEAT_INTERVAL: u32 = 30;
const READ_BUF_SIZE: usize = 4096;

/// Header/trailer tags excluded from the application-level `fields` list.
const HEADER_TAGS: &[u32] = &[
    TAG_BEGIN_STRING,
    TAG_BODY_LENGTH,
    TAG_MSG_TYPE,
    TAG_SENDER_COMP_ID,
    TAG_TARGET_COMP_ID,
    TAG_MSG_SEQ_NUM,
    TAG_SENDING_TIME,
    TAG_CHECKSUM,
];

// ── Public types ─────────────────────────────────────────────────────────────

/// A decoded FIX tag-value message.
#[derive(Debug, Clone, Default)]
pub struct FixMessage {
    /// MsgType (tag 35).
    pub msg_type: String,
    /// Inbound sequence number (tag 34).
    pub seq_num: u64,
    /// SendingTime as [`NanoTime`] (tag 52; currently set to zero — future work).
    pub sending_time: NanoTime,
    /// Application-level tag/value pairs (standard header and trailer excluded).
    pub fields: Vec<(u32, String)>,
}

impl FixMessage {
    /// Returns the value for `tag`, if present in the application fields.
    pub fn field(&self, tag: u32) -> Option<&str> {
        self.fields
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, v)| v.as_str())
    }
}

/// FIX session lifecycle state.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum FixSessionStatus {
    #[default]
    Disconnected,
    LoggingIn,
    LoggedIn,
    LoggedOut,
    Error(String),
}

/// Internal event multiplexing data messages and session status changes.
#[derive(Debug, Clone)]
pub enum FixEvent {
    Data(FixMessage),
    Status(FixSessionStatus),
}

impl Default for FixEvent {
    fn default() -> Self {
        FixEvent::Data(FixMessage::default())
    }
}

/// Controls how incoming FIX data is polled from the network.
pub enum FixPollMode {
    /// Graph spin loop drives polling — no dedicated thread, lowest latency.
    AlwaysSpin,
    /// Background thread + channel — shares CPU with other work.
    Threaded,
}

// ── FIX tag-value codec ───────────────────────────────────────────────────────

fn append_field(buf: &mut Vec<u8>, tag: u32, value: &str) {
    buf.extend_from_slice(tag.to_string().as_bytes());
    buf.push(b'=');
    buf.extend_from_slice(value.as_bytes());
    buf.push(SOH);
}

fn encode_message(
    msg_type: &str,
    sender: &str,
    target: &str,
    seq: u64,
    extra: &[(u32, String)],
) -> Vec<u8> {
    let mut body = Vec::<u8>::new();
    append_field(&mut body, TAG_MSG_TYPE, msg_type);
    append_field(&mut body, TAG_SENDER_COMP_ID, sender);
    append_field(&mut body, TAG_TARGET_COMP_ID, target);
    append_field(&mut body, TAG_MSG_SEQ_NUM, &seq.to_string());
    let ts = chrono::Utc::now().format("%Y%m%d-%H:%M:%S").to_string();
    append_field(&mut body, TAG_SENDING_TIME, &ts);
    for (tag, val) in extra {
        append_field(&mut body, *tag, val);
    }

    let mut out = Vec::<u8>::new();
    append_field(&mut out, TAG_BEGIN_STRING, BEGIN_STRING);
    append_field(&mut out, TAG_BODY_LENGTH, &body.len().to_string());
    out.extend_from_slice(&body);
    let checksum: u8 = out.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    append_field(&mut out, TAG_CHECKSUM, &format!("{checksum:03}"));
    out
}

fn decode_fields(data: &[u8]) -> Vec<(u32, String)> {
    let mut fields = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let Some(eq_off) = data[pos..].iter().position(|&b| b == b'=') else {
            break;
        };
        let eq = pos + eq_off;
        let tag: u32 = match std::str::from_utf8(&data[pos..eq])
            .ok()
            .and_then(|s| s.parse().ok())
        {
            Some(t) => t,
            None => {
                pos = eq + 1;
                continue;
            }
        };
        let Some(soh_off) = data[eq + 1..].iter().position(|&b| b == SOH) else {
            break;
        };
        let soh = eq + 1 + soh_off;
        let value = std::str::from_utf8(&data[eq + 1..soh])
            .unwrap_or("")
            .to_string();
        fields.push((tag, value));
        pos = soh + 1;
    }
    fields
}

fn build_message(all: Vec<(u32, String)>) -> Option<FixMessage> {
    let msg_type = all.iter().find(|(t, _)| *t == TAG_MSG_TYPE)?.1.clone();
    let seq_num = all
        .iter()
        .find(|(t, _)| *t == TAG_MSG_SEQ_NUM)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let fields = all
        .into_iter()
        .filter(|(t, _)| !HEADER_TAGS.contains(t))
        .collect();
    Some(FixMessage {
        msg_type,
        seq_num,
        sending_time: NanoTime::ZERO,
        fields,
    })
}

/// Find the first complete FIX message in `buf` (delimited by `\x0110=xxx\x01`).
/// Returns `(owned_msg_bytes, bytes_consumed)`.
fn find_message(buf: &[u8]) -> Option<(Vec<u8>, usize)> {
    let pattern = b"\x0110=";
    let pos = buf.windows(pattern.len()).position(|w| w == pattern)?;
    let val_start = pos + pattern.len();
    let soh_off = buf[val_start..].iter().position(|&b| b == SOH)?;
    let end = val_start + soh_off + 1;
    Some((buf[..end].to_vec(), end))
}

// ── FixSession ────────────────────────────────────────────────────────────────

struct FixSession {
    sender_comp_id: String,
    target_comp_id: String,
    out_seq: u64,
}

impl FixSession {
    fn new(sender: &str, target: &str) -> Self {
        Self {
            sender_comp_id: sender.to_string(),
            target_comp_id: target.to_string(),
            out_seq: 0,
        }
    }

    fn send(
        &mut self,
        sock: &mut TcpStream,
        msg_type: &str,
        extra: &[(u32, String)],
    ) -> anyhow::Result<()> {
        self.out_seq += 1;
        let bytes = encode_message(
            msg_type,
            &self.sender_comp_id,
            &self.target_comp_id,
            self.out_seq,
            extra,
        );
        sock.write_all(&bytes)?;
        Ok(())
    }

    fn send_logon(&mut self, sock: &mut TcpStream) -> anyhow::Result<()> {
        self.send(
            sock,
            MSG_LOGON,
            &[
                (TAG_ENCRYPT_METHOD, "0".to_string()),
                (TAG_HEARTBT_INT, HEARTBEAT_INTERVAL.to_string()),
            ],
        )
    }

    fn send_logout(&mut self, sock: &mut TcpStream) -> anyhow::Result<()> {
        self.send(sock, MSG_LOGOUT, &[])
    }

    fn send_heartbeat(
        &mut self,
        sock: &mut TcpStream,
        test_req_id: Option<String>,
    ) -> anyhow::Result<()> {
        let extra = test_req_id
            .map(|id| vec![(TAG_TEST_REQ_ID, id)])
            .unwrap_or_default();
        self.send(sock, MSG_HEARTBEAT, &extra)
    }
}

/// Handle a session-level message for the **initiator** role.
/// Appends any generated status events to `events`.
/// Returns `true` if the message should be forwarded to the application layer.
fn handle_initiator(
    session: &mut FixSession,
    msg: &FixMessage,
    sock: &mut TcpStream,
    events: &mut Burst<FixEvent>,
) -> anyhow::Result<bool> {
    match msg.msg_type.as_str() {
        MSG_LOGON => {
            events.push(FixEvent::Status(FixSessionStatus::LoggedIn));
            Ok(false)
        }
        MSG_HEARTBEAT => Ok(false),
        MSG_TEST_REQUEST => {
            let id = msg.field(TAG_TEST_REQ_ID).map(str::to_string);
            session.send_heartbeat(sock, id)?;
            Ok(false)
        }
        MSG_LOGOUT => {
            events.push(FixEvent::Status(FixSessionStatus::LoggedOut));
            Ok(false)
        }
        _ => Ok(true),
    }
}

/// Handle a session-level message for the **acceptor** role.
fn handle_acceptor(
    session: &mut FixSession,
    msg: &FixMessage,
    sock: &mut TcpStream,
    events: &mut Burst<FixEvent>,
) -> anyhow::Result<bool> {
    match msg.msg_type.as_str() {
        MSG_LOGON => {
            session.send_logon(sock)?;
            events.push(FixEvent::Status(FixSessionStatus::LoggedIn));
            Ok(false)
        }
        MSG_HEARTBEAT => Ok(false),
        MSG_TEST_REQUEST => {
            let id = msg.field(TAG_TEST_REQ_ID).map(str::to_string);
            session.send_heartbeat(sock, id)?;
            Ok(false)
        }
        MSG_LOGOUT => {
            events.push(FixEvent::Status(FixSessionStatus::LoggedOut));
            Ok(false)
        }
        _ => Ok(true),
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

fn connect_with_retry(host: &str, port: u16) -> anyhow::Result<TcpStream> {
    for attempt in 0..20u32 {
        match TcpStream::connect((host, port)) {
            Ok(s) => return Ok(s),
            Err(_) if attempt < 19 => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("failed to connect to {host}:{port} after 20 attempts")
}

fn split_events(
    events: Rc<dyn Stream<Burst<FixEvent>>>,
) -> (
    Rc<dyn Stream<Burst<FixMessage>>>,
    Rc<dyn Stream<FixSessionStatus>>,
) {
    let data = MapFilterStream::new(
        events.clone(),
        Box::new(|burst: Burst<FixEvent>| {
            let msgs: Burst<FixMessage> = burst
                .into_iter()
                .filter_map(|e| {
                    if let FixEvent::Data(m) = e {
                        Some(m)
                    } else {
                        None
                    }
                })
                .collect();
            let ticked = !msgs.is_empty();
            (msgs, ticked)
        }),
    )
    .into_stream();

    let status = MapFilterStream::new(
        events,
        Box::new(|burst: Burst<FixEvent>| {
            match burst
                .into_iter()
                .filter_map(|e| {
                    if let FixEvent::Status(s) = e {
                        Some(s)
                    } else {
                        None
                    }
                })
                .next_back()
            {
                Some(s) => (s, true),
                None => (FixSessionStatus::default(), false),
            }
        }),
    )
    .into_stream();

    (data, status)
}

// ── AlwaysSpin initiator ──────────────────────────────────────────────────────

struct FixSpinSource {
    host: String,
    port: u16,
    session: FixSession,
    socket: Option<TcpStream>,
    parse_buf: Vec<u8>,
    value: Burst<FixEvent>,
}

impl FixSpinSource {
    fn new(host: &str, port: u16, sender: &str, target: &str) -> Self {
        Self {
            host: host.to_string(),
            port,
            session: FixSession::new(sender, target),
            socket: None,
            parse_buf: Vec::new(),
            value: TinyVec::new(),
        }
    }

    fn drain_messages(&mut self, is_acceptor: bool) -> anyhow::Result<bool> {
        let before = self.value.len();
        loop {
            let Some((msg_bytes, consumed)) = find_message(&self.parse_buf) else {
                break;
            };
            self.parse_buf.drain(..consumed);
            let Some(msg) = build_message(decode_fields(&msg_bytes)) else {
                continue;
            };
            let mut sock = match self.socket.take() {
                Some(s) => s,
                None => continue, // disconnected, skip session responses
            };
            let pass = if is_acceptor {
                handle_acceptor(&mut self.session, &msg, &mut sock, &mut self.value)?
            } else {
                handle_initiator(&mut self.session, &msg, &mut sock, &mut self.value)?
            };
            self.socket = Some(sock);
            if pass {
                self.value.push(FixEvent::Data(msg));
            }
        }
        // Ticked if any events (status or data) were pushed
        Ok(self.value.len() > before)
    }
}

impl MutableNode for FixSpinSource {
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if state.run_mode() != RunMode::RealTime {
            anyhow::bail!("FIX nodes only support real-time mode");
        }
        state.always_callback();
        let mut sock = connect_with_retry(&self.host, self.port)?;
        self.session.send_logon(&mut sock)?;
        sock.set_nonblocking(true)?;
        self.socket = Some(sock);
        self.value
            .push(FixEvent::Status(FixSessionStatus::LoggingIn));
        Ok(())
    }

    fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
        self.value.clear();

        // Read phase — borrow socket and parse_buf separately
        let eof = {
            let mut eof = false;
            if let Some(sock) = self.socket.as_mut() {
                let mut tmp = [0u8; READ_BUF_SIZE];
                loop {
                    match sock.read(&mut tmp) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(n) => self.parse_buf.extend_from_slice(&tmp[..n]),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            eof
        };

        if eof {
            self.socket = None;
            self.value
                .push(FixEvent::Status(FixSessionStatus::Disconnected));
            return Ok(true);
        }

        self.drain_messages(false)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::default()
    }

    fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        if let Some(mut sock) = self.socket.take() {
            let _ = self.session.send_logout(&mut sock);
        }
        Ok(())
    }
}

impl StreamPeekRef<Burst<FixEvent>> for FixSpinSource {
    fn peek_ref(&self) -> &Burst<FixEvent> {
        &self.value
    }
}

// ── AlwaysSpin acceptor ───────────────────────────────────────────────────────

struct FixAcceptorSpin {
    port: u16,
    session: FixSession,
    listener: Option<TcpListener>,
    socket: Option<TcpStream>,
    parse_buf: Vec<u8>,
    value: Burst<FixEvent>,
}

impl FixAcceptorSpin {
    fn new(port: u16, sender: &str, target: &str) -> Self {
        Self {
            port,
            session: FixSession::new(sender, target),
            listener: None,
            socket: None,
            parse_buf: Vec::new(),
            value: TinyVec::new(),
        }
    }
}

impl MutableNode for FixAcceptorSpin {
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if state.run_mode() != RunMode::RealTime {
            anyhow::bail!("FIX nodes only support real-time mode");
        }
        state.always_callback();
        let listener = TcpListener::bind(("0.0.0.0", self.port))?;
        listener.set_nonblocking(true)?;
        self.listener = Some(listener);
        Ok(())
    }

    fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
        self.value.clear();
        let mut ticked = false;

        // Accept phase
        if self.socket.is_none()
            && let Some(listener) = self.listener.as_ref()
        {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(true)?;
                    self.socket = Some(stream);
                    self.value
                        .push(FixEvent::Status(FixSessionStatus::LoggingIn));
                    ticked = true;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }

        // Read phase
        let eof = {
            let mut eof = false;
            if let Some(sock) = self.socket.as_mut() {
                let mut tmp = [0u8; READ_BUF_SIZE];
                loop {
                    match sock.read(&mut tmp) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(n) => self.parse_buf.extend_from_slice(&tmp[..n]),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            eof
        };

        if eof {
            self.socket = None;
            self.value
                .push(FixEvent::Status(FixSessionStatus::Disconnected));
            ticked = true;
        }

        // Process complete messages; track whether any events are emitted
        let before = self.value.len();
        loop {
            let Some((msg_bytes, consumed)) = find_message(&self.parse_buf) else {
                break;
            };
            self.parse_buf.drain(..consumed);
            let Some(msg) = build_message(decode_fields(&msg_bytes)) else {
                continue;
            };
            let mut sock = match self.socket.take() {
                Some(s) => s,
                None => continue,
            };
            let pass = handle_acceptor(&mut self.session, &msg, &mut sock, &mut self.value)?;
            self.socket = Some(sock);
            if pass {
                self.value.push(FixEvent::Data(msg));
            }
        }
        let msg_ticked = self.value.len() > before;

        Ok(ticked || msg_ticked)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::default()
    }

    fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        if let Some(mut sock) = self.socket.take() {
            let _ = self.session.send_logout(&mut sock);
        }
        Ok(())
    }
}

impl StreamPeekRef<Burst<FixEvent>> for FixAcceptorSpin {
    fn peek_ref(&self) -> &Burst<FixEvent> {
        &self.value
    }
}

// ── Threaded source (initiator or acceptor) ───────────────────────────────────

/// Run the FIX session on `sock`, forwarding events to `chan`.
/// Sends a clone of the socket back via `socket_back` so the owner can shut it down.
fn run_fix_thread(
    mut sock: TcpStream,
    mut session: FixSession,
    is_acceptor: bool,
    chan: ChannelSender<FixEvent>,
    socket_back: mpsc::SyncSender<TcpStream>,
) {
    use crate::channel::Message;

    let send = |msg| {
        chan.send_message(Message::RealtimeValue(msg))
            .map_err(|e| anyhow::anyhow!(e))
    };

    let finish = |status: FixSessionStatus| {
        let _ = chan.send_message(Message::RealtimeValue(FixEvent::Status(status)));
        let _ = chan.send_message(Message::EndOfStream);
    };

    // Send a clone back so stop() can call shutdown() on it.
    match sock.try_clone() {
        Ok(clone) => {
            let _ = socket_back.send(clone);
        }
        Err(e) => {
            finish(FixSessionStatus::Error(e.to_string()));
            return;
        }
    }

    if let Err(e) = send(FixEvent::Status(FixSessionStatus::LoggingIn)) {
        let _ = e; // channel closed — nothing to do
        return;
    }

    if !is_acceptor && let Err(e) = session.send_logon(&mut sock) {
        finish(FixSessionStatus::Error(e.to_string()));
        return;
    }

    let mut parse_buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; READ_BUF_SIZE];

    loop {
        match sock.read(&mut tmp) {
            Ok(0) => {
                finish(FixSessionStatus::Disconnected);
                return;
            }
            Err(e)
                if e.kind() == io::ErrorKind::ConnectionReset
                    || e.kind() == io::ErrorKind::BrokenPipe =>
            {
                finish(FixSessionStatus::Disconnected);
                return;
            }
            Err(_) => {
                // Shutdown or other error — clean exit.
                finish(FixSessionStatus::Disconnected);
                return;
            }
            Ok(n) => parse_buf.extend_from_slice(&tmp[..n]),
        }

        loop {
            let Some((msg_bytes, consumed)) = find_message(&parse_buf) else {
                break;
            };
            parse_buf.drain(..consumed);
            let Some(msg) = build_message(decode_fields(&msg_bytes)) else {
                continue;
            };

            let mut events: Burst<FixEvent> = TinyVec::new();
            let pass = if is_acceptor {
                match handle_acceptor(&mut session, &msg, &mut sock, &mut events) {
                    Ok(p) => p,
                    Err(_) => {
                        finish(FixSessionStatus::Disconnected);
                        return;
                    }
                }
            } else {
                match handle_initiator(&mut session, &msg, &mut sock, &mut events) {
                    Ok(p) => p,
                    Err(_) => {
                        finish(FixSessionStatus::Disconnected);
                        return;
                    }
                }
            };

            for event in events {
                if send(event).is_err() {
                    return;
                }
            }
            if pass && send(FixEvent::Data(msg)).is_err() {
                return;
            }
        }
    }
}

struct FixThreadedSource {
    // Config
    host: String,
    port: u16,
    sender_comp_id: String,
    target_comp_id: String,
    is_acceptor: bool,
    // Graph integration
    inner: ChannelReceiverStream<FixEvent>,
    chan_sender: Option<ChannelSender<FixEvent>>,
    // Thread management
    thread: Option<JoinHandle<()>>,
    // Receives a TcpStream clone from the thread so stop() can call shutdown()
    socket_back_rx: Option<mpsc::Receiver<TcpStream>>,
    socket_handle: Option<TcpStream>,
}

impl FixThreadedSource {
    fn new_initiator(host: &str, port: u16, sender: &str, target: &str) -> Self {
        let (chan_sender, receiver) = channel_pair(None);
        let inner = ChannelReceiverStream::new(receiver, None, None);
        Self {
            host: host.to_string(),
            port,
            sender_comp_id: sender.to_string(),
            target_comp_id: target.to_string(),
            is_acceptor: false,
            inner,
            chan_sender: Some(chan_sender),
            thread: None,
            socket_back_rx: None,
            socket_handle: None,
        }
    }

    fn new_acceptor(port: u16, sender: &str, target: &str) -> Self {
        let (chan_sender, receiver) = channel_pair(None);
        let inner = ChannelReceiverStream::new(receiver, None, None);
        Self {
            host: "0.0.0.0".to_string(),
            port,
            sender_comp_id: sender.to_string(),
            target_comp_id: target.to_string(),
            is_acceptor: true,
            inner,
            chan_sender: Some(chan_sender),
            thread: None,
            socket_back_rx: None,
            socket_handle: None,
        }
    }

    /// Try to obtain the socket handle from the background thread.
    /// Blocks up to 2 s waiting for the thread to establish its connection.
    fn fetch_socket_handle(&mut self) {
        if self.socket_handle.is_some() {
            return;
        }
        if let Some(rx) = self.socket_back_rx.take()
            && let Ok(s) = rx.recv_timeout(Duration::from_secs(2))
        {
            self.socket_handle = Some(s);
        }
    }
}

impl MutableNode for FixThreadedSource {
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if state.run_mode() != RunMode::RealTime {
            anyhow::bail!("FIX nodes only support real-time mode");
        }
        self.inner.start(state)
    }

    fn setup(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        let mut chan_sender = self
            .chan_sender
            .take()
            .ok_or_else(|| anyhow::anyhow!("FixThreadedSource: already set up"))?;

        if state.run_mode() == RunMode::RealTime {
            chan_sender.set_notifier(state.ready_notifier());
        }

        let (socket_back_tx, socket_back_rx) = mpsc::sync_channel(1);
        self.socket_back_rx = Some(socket_back_rx);

        let host = self.host.clone();
        let port = self.port;
        let sender_id = self.sender_comp_id.clone();
        let target_id = self.target_comp_id.clone();
        let is_acceptor = self.is_acceptor;

        let handle = std::thread::spawn(move || {
            let sock_result = if is_acceptor {
                TcpListener::bind(("0.0.0.0", port)).and_then(|l| l.accept().map(|(s, _)| s))
            } else {
                connect_with_retry(&host, port).map_err(|e| io::Error::other(e.to_string()))
            };

            let sock = match sock_result {
                Ok(s) => s,
                Err(e) => {
                    let _ = chan_sender.send_message(crate::channel::Message::RealtimeValue(
                        FixEvent::Status(FixSessionStatus::Error(e.to_string())),
                    ));
                    let _ = chan_sender.send_message(crate::channel::Message::EndOfStream);
                    return;
                }
            };

            let session = FixSession::new(&sender_id, &target_id);
            run_fix_thread(sock, session, is_acceptor, chan_sender, socket_back_tx);
        });

        self.thread = Some(handle);
        self.inner.setup(state)
    }

    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.inner.cycle(state)
    }

    fn upstreams(&self) -> UpStreams {
        self.inner.upstreams()
    }

    fn stop(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        // Obtain the socket handle sent back by the thread, then shut it down
        // so the blocking read() returns and the thread can exit cleanly.
        self.fetch_socket_handle();
        if let Some(s) = self.socket_handle.take() {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        self.inner.stop(state)
    }

    fn teardown(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.inner.teardown(state)
    }
}

impl StreamPeekRef<Burst<FixEvent>> for FixThreadedSource {
    fn peek_ref(&self) -> &Burst<FixEvent> {
        self.inner.peek_ref()
    }
}

// ── FixSenderNode (sink) ──────────────────────────────────────────────────────

struct FixSenderNode {
    src: Rc<dyn Stream<FixMessage>>,
    host: String,
    port: u16,
    session: FixSession,
    socket: Option<TcpStream>,
}

impl MutableNode for FixSenderNode {
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if state.run_mode() != RunMode::RealTime {
            anyhow::bail!("FIX nodes only support real-time mode");
        }
        let mut sock = connect_with_retry(&self.host, self.port)?;
        self.session.send_logon(&mut sock)?;
        self.socket = Some(sock);
        Ok(())
    }

    fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
        let msg = self.src.peek_value();
        let mut sock = self
            .socket
            .take()
            .ok_or_else(|| anyhow::anyhow!("FIX sender: not connected"))?;
        self.session.send(&mut sock, &msg.msg_type, &msg.fields)?;
        self.socket = Some(sock);
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.src.clone().as_node()], vec![])
    }

    fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        if let Some(mut sock) = self.socket.take() {
            let _ = self.session.send_logout(&mut sock);
        }
        Ok(())
    }
}

// ── Public factory functions ──────────────────────────────────────────────────

/// Connect to a FIX acceptor as an initiator.
///
/// Returns `(data_stream, status_stream)`.
pub fn fix_connect(
    host: &str,
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    mode: FixPollMode,
) -> (
    Rc<dyn Stream<Burst<FixMessage>>>,
    Rc<dyn Stream<FixSessionStatus>>,
) {
    match mode {
        FixPollMode::AlwaysSpin => {
            let events: Rc<dyn Stream<Burst<FixEvent>>> =
                FixSpinSource::new(host, port, sender_comp_id, target_comp_id).into_stream();
            split_events(events)
        }
        FixPollMode::Threaded => {
            let events: Rc<dyn Stream<Burst<FixEvent>>> =
                FixThreadedSource::new_initiator(host, port, sender_comp_id, target_comp_id)
                    .into_stream();
            split_events(events)
        }
    }
}

/// Bind a FIX acceptor on `port`, accepting one initiator connection.
///
/// Returns `(data_stream, status_stream)`.
pub fn fix_accept(
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    mode: FixPollMode,
) -> (
    Rc<dyn Stream<Burst<FixMessage>>>,
    Rc<dyn Stream<FixSessionStatus>>,
) {
    match mode {
        FixPollMode::AlwaysSpin => {
            let events: Rc<dyn Stream<Burst<FixEvent>>> =
                FixAcceptorSpin::new(port, sender_comp_id, target_comp_id).into_stream();
            split_events(events)
        }
        FixPollMode::Threaded => {
            let events: Rc<dyn Stream<Burst<FixEvent>>> =
                FixThreadedSource::new_acceptor(port, sender_comp_id, target_comp_id).into_stream();
            split_events(events)
        }
    }
}

// ── FixOperators trait ────────────────────────────────────────────────────────

/// Fluent extension for sending a [`FixMessage`] stream to a FIX acceptor.
pub trait FixOperators {
    fn fix_send(
        &self,
        host: &str,
        port: u16,
        sender_comp_id: &str,
        target_comp_id: &str,
    ) -> Rc<dyn Node>;
}

impl FixOperators for Rc<dyn Stream<FixMessage>> {
    fn fix_send(
        &self,
        host: &str,
        port: u16,
        sender_comp_id: &str,
        target_comp_id: &str,
    ) -> Rc<dyn Node> {
        FixSenderNode {
            src: self.clone(),
            host: host.to_string(),
            port,
            session: FixSession::new(sender_comp_id, target_comp_id),
            socket: None,
        }
        .into_node()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Graph, NanoTime, NodeOperators, RunFor, RunMode, StreamOperators};
    use std::time::Duration;

    #[test]
    fn encode_decode_roundtrip() {
        let bytes = encode_message(
            "D",
            "SENDER",
            "TARGET",
            1,
            &[
                (55, "AAPL".to_string()),
                (54, "1".to_string()),
                (38, "100".to_string()),
                (44, "150.00".to_string()),
            ],
        );
        let (msg_bytes, _) = find_message(&bytes).expect("message not found");
        let msg = build_message(decode_fields(&msg_bytes)).expect("parse failed");
        assert_eq!(msg.msg_type, "D");
        assert_eq!(msg.seq_num, 1);
        assert_eq!(msg.field(55), Some("AAPL"));
        assert_eq!(msg.field(54), Some("1"));
        assert_eq!(msg.field(38), Some("100"));
        assert_eq!(msg.field(44), Some("150.00"));
    }

    #[test]
    fn fix_historical_mode_fails() {
        let (data, _) = fix_connect("127.0.0.1", 29876, "S", "T", FixPollMode::AlwaysSpin);
        let err = data
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .expect_err("expected historical mode to fail");
        assert!(
            format!("{err:?}").contains("real-time"),
            "expected real-time error, got: {err:?}"
        );
    }

    #[test]
    fn fix_same_process_spin() {
        let _ = env_logger::try_init();
        let port = 19876u16;
        let run_for = RunFor::Duration(Duration::from_millis(500));

        let (acc_data, acc_status) =
            fix_accept(port, "ACCEPTOR", "INITIATOR", FixPollMode::AlwaysSpin);
        let (init_data, init_status) = fix_connect(
            "127.0.0.1",
            port,
            "INITIATOR",
            "ACCEPTOR",
            FixPollMode::AlwaysSpin,
        );

        let acc_node = acc_status.collect().finally(|items, _| {
            let vs: Vec<FixSessionStatus> = items.into_iter().map(|i| i.value).collect();
            assert!(
                vs.contains(&FixSessionStatus::LoggedIn),
                "acceptor: expected LoggedIn, got: {vs:?}"
            );
            Ok(())
        });
        let init_node = init_status.collect().finally(|items, _| {
            let vs: Vec<FixSessionStatus> = items.into_iter().map(|i| i.value).collect();
            assert!(
                vs.contains(&FixSessionStatus::LoggedIn),
                "initiator: expected LoggedIn, got: {vs:?}"
            );
            Ok(())
        });

        Graph::new(
            vec![acc_data.as_node(), acc_node, init_data.as_node(), init_node],
            RunMode::RealTime,
            run_for,
        )
        .run()
        .unwrap();
    }

    #[test]
    fn fix_same_process_threaded() {
        let _ = env_logger::try_init();
        let port = 19877u16;
        let run_for = RunFor::Duration(Duration::from_millis(500));

        let (acc_data, acc_status) =
            fix_accept(port, "ACCEPTOR", "INITIATOR", FixPollMode::Threaded);
        let (init_data, init_status) = fix_connect(
            "127.0.0.1",
            port,
            "INITIATOR",
            "ACCEPTOR",
            FixPollMode::Threaded,
        );

        let acc_node = acc_status.collect().finally(|items, _| {
            let vs: Vec<FixSessionStatus> = items.into_iter().map(|i| i.value).collect();
            assert!(
                vs.contains(&FixSessionStatus::LoggedIn),
                "acceptor: expected LoggedIn, got: {vs:?}"
            );
            Ok(())
        });
        let init_node = init_status.collect().finally(|items, _| {
            let vs: Vec<FixSessionStatus> = items.into_iter().map(|i| i.value).collect();
            assert!(
                vs.contains(&FixSessionStatus::LoggedIn),
                "initiator: expected LoggedIn, got: {vs:?}"
            );
            Ok(())
        });

        Graph::new(
            vec![acc_data.as_node(), acc_node, init_data.as_node(), init_node],
            RunMode::RealTime,
            run_for,
        )
        .run()
        .unwrap();
    }
}
