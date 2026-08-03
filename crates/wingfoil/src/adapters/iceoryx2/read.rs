//! iceoryx2 subscriber (read) implementation — the three polling modes.

use core::time::Duration;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result};
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::port::update_connections::UpdateConnections;
use iceoryx2::prelude::*;
use wingfoil::RunMode;

use crate::Burst;
use crate::channel::ChannelSender;
use crate::fluent::{GraphBuilder, SourceOps, Stream};
use crate::interp::StopHandle;
use crate::op::Activation;
use crate::op::Tick;

use super::{
    Iceoryx2Error, Iceoryx2Mode, Iceoryx2NodeHandle, Iceoryx2Result, Iceoryx2ServiceContract,
    Iceoryx2ServiceVariant, Iceoryx2SubOpts, iceoryx2_default_config,
    service_open_err_with_context,
};

/// How often (in cycles) a port refreshes its connections, matching legacy.
const UPDATE_CONNECTIONS_EVERY: u64 = 10;

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

/// Run the background subscriber poll loop (both the `Signaled` and `Threaded`
/// paths), forwarding each received sample onto the channel.
macro_rules! run_poll_loop {
    ($svc:ty, $subscriber:expr, $sender:expr, $opts:expr, $running:expr,
     $service_name:expr, $node:expr, $extract:expr) => {{
        if $opts.mode == Iceoryx2Mode::Signaled {
            let signal_name = format!("{}.signal", $service_name);
            let event_service = open_event_service!($node, signal_name, $opts.variant);
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

            while $running.load(Ordering::Relaxed) {
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
                    drop(sample);
                    // `send` returns false once the graph is gone — a normal
                    // teardown race, so stop producing quietly.
                    if !$sender.send(data) {
                        return Ok(());
                    }
                }

                while let Ok(Some(_)) = listener.try_wait_one() {}
            }
        } else {
            let mut loop_cycles = 0u64;
            while $running.load(Ordering::Relaxed) {
                loop_cycles += 1;
                if loop_cycles % 100 == 0 {
                    let _ = $subscriber.update_connections();
                }

                let mut received = false;
                while let Ok(Some(sample)) = $subscriber.receive() {
                    let data = $extract(&sample);
                    drop(sample);
                    if !$sender.send(data) {
                        return Ok(());
                    }
                    received = true;
                }

                if !received {
                    thread::sleep(Duration::from_micros(10));
                }
            }
        }
        Ok(())
    }};
}

/// Open (or create) the `<service>.signal` Event service, retrying briefly —
/// the publisher may not have created it yet. Ported from legacy.
macro_rules! open_event_service {
    ($node:expr, $signal_name:expr, $variant:expr) => {{
        let mut last_err: Option<String> = None;
        let mut service = None;
        for _ in 0..25 {
            match $node
                .service_builder(&$signal_name.as_str().try_into().map_err(
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
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
        service.ok_or_else(|| {
            service_open_err_with_context(
                &$signal_name,
                $variant,
                Iceoryx2ServiceContract::new(0),
                last_err.unwrap_or_else(|| "event open_or_create failed".to_string()),
            )
        })?
    }};
}

pub(crate) use open_event_service;

// ---------------------------------------------------------------------------
// Typed subscriber (Burst<T>)
// ---------------------------------------------------------------------------

/// Subscribe to an iceoryx2 service and emit each cycle's samples as a burst.
///
/// Uses [`Iceoryx2SubOpts::default`] — the `Ipc` variant in [`Iceoryx2Mode::Spin`]
/// mode. See [`iceoryx2_sub_opts`] for the full knob set.
///
/// # Errors
///
/// See [`iceoryx2_sub_opts`] — a historical run is rejected at wiring.
pub fn iceoryx2_sub<T>(
    g: &GraphBuilder,
    run_mode: RunMode,
    service_name: &str,
) -> Result<Stream<Burst<T>>>
where
    T: ZeroCopySend + Clone + Copy + Default + Send + std::fmt::Debug + 'static,
{
    iceoryx2_sub_opts(g, run_mode, service_name, Iceoryx2SubOpts::default())
}

/// Subscribe to an iceoryx2 service using the selected service variant.
///
/// # Errors
///
/// See [`iceoryx2_sub_opts`] — a historical run is rejected at wiring.
pub fn iceoryx2_sub_with<T>(
    g: &GraphBuilder,
    run_mode: RunMode,
    service_name: &str,
    variant: Iceoryx2ServiceVariant,
) -> Result<Stream<Burst<T>>>
where
    T: ZeroCopySend + Clone + Copy + Default + Send + std::fmt::Debug + 'static,
{
    iceoryx2_sub_opts(
        g,
        run_mode,
        service_name,
        Iceoryx2SubOpts {
            variant,
            ..Default::default()
        },
    )
}

/// Subscribe to an iceoryx2 service with explicit options.
///
/// The subscriber port is created at graph `start()` (legacy parity), so an
/// invalid service name or a contract mismatch aborts the run rather than
/// wiring. Samples received between cycles group into one [`Burst`].
///
/// # Errors
///
/// Returns an error at **wiring time** if `run_mode` is
/// [`RunMode::HistoricalFrom`]: an iceoryx2 subscription is a live, unbounded
/// source with no historical timeline to replay, and the `Threaded`/`Signaled`
/// modes ride the channel layer, whose historical receiver would block-collect
/// the never-closing producer and deadlock at `start`. Run under
/// [`RunMode::RealTime`].
///
/// A node/service/port creation failure surfaces during the run: at `start()`
/// for [`Iceoryx2Mode::Spin`], or via the channel's error path for the other two
/// modes (the ports live on the background thread).
pub fn iceoryx2_sub_opts<T>(
    g: &GraphBuilder,
    run_mode: RunMode,
    service_name: &str,
    opts: Iceoryx2SubOpts,
) -> Result<Stream<Burst<T>>>
where
    T: ZeroCopySend + Clone + Copy + Default + Send + std::fmt::Debug + 'static,
{
    reject_historical(run_mode, "iceoryx2_sub")?;
    let service_name = service_name.to_string();
    Ok(match opts.mode {
        Iceoryx2Mode::Spin => spin_source(g, service_name, opts),
        Iceoryx2Mode::Threaded | Iceoryx2Mode::Signaled => {
            threaded_source(g, service_name, opts, run_subscriber_thread::<T>)
        }
    })
}

/// Subscribe to a byte-slice service.
///
/// Uses [`Iceoryx2SubOpts::default`]; see [`iceoryx2_sub_slice_opts`].
///
/// # Errors
///
/// See [`iceoryx2_sub_opts`] — a historical run is rejected at wiring.
pub fn iceoryx2_sub_slice(
    g: &GraphBuilder,
    run_mode: RunMode,
    service_name: &str,
) -> Result<Stream<Burst<Vec<u8>>>> {
    iceoryx2_sub_slice_opts(g, run_mode, service_name, Iceoryx2SubOpts::default())
}

/// Subscribe to a byte-slice service with explicit options.
///
/// Each received `[u8]` sample is copied out into a `Vec<u8>` (the sample's
/// shared-memory loan is released immediately), so variable-length payloads need
/// no `ZeroCopySend` type of their own.
///
/// # Errors
///
/// See [`iceoryx2_sub_opts`] — a historical run is rejected at wiring.
pub fn iceoryx2_sub_slice_opts(
    g: &GraphBuilder,
    run_mode: RunMode,
    service_name: &str,
    opts: Iceoryx2SubOpts,
) -> Result<Stream<Burst<Vec<u8>>>> {
    reject_historical(run_mode, "iceoryx2_sub_slice")?;
    let service_name = service_name.to_string();
    Ok(match opts.mode {
        Iceoryx2Mode::Spin => spin_slice_source(g, service_name, opts),
        Iceoryx2Mode::Threaded | Iceoryx2Mode::Signaled => {
            threaded_source(g, service_name, opts, run_slice_subscriber_thread)
        }
    })
}

/// The shared wiring-time live-source guard (register B2).
fn reject_historical(run_mode: RunMode, who: &str) -> Result<()> {
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "{who}: RunMode::HistoricalFrom is unsupported — an iceoryx2 subscription \
             is a live, unbounded source with no historical timeline to replay; run \
             {who} under RunMode::RealTime"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Spin mode — a busy-spin custom_node polling on the graph thread
// ---------------------------------------------------------------------------

enum Iceoryx2SubscriberPort<T>
where
    T: ZeroCopySend + Clone + Copy + Default + Send + std::fmt::Debug + 'static,
{
    Ipc(Subscriber<ipc::Service, T, ()>),
    Local(Subscriber<local::Service, T, ()>),
}

impl<T> Iceoryx2SubscriberPort<T>
where
    T: ZeroCopySend + Clone + Copy + Default + Send + std::fmt::Debug + 'static,
{
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

    fn receive_into(&self, burst: &mut Burst<T>) {
        match self {
            Self::Ipc(subscriber) => {
                while let Ok(Some(sample)) = subscriber.receive() {
                    burst.push(*sample);
                    drop(sample);
                }
            }
            Self::Local(subscriber) => {
                while let Ok(Some(sample)) = subscriber.receive() {
                    burst.push(*sample);
                    drop(sample);
                }
            }
        }
    }
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

/// Graph-thread state for a spin-mode subscriber: the ports created at
/// `start()` plus the cycle counter driving periodic connection refreshes.
///
/// Single-threaded interior mutability (`Rc<RefCell<..>>`), never a lock — the
/// no-locks-on-the-graph-path invariant.
struct SpinSubState<P> {
    service_name: String,
    opts: Iceoryx2SubOpts,
    node: Option<Iceoryx2NodeHandle>,
    port: Option<P>,
    cycles: u64,
}

impl<P> SpinSubState<P> {
    fn new(service_name: String, opts: Iceoryx2SubOpts) -> Self {
        Self {
            service_name,
            opts,
            node: None,
            port: None,
            cycles: 0,
        }
    }
}

/// Releases the subscriber port and its node at teardown.
struct SpinSubGuard<P>(Rc<RefCell<SpinSubState<P>>>);

impl<P> Drop for SpinSubGuard<P> {
    fn drop(&mut self) {
        let mut st = self.0.borrow_mut();
        st.port = None;
        st.node = None;
    }
}

/// Wire a `Spin` typed subscriber: a busy-spin `custom_node` draining the port
/// each cycle, with port creation deferred to graph `start()`.
fn spin_source<T>(g: &GraphBuilder, service_name: String, opts: Iceoryx2SubOpts) -> Stream<Burst<T>>
where
    T: ZeroCopySend + Clone + Copy + Default + Send + std::fmt::Debug + 'static,
{
    let state: Rc<RefCell<SpinSubState<Iceoryx2SubscriberPort<T>>>> =
        Rc::new(RefCell::new(SpinSubState::new(service_name, opts)));
    let cycle_state = state.clone();

    let events = g.custom_node::<Burst<T>, _>(&[], &[], Activation::ALWAYS, move |_ctx| {
        let st = &mut *cycle_state.borrow_mut();
        let Some(port) = &st.port else {
            return Ok(Tick::Quiet);
        };
        st.cycles += 1;
        if st.cycles.is_multiple_of(UPDATE_CONNECTIONS_EVERY) {
            port.update_connections_periodic();
        }
        let mut burst = Burst::default();
        port.receive_into(&mut burst);
        Ok(if burst.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(burst)
        })
    });

    let idx = events.handle().index();
    let start_state = state;
    g.with_builder(move |b| {
        b.compose_spawn_at_start(idx, move |_run_mode, _run_for, _start_time| {
            {
                let st = &mut *start_state.borrow_mut();
                let (node, port) = match st.opts.variant {
                    Iceoryx2ServiceVariant::Ipc => {
                        let (node, sub) = create_subscriber!(
                            ipc::Service,
                            &st.service_name,
                            st.opts.variant,
                            st.opts.history_size,
                            T
                        );
                        (
                            Iceoryx2NodeHandle::Ipc(node),
                            Iceoryx2SubscriberPort::Ipc(sub),
                        )
                    }
                    Iceoryx2ServiceVariant::Local => {
                        let (node, sub) = create_subscriber!(
                            local::Service,
                            &st.service_name,
                            st.opts.variant,
                            st.opts.history_size,
                            T
                        );
                        (
                            Iceoryx2NodeHandle::Local(node),
                            Iceoryx2SubscriberPort::Local(sub),
                        )
                    }
                };
                st.node = Some(node);
                st.port = Some(port);
            }
            Ok(StopHandle::new(SpinSubGuard(start_state.clone())))
        });
    });

    events
}

/// Wire a `Spin` slice subscriber — the `[u8]` twin of [`spin_source`].
fn spin_slice_source(
    g: &GraphBuilder,
    service_name: String,
    opts: Iceoryx2SubOpts,
) -> Stream<Burst<Vec<u8>>> {
    let state: Rc<RefCell<SpinSubState<Iceoryx2SliceSubscriberPort>>> =
        Rc::new(RefCell::new(SpinSubState::new(service_name, opts)));
    let cycle_state = state.clone();

    let events = g.custom_node::<Burst<Vec<u8>>, _>(&[], &[], Activation::ALWAYS, move |_ctx| {
        let st = &mut *cycle_state.borrow_mut();
        let Some(port) = &st.port else {
            return Ok(Tick::Quiet);
        };
        st.cycles += 1;
        if st.cycles.is_multiple_of(UPDATE_CONNECTIONS_EVERY) {
            port.update_connections_periodic();
        }
        let mut burst = Burst::default();
        port.receive_into(&mut burst);
        Ok(if burst.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(burst)
        })
    });

    let idx = events.handle().index();
    let start_state = state;
    g.with_builder(move |b| {
        b.compose_spawn_at_start(idx, move |_run_mode, _run_for, _start_time| {
            {
                let st = &mut *start_state.borrow_mut();
                let (node, port) = match st.opts.variant {
                    Iceoryx2ServiceVariant::Ipc => {
                        let (node, sub) = create_subscriber!(
                            ipc::Service,
                            &st.service_name,
                            st.opts.variant,
                            st.opts.history_size,
                            [u8]
                        );
                        (
                            Iceoryx2NodeHandle::Ipc(node),
                            Iceoryx2SliceSubscriberPort::Ipc(sub),
                        )
                    }
                    Iceoryx2ServiceVariant::Local => {
                        let (node, sub) = create_subscriber!(
                            local::Service,
                            &st.service_name,
                            st.opts.variant,
                            st.opts.history_size,
                            [u8]
                        );
                        (
                            Iceoryx2NodeHandle::Local(node),
                            Iceoryx2SliceSubscriberPort::Local(sub),
                        )
                    }
                };
                st.node = Some(node);
                st.port = Some(port);
            }
            Ok(StopHandle::new(SpinSubGuard(start_state.clone())))
        });
    });

    events
}

// ---------------------------------------------------------------------------
// Threaded / Signaled modes — a background thread over the channel layer
// ---------------------------------------------------------------------------

/// Sets the shared stop flag at teardown so the background poll loop exits on
/// its next turn instead of leaking.
struct ThreadStopGuard(Arc<AtomicBool>);

impl Drop for ThreadStopGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// Wire a `Threaded`/`Signaled` subscriber: `source_at_start` allocates the
/// channel at wiring but spawns the polling thread — which creates the ports —
/// at graph `start()`, returning a stop guard dropped at teardown.
fn threaded_source<T, Run>(
    g: &GraphBuilder,
    service_name: String,
    opts: Iceoryx2SubOpts,
    run: Run,
) -> Stream<Burst<T>>
where
    T: Clone + Default + Send + 'static,
    Run: Fn(String, ChannelSender<T>, Iceoryx2SubOpts, Arc<AtomicBool>) -> Iceoryx2Result<()>
        + Copy
        + Send
        + 'static,
{
    g.source_at_start::<T, _>(move |sender| {
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = running.clone();
        let service_name = service_name.clone();
        let opts = opts.clone();
        thread::Builder::new()
            .name("iceoryx2-sub".into())
            .spawn(move || {
                if let Err(e) = run(service_name, sender.clone(), opts, thread_running) {
                    sender.send_error(anyhow::anyhow!(e.to_string()));
                }
            })
            .context("iceoryx2_sub: spawning subscriber thread")?;
        Ok(StopHandle::new(ThreadStopGuard(running)))
    })
}

fn run_subscriber_thread<T>(
    service_name: String,
    sender: ChannelSender<T>,
    opts: Iceoryx2SubOpts,
    running: Arc<AtomicBool>,
) -> Iceoryx2Result<()>
where
    T: ZeroCopySend + Clone + Copy + Default + Send + std::fmt::Debug + 'static,
{
    match opts.variant {
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
                sender,
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
                sender,
                opts,
                running,
                service_name,
                node,
                |s: &T| *s
            )
        }
    }
}

fn run_slice_subscriber_thread(
    service_name: String,
    sender: ChannelSender<Vec<u8>>,
    opts: Iceoryx2SubOpts,
    running: Arc<AtomicBool>,
) -> Iceoryx2Result<()> {
    match opts.variant {
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
                sender,
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
                sender,
                opts,
                running,
                service_name,
                node,
                |s: &[u8]| s.to_vec()
            )
        }
    }
}
