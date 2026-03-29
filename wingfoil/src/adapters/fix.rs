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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

/// Drain all complete FIX messages from `parse_buf`, dispatching session-level messages
/// and pushing application/status events into `events`.
/// Returns `true` if any events were pushed.
fn drain_parse_buf(
    parse_buf: &mut Vec<u8>,
    socket: &mut Option<TcpStream>,
    session: &mut FixSession,
    events: &mut Burst<FixEvent>,
    is_acceptor: bool,
) -> anyhow::Result<bool> {
    let before = events.len();
    loop {
        let Some((msg_bytes, consumed)) = find_message(parse_buf) else {
            break;
        };
        parse_buf.drain(..consumed);
        let Some(msg) = build_message(decode_fields(&msg_bytes)) else {
            continue;
        };
        let mut sock = match socket.take() {
            Some(s) => s,
            None => continue,
        };
        let pass = if is_acceptor {
            handle_acceptor(session, &msg, &mut sock, events)?
        } else {
            handle_initiator(session, &msg, &mut sock, events)?
        };
        *socket = Some(sock);
        if pass {
            events.push(FixEvent::Data(msg));
        }
    }
    Ok(events.len() > before)
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

    fn drain_messages(&mut self) -> anyhow::Result<bool> {
        drain_parse_buf(
            &mut self.parse_buf,
            &mut self.socket,
            &mut self.session,
            &mut self.value,
            false,
        )
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

        self.drain_messages()
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
        let msg_ticked = drain_parse_buf(
            &mut self.parse_buf,
            &mut self.socket,
            &mut self.session,
            &mut self.value,
            true,
        )?;

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

/// Run a single FIX session on `sock`, forwarding events to `chan`.
///
/// Returns `true` if the session ended due to a normal network disconnect
/// (the caller may reconnect), or `false` if the channel is closed (graph
/// has stopped and the thread should exit).
fn run_fix_session(
    mut sock: TcpStream,
    session: &mut FixSession,
    is_acceptor: bool,
    chan: &ChannelSender<FixEvent>,
) -> bool {
    use crate::channel::Message;

    let send = |msg| {
        chan.send_message(Message::RealtimeValue(msg))
            .map_err(|e| anyhow::anyhow!(e))
    };

    if send(FixEvent::Status(FixSessionStatus::LoggingIn)).is_err() {
        return false;
    }

    if !is_acceptor && let Err(e) = session.send_logon(&mut sock) {
        let _ = send(FixEvent::Status(FixSessionStatus::Error(e.to_string())));
        return true;
    }

    let mut parse_buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; READ_BUF_SIZE];
    let mut sock_opt = Some(sock);

    loop {
        let sock = match sock_opt.as_mut() {
            Some(s) => s,
            None => return true, // disconnected during session dispatch
        };

        match sock.read(&mut tmp) {
            Ok(0) => return true,
            Err(e)
                if e.kind() == io::ErrorKind::ConnectionReset
                    || e.kind() == io::ErrorKind::BrokenPipe =>
            {
                return true;
            }
            Err(_) => return true, // shutdown or other error — clean exit
            Ok(n) => parse_buf.extend_from_slice(&tmp[..n]),
        }

        let mut events: Burst<FixEvent> = TinyVec::new();
        match drain_parse_buf(
            &mut parse_buf,
            &mut sock_opt,
            session,
            &mut events,
            is_acceptor,
        ) {
            Ok(_) => {}
            Err(_) => return true,
        }

        for event in events {
            if send(event).is_err() {
                return false;
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
    // Current live socket, updated by the thread on each (re)connect.
    // stop() shuts it down so the blocking read() unblocks and the thread exits.
    socket_arc: Option<Arc<Mutex<Option<TcpStream>>>>,
    // Set by stop() to prevent the thread from re-accepting after socket shutdown.
    stop_flag: Arc<AtomicBool>,
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
            socket_arc: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
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
            socket_arc: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
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

        let socket_arc: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
        let socket_arc_thread = socket_arc.clone();
        self.socket_arc = Some(socket_arc);

        let stop_flag = self.stop_flag.clone();
        let host = self.host.clone();
        let port = self.port;
        let sender_id = self.sender_comp_id.clone();
        let target_id = self.target_comp_id.clone();
        let is_acceptor = self.is_acceptor;

        let handle = std::thread::spawn(move || {
            use crate::channel::Message;

            let send_status = |status: FixSessionStatus| {
                chan_sender
                    .send_message(Message::RealtimeValue(FixEvent::Status(status)))
                    .is_ok()
            };

            loop {
                // Check the stop flag before each (re)connect attempt.
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }

                // Connect (initiator) or bind+accept (acceptor).
                let sock_result = if is_acceptor {
                    TcpListener::bind(("0.0.0.0", port)).and_then(|l| l.accept().map(|(s, _)| s))
                } else {
                    connect_with_retry(&host, port).map_err(|e| io::Error::other(e.to_string()))
                };

                let sock = match sock_result {
                    Ok(s) => s,
                    Err(e) => {
                        if !send_status(FixSessionStatus::Error(e.to_string())) {
                            break;
                        }
                        // For initiators, give up; for acceptors, retry.
                        if !is_acceptor {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                };

                // Store a clone so stop() can shut down the live socket.
                match sock.try_clone() {
                    Ok(clone) => *socket_arc_thread.lock().unwrap() = Some(clone),
                    Err(e) => {
                        if !send_status(FixSessionStatus::Error(e.to_string())) {
                            break;
                        }
                        if !is_acceptor {
                            break;
                        }
                        continue;
                    }
                }

                let mut session = FixSession::new(&sender_id, &target_id);
                let still_open = run_fix_session(sock, &mut session, is_acceptor, &chan_sender);

                // Clear the stale socket handle.
                *socket_arc_thread.lock().unwrap() = None;

                if !still_open {
                    break; // channel closed — graph has stopped
                }

                if !send_status(FixSessionStatus::Disconnected) {
                    break;
                }

                // Initiators don't auto-reconnect; acceptors loop to re-accept.
                if !is_acceptor {
                    break;
                }
            }

            let _ = chan_sender.send_message(Message::EndOfStream);
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
        // Signal the thread not to re-accept after the current session ends.
        self.stop_flag.store(true, Ordering::Relaxed);
        // Shut down the live socket so the thread's blocking read() returns.
        if let Some(arc) = self.socket_arc.take()
            && let Some(s) = arc.lock().unwrap().take()
        {
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
    parse_buf: Vec<u8>,
}

impl MutableNode for FixSenderNode {
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if state.run_mode() != RunMode::RealTime {
            anyhow::bail!("FIX nodes only support real-time mode");
        }
        let mut sock = connect_with_retry(&self.host, self.port)?;
        self.session.send_logon(&mut sock)?;
        sock.set_nonblocking(true)?;
        self.socket = Some(sock);
        Ok(())
    }

    fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
        let msg = self.src.peek_value();
        let mut sock_opt = self.socket.take();

        // Drain any incoming bytes (heartbeats, test requests, etc.)
        if let Some(sock) = sock_opt.as_mut() {
            let mut tmp = [0u8; READ_BUF_SIZE];
            loop {
                match sock.read(&mut tmp) {
                    Ok(0) => {
                        sock_opt = None;
                        break;
                    }
                    Ok(n) => self.parse_buf.extend_from_slice(&tmp[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        sock_opt = None;
                        break;
                    }
                }
            }
        }

        // Handle session-level messages (respond to test requests, etc.)
        let mut events: Burst<FixEvent> = TinyVec::new();
        drain_parse_buf(
            &mut self.parse_buf,
            &mut sock_opt,
            &mut self.session,
            &mut events,
            false,
        )?;

        let mut sock = sock_opt.ok_or_else(|| anyhow::anyhow!("FIX sender: connection lost"))?;
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
            parse_buf: Vec::new(),
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

    /// Allocate an ephemeral port by binding to :0 and immediately dropping the listener.
    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

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
        let port = free_port();
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
        let port = free_port();
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
