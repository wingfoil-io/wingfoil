//! iceoryx2 subscriber (read) implementation

use core::time::Duration;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crate::channel::{ChannelReceiver, ChannelSender, Message, channel_pair};
use crate::types::{Element, IntoStream};
use crate::{Burst, Stream};

use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::port::update_connections::UpdateConnections;
use iceoryx2::prelude::*;

use super::{
    Iceoryx2Error, Iceoryx2Mode, Iceoryx2NodeHandle, Iceoryx2Result, Iceoryx2ServiceContract,
    Iceoryx2ServiceVariant, Iceoryx2SubOpts, iceoryx2_default_config,
};

fn service_open_err_with_context(
    service_name: &str,
    variant: Iceoryx2ServiceVariant,
    contract: Iceoryx2ServiceContract,
    err: impl std::fmt::Display,
) -> Iceoryx2Error {
    let error = err.to_string();
    let looks_like_mismatch = {
        let e = error.to_lowercase();
        e.contains("incompatible") || e.contains("mismatch") || e.contains("config")
    };

    if looks_like_mismatch {
        Iceoryx2Error::ServiceConfigMismatch {
            error,
            service_name: service_name.to_string(),
            variant,
            history_size: contract.history_size,
            subscriber_max_buffer_size: contract.subscriber_max_buffer_size,
        }
    } else {
        Iceoryx2Error::ServiceOpenFailed {
            error,
            service_name: service_name.to_string(),
            variant,
            history_size: contract.history_size,
            subscriber_max_buffer_size: contract.subscriber_max_buffer_size,
        }
    }
}

// ---------------------------------------------------------------------------
// Macros to eliminate Ipc/Local duplication
// ---------------------------------------------------------------------------

/// Create a node, open/create a pub-sub service, and build a subscriber.
macro_rules! create_subscriber {
    ($svc:ty, $service_name:expr, $variant:expr, $history_size:expr, $payload:ty) => {{
        let node = NodeBuilder::new()
            .config(iceoryx2_default_config())
            .create::<$svc>()
            .map_err(|e| Iceoryx2Error::NodeCreationFailed(e.to_string()))?;
        let contract = Iceoryx2ServiceContract::new($history_size);
        let service = node
            .service_builder(&$service_name.as_str().try_into().map_err(
                |e: iceoryx2::service::service_name::ServiceNameError| {
                    Iceoryx2Error::Other(anyhow::anyhow!(e.to_string()))
                },
            )?)
            .publish_subscribe::<$payload>()
            .history_size($history_size)
            .subscriber_max_buffer_size(contract.subscriber_max_buffer_size)
            .open_or_create()
            .map_err(|e| service_open_err_with_context($service_name, $variant, contract, e))?;
        let subscriber = service
            .subscriber_builder()
            .buffer_size($history_size.max(1))
            .create()
            .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
        subscriber
            .update_connections()
            .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
        (node, subscriber)
    }};
}

/// Run the subscriber poll loop (both Signaled and Threaded paths).
macro_rules! run_poll_loop {
    ($svc:ty, $subscriber:expr, $channel_sender:expr, $opts:expr, $running:expr,
     $service_name:expr, $node:expr, $extract:expr) => {{
        if $opts.mode == Iceoryx2Mode::Signaled {
            let signal_name = format!("{}.signal", $service_name);
            let event_service = {
                let mut last_err: Option<String> = None;
                let mut service = None;
                for _ in 0..25 {
                    match $node
                        .service_builder(&signal_name.as_str().try_into().map_err(
                            |e: iceoryx2::service::service_name::ServiceNameError| {
                                Iceoryx2Error::Other(anyhow::anyhow!(e.to_string()))
                            },
                        )?)
                        .event()
                        .open_or_create()
                    {
                        Ok(s) => {
                            service = Some(s);
                            break;
                        }
                        Err(e) => {
                            last_err = Some(e.to_string());
                            thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }
                }
                service.ok_or_else(|| {
                    service_open_err_with_context(
                        &signal_name,
                        $opts.variant,
                        Iceoryx2ServiceContract::new(0),
                        last_err.unwrap_or_else(|| "event open_or_create failed".to_string()),
                    )
                })?
            };
            let listener = event_service
                .listener_builder()
                .create()
                .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
            let ws = WaitSetBuilder::new().create::<$svc>().map_err(
                |e: iceoryx2::waitset::WaitSetCreateError| {
                    Iceoryx2Error::Other(anyhow::anyhow!(e.to_string()))
                },
            )?;
            let _attachment = ws
                .attach_notification(&listener)
                .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;

            while $running.load(Ordering::SeqCst) {
                let _ = ws
                    .wait_and_process_once_with_timeout(
                        |_| CallbackProgression::Continue,
                        Duration::from_millis(100),
                    )
                    .map_err(|e: iceoryx2::waitset::WaitSetRunError| {
                        Iceoryx2Error::Other(anyhow::anyhow!(e.to_string()))
                    })?;

                let _ = $subscriber.update_connections();

                while let Ok(Some(sample)) = $subscriber.receive() {
                    let data = $extract(&sample);
                    let _ = $channel_sender.send_message(Message::RealtimeValue(data));
                    drop(sample);
                }

                while let Ok(Some(_)) = listener.try_wait_one() {}
            }
        } else {
            let mut loop_cycles = 0u64;
            while $running.load(Ordering::SeqCst) {
                loop_cycles += 1;
                if loop_cycles % 100 == 0 {
                    let _ = $subscriber.update_connections();
                }

                let mut received = false;
                while let Ok(Some(sample)) = $subscriber.receive() {
                    let data = $extract(&sample);
                    let _ = $channel_sender.send_message(Message::RealtimeValue(data));
                    drop(sample);
                    received = true;
                }

                if !received && $opts.mode == Iceoryx2Mode::Threaded {
                    thread::sleep(std::time::Duration::from_micros(10));
                }
            }
        }
        Ok(())
    }};
}

// ---------------------------------------------------------------------------
// Typed subscriber (Burst<T>)
// ---------------------------------------------------------------------------

/// Subscribe to an iceoryx2 service and produce a stream of samples.
///
/// # Type Parameters
/// - `T`: Must implement `ZeroCopySend`, `Clone`, `Copy`, `Debug`, `Default`, `'static`
///
/// # Arguments
/// - `service_name`: The iceoryx2 service name (e.g., "my/service")
///
/// # Returns
/// A stream that emits `Burst<T>` - batches of samples received since last cycle.
#[must_use]
pub fn iceoryx2_sub<T>(service_name: &str) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    iceoryx2_sub_opts(service_name, Iceoryx2SubOpts::default())
}

/// Subscribe to an iceoryx2 service using the selected service variant.
#[must_use]
pub fn iceoryx2_sub_with<T>(
    service_name: &str,
    variant: Iceoryx2ServiceVariant,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    iceoryx2_sub_opts(
        service_name,
        Iceoryx2SubOpts {
            variant,
            mode: Iceoryx2Mode::default(),
            ..Default::default()
        },
    )
}

/// Subscribe with explicit options.
#[must_use]
pub fn iceoryx2_sub_opts<T>(service_name: &str, opts: Iceoryx2SubOpts) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    Iceoryx2ReceiverStream::new(service_name.to_string(), opts).into_stream()
}

/// Subscribe to a byte-slice service.
#[must_use]
pub fn iceoryx2_sub_slice(service_name: &str) -> Rc<dyn Stream<Burst<Vec<u8>>>> {
    iceoryx2_sub_slice_opts(service_name, Iceoryx2SubOpts::default())
}

/// Subscribe to a byte-slice service with explicit options.
#[must_use]
pub fn iceoryx2_sub_slice_opts(
    service_name: &str,
    opts: Iceoryx2SubOpts,
) -> Rc<dyn Stream<Burst<Vec<u8>>>> {
    Iceoryx2SliceReceiverStream::new(service_name.to_string(), opts).into_stream()
}

enum Iceoryx2SubscriberPort<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    Ipc(Subscriber<ipc::Service, T, ()>),
    Local(Subscriber<local::Service, T, ()>),
}

struct Iceoryx2ReceiverStream<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    service_name: String,
    opts: Iceoryx2SubOpts,
    node: Option<Iceoryx2NodeHandle>,
    subscriber: Option<Iceoryx2SubscriberPort<T>>,
    receiver: Option<ChannelReceiver<T>>,
    running: Arc<AtomicBool>,
    thread_handle: Option<thread::JoinHandle<()>>,
    value: Burst<T>,
    cycles: u64,
}

impl<T> Iceoryx2ReceiverStream<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    fn new(service_name: String, opts: Iceoryx2SubOpts) -> Self {
        Self {
            service_name,
            opts,
            node: None,
            subscriber: None,
            receiver: None,
            running: Arc::new(AtomicBool::new(false)),
            thread_handle: None,
            value: Burst::default(),
            cycles: 0,
        }
    }
}

/// Dispatch `update_connections()` on either Ipc or Local subscriber port.
macro_rules! dispatch_update_connections {
    ($port:expr, $($variant:ident),+) => {
        match $port {
            $( Self::$variant(s) => { let _ = s.update_connections(); } )+
        }
    };
}

/// Dispatch `receive()` loop, pushing to burst.
macro_rules! dispatch_receive {
    ($port:expr, $burst:expr, $extract:expr, $($variant:ident),+) => {
        match $port {
            $( Self::$variant(subscriber) => {
                while let Ok(Some(sample)) = subscriber.receive() {
                    $burst.push($extract(&sample));
                    drop(sample);
                }
            } )+
        }
    };
}

impl Iceoryx2SubscriberPort<()> {
    // Just for the macro namespace; actual methods are on the generic version.
}

impl<T> Iceoryx2SubscriberPort<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    fn update_connections_periodic(&self) {
        dispatch_update_connections!(self, Ipc, Local);
    }

    fn receive_into(&self, burst: &mut Burst<T>) {
        dispatch_receive!(self, burst, |s: &T| *s, Ipc, Local);
    }
}

impl<T> crate::MutableNode for Iceoryx2ReceiverStream<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    fn cycle(&mut self, _state: &mut crate::GraphState) -> anyhow::Result<bool> {
        self.value.clear();
        self.cycles += 1;

        match self.opts.mode {
            Iceoryx2Mode::Spin => {
                let Some(subscriber) = &self.subscriber else {
                    return Ok(false);
                };

                if self.cycles.is_multiple_of(10) {
                    subscriber.update_connections_periodic();
                }

                subscriber.receive_into(&mut self.value);
            }
            Iceoryx2Mode::Threaded | Iceoryx2Mode::Signaled => {
                let Some(receiver) = &self.receiver else {
                    return Ok(false);
                };

                loop {
                    match receiver.try_recv() {
                        Some(Message::RealtimeValue(value)) => {
                            self.value.push(value);
                        }
                        Some(Message::EndOfStream) => {
                            break;
                        }
                        Some(Message::Error(err)) => {
                            return Err(anyhow::anyhow!(err));
                        }
                        None => break,
                        _ => {}
                    }
                }
            }
        }

        Ok(!self.value.is_empty())
    }

    fn start(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
        match self.opts.mode {
            Iceoryx2Mode::Spin => {
                state.always_callback();

                match self.opts.variant {
                    Iceoryx2ServiceVariant::Ipc => {
                        let (node, subscriber) = create_subscriber!(
                            ipc::Service,
                            &self.service_name,
                            self.opts.variant,
                            self.opts.history_size,
                            T
                        );
                        self.node = Some(Iceoryx2NodeHandle::Ipc(node));
                        self.subscriber = Some(Iceoryx2SubscriberPort::Ipc(subscriber));
                    }
                    Iceoryx2ServiceVariant::Local => {
                        let (node, subscriber) = create_subscriber!(
                            local::Service,
                            &self.service_name,
                            self.opts.variant,
                            self.opts.history_size,
                            T
                        );
                        self.node = Some(Iceoryx2NodeHandle::Local(node));
                        self.subscriber = Some(Iceoryx2SubscriberPort::Local(subscriber));
                    }
                }
            }
            Iceoryx2Mode::Threaded | Iceoryx2Mode::Signaled => {
                state.always_callback();

                let (sender, receiver) = channel_pair(None);
                self.receiver = Some(receiver);
                self.running.store(true, Ordering::SeqCst);

                let service_name = self.service_name.clone();
                let opts = self.opts.clone();
                let running = self.running.clone();

                self.thread_handle = Some(thread::spawn(move || {
                    if let Err(e) = run_subscriber_thread::<T>(service_name, sender, opts, running)
                    {
                        log::error!("iceoryx2 subscriber thread error: {:?}", e);
                    }
                }));
            }
        }

        Ok(())
    }

    fn stop(&mut self, _state: &mut crate::GraphState) -> anyhow::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
        self.subscriber = None;
        self.receiver = None;
        self.node = None;
        Ok(())
    }

    fn upstreams(&self) -> crate::UpStreams {
        crate::UpStreams::none()
    }
}

impl<T> crate::StreamPeekRef<Burst<T>> for Iceoryx2ReceiverStream<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    fn peek_ref(&self) -> &Burst<T> {
        &self.value
    }
}

// ---------------------------------------------------------------------------
// Slice subscriber (Burst<Vec<u8>>)
// ---------------------------------------------------------------------------

struct Iceoryx2SliceReceiverStream {
    service_name: String,
    opts: Iceoryx2SubOpts,
    node: Option<Iceoryx2NodeHandle>,
    subscriber: Option<Iceoryx2SliceSubscriberPort>,
    receiver: Option<ChannelReceiver<Vec<u8>>>,
    running: Arc<AtomicBool>,
    thread_handle: Option<thread::JoinHandle<()>>,
    value: Burst<Vec<u8>>,
    cycles: u64,
}

enum Iceoryx2SliceSubscriberPort {
    Ipc(Subscriber<ipc::Service, [u8], ()>),
    Local(Subscriber<local::Service, [u8], ()>),
}

impl Iceoryx2SliceSubscriberPort {
    fn update_connections_periodic(&self) {
        match self {
            Self::Ipc(s) => {
                let _ = s.update_connections();
            }
            Self::Local(s) => {
                let _ = s.update_connections();
            }
        }
    }

    fn receive_into(&self, burst: &mut Burst<Vec<u8>>) {
        match self {
            Self::Ipc(subscriber) => {
                while let Ok(Some(sample)) = subscriber.receive() {
                    burst.push(sample.to_vec());
                    drop(sample);
                }
            }
            Self::Local(subscriber) => {
                while let Ok(Some(sample)) = subscriber.receive() {
                    burst.push(sample.to_vec());
                    drop(sample);
                }
            }
        }
    }
}

impl Iceoryx2SliceReceiverStream {
    fn new(service_name: String, opts: Iceoryx2SubOpts) -> Self {
        Self {
            service_name,
            opts,
            node: None,
            subscriber: None,
            receiver: None,
            running: Arc::new(AtomicBool::new(false)),
            thread_handle: None,
            value: Burst::default(),
            cycles: 0,
        }
    }
}

impl crate::MutableNode for Iceoryx2SliceReceiverStream {
    fn cycle(&mut self, _state: &mut crate::GraphState) -> anyhow::Result<bool> {
        self.value.clear();
        self.cycles += 1;

        match self.opts.mode {
            Iceoryx2Mode::Spin => {
                let Some(subscriber) = &self.subscriber else {
                    return Ok(false);
                };

                if self.cycles.is_multiple_of(10) {
                    subscriber.update_connections_periodic();
                }

                subscriber.receive_into(&mut self.value);
            }
            Iceoryx2Mode::Threaded | Iceoryx2Mode::Signaled => {
                let Some(receiver) = &self.receiver else {
                    return Ok(false);
                };

                loop {
                    match receiver.try_recv() {
                        Some(Message::RealtimeValue(value)) => {
                            self.value.push(value);
                        }
                        Some(Message::EndOfStream) => {
                            break;
                        }
                        Some(Message::Error(err)) => {
                            return Err(anyhow::anyhow!(err));
                        }
                        None => break,
                        _ => {}
                    }
                }
            }
        }

        Ok(!self.value.is_empty())
    }

    fn start(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
        match self.opts.mode {
            Iceoryx2Mode::Spin => {
                state.always_callback();
                match self.opts.variant {
                    Iceoryx2ServiceVariant::Ipc => {
                        let (node, subscriber) = create_subscriber!(
                            ipc::Service,
                            &self.service_name,
                            self.opts.variant,
                            self.opts.history_size,
                            [u8]
                        );
                        self.node = Some(Iceoryx2NodeHandle::Ipc(node));
                        self.subscriber = Some(Iceoryx2SliceSubscriberPort::Ipc(subscriber));
                    }
                    Iceoryx2ServiceVariant::Local => {
                        let (node, subscriber) = create_subscriber!(
                            local::Service,
                            &self.service_name,
                            self.opts.variant,
                            self.opts.history_size,
                            [u8]
                        );
                        self.node = Some(Iceoryx2NodeHandle::Local(node));
                        self.subscriber = Some(Iceoryx2SliceSubscriberPort::Local(subscriber));
                    }
                }
            }
            Iceoryx2Mode::Threaded | Iceoryx2Mode::Signaled => {
                state.always_callback();
                let (sender, receiver) = channel_pair(None);
                self.receiver = Some(receiver);
                self.running.store(true, Ordering::SeqCst);

                let service_name = self.service_name.clone();
                let opts = self.opts.clone();
                let running = self.running.clone();

                self.thread_handle = Some(thread::spawn(move || {
                    if let Err(e) = run_slice_subscriber_thread(service_name, sender, opts, running)
                    {
                        log::error!("iceoryx2 slice subscriber thread error: {:?}", e);
                    }
                }));
            }
        }
        Ok(())
    }

    fn stop(&mut self, _state: &mut crate::GraphState) -> anyhow::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
        self.subscriber = None;
        self.receiver = None;
        self.node = None;
        Ok(())
    }

    fn upstreams(&self) -> crate::UpStreams {
        crate::UpStreams::none()
    }
}

impl crate::StreamPeekRef<Burst<Vec<u8>>> for Iceoryx2SliceReceiverStream {
    fn peek_ref(&self) -> &Burst<Vec<u8>> {
        &self.value
    }
}

// ---------------------------------------------------------------------------
// Background thread implementations (typed)
// ---------------------------------------------------------------------------

fn run_subscriber_thread<T>(
    service_name: String,
    channel_sender: ChannelSender<T>,
    opts: Iceoryx2SubOpts,
    running: Arc<AtomicBool>,
) -> Iceoryx2Result<()>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    let res: Iceoryx2Result<()> = match opts.variant {
        Iceoryx2ServiceVariant::Ipc => {
            let (node, subscriber) = create_subscriber!(
                ipc::Service,
                &service_name,
                opts.variant,
                opts.history_size,
                T
            );
            run_poll_loop!(
                ipc::Service,
                subscriber,
                channel_sender,
                opts,
                running,
                service_name,
                node,
                |s: &T| *s
            )
        }
        Iceoryx2ServiceVariant::Local => {
            let (node, subscriber) = create_subscriber!(
                local::Service,
                &service_name,
                opts.variant,
                opts.history_size,
                T
            );
            run_poll_loop!(
                local::Service,
                subscriber,
                channel_sender,
                opts,
                running,
                service_name,
                node,
                |s: &T| *s
            )
        }
    };

    if let Err(ref e) = res {
        let _ =
            channel_sender.send_message(Message::Error(Arc::new(anyhow::anyhow!(e.to_string()))));
    }
    let _ = channel_sender.send_message(Message::EndOfStream);
    res
}

// ---------------------------------------------------------------------------
// Background thread implementations (slice)
// ---------------------------------------------------------------------------

fn run_slice_subscriber_thread(
    service_name: String,
    channel_sender: ChannelSender<Vec<u8>>,
    opts: Iceoryx2SubOpts,
    running: Arc<AtomicBool>,
) -> Iceoryx2Result<()> {
    let res: Iceoryx2Result<()> = match opts.variant {
        Iceoryx2ServiceVariant::Ipc => {
            let (node, subscriber) = create_subscriber!(
                ipc::Service,
                &service_name,
                opts.variant,
                opts.history_size,
                [u8]
            );
            run_poll_loop!(
                ipc::Service,
                subscriber,
                channel_sender,
                opts,
                running,
                service_name,
                node,
                |s: &[u8]| s.to_vec()
            )
        }
        Iceoryx2ServiceVariant::Local => {
            let (node, subscriber) = create_subscriber!(
                local::Service,
                &service_name,
                opts.variant,
                opts.history_size,
                [u8]
            );
            run_poll_loop!(
                local::Service,
                subscriber,
                channel_sender,
                opts,
                running,
                service_name,
                node,
                |s: &[u8]| s.to_vec()
            )
        }
    };

    if let Err(ref e) = res {
        let _ =
            channel_sender.send_message(Message::Error(Arc::new(anyhow::anyhow!(e.to_string()))));
    }
    let _ = channel_sender.send_message(Message::EndOfStream);
    res
}
