//! iceoryx2 publisher (write) implementation — the sink extension traits.

use std::cell::RefCell;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::update_connections::UpdateConnections;
use iceoryx2::prelude::*;

use crate::Burst;
use crate::fluent::Stream;
use crate::interp::StopHandle;
use crate::op::{Activation, Ctx, Tick};

use super::read::open_event_service;
use super::{
    Iceoryx2Error, Iceoryx2NodeHandle, Iceoryx2PubOpts, Iceoryx2PubSliceOpts,
    Iceoryx2ServiceContract, Iceoryx2ServiceVariant, iceoryx2_default_config,
    service_open_err_with_context,
};

/// How often (in cycles) a port refreshes its connections, matching legacy.
const UPDATE_CONNECTIONS_EVERY: u64 = 10;

// ---------------------------------------------------------------------------
// Macros to eliminate Ipc/Local duplication
// ---------------------------------------------------------------------------

/// Create a node, open/create a pub-sub service, build a publisher, and create
/// an event notifier for signalling `Signaled` subscribers.
macro_rules! create_publisher_and_notifier {
    ($svc:ty, $service_name:expr, $variant:expr, $history_size:expr, $payload:ty
     $(, $initial_max_slice_len:expr)?) => {{
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

        let publisher = service
            .publisher_builder()
            $(.initial_max_slice_len($initial_max_slice_len))?
            .create()
            .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
        publisher
            .update_connections()
            .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;

        let signal_name = format!("{}.signal", $service_name);
        let event_service = open_event_service!(node, signal_name, $variant);
        let notifier = event_service
            .notifier_builder()
            .create()
            .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;

        (node, publisher, notifier)
    }};
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

enum Iceoryx2PublisherPort<T>
where
    T: ZeroCopySend + Clone + Copy + Send + std::fmt::Debug + 'static,
{
    Ipc(Publisher<ipc::Service, T, ()>),
    Local(Publisher<local::Service, T, ()>),
}

impl<T> Iceoryx2PublisherPort<T>
where
    T: ZeroCopySend + Clone + Copy + Send + std::fmt::Debug + 'static,
{
    fn update_connections(&self) -> Result<()> {
        match self {
            Self::Ipc(p) => p.update_connections()?,
            Self::Local(p) => p.update_connections()?,
        }
        Ok(())
    }

    fn send(&self, data: &T) -> Result<()> {
        match self {
            Self::Ipc(publisher) => {
                let sample = publisher
                    .loan_uninit()
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                sample
                    .write_payload(*data)
                    .send()
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
            }
            Self::Local(publisher) => {
                let sample = publisher
                    .loan_uninit()
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                sample
                    .write_payload(*data)
                    .send()
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
            }
        }
        Ok(())
    }
}

enum Iceoryx2SlicePublisherPort {
    Ipc(Publisher<ipc::Service, [u8], ()>),
    Local(Publisher<local::Service, [u8], ()>),
}

impl Iceoryx2SlicePublisherPort {
    fn update_connections(&self) -> Result<()> {
        match self {
            Self::Ipc(p) => p.update_connections()?,
            Self::Local(p) => p.update_connections()?,
        }
        Ok(())
    }

    fn send(&self, data: &[u8]) -> Result<()> {
        match self {
            Self::Ipc(publisher) => {
                let sample = publisher
                    .loan_slice_uninit(data.len())
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                sample
                    .write_from_slice(data)
                    .send()
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
            }
            Self::Local(publisher) => {
                let sample = publisher
                    .loan_slice_uninit(data.len())
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                sample
                    .write_from_slice(data)
                    .send()
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
            }
        }
        Ok(())
    }
}

enum Iceoryx2NotifierPort {
    Ipc(Notifier<ipc::Service>),
    Local(Notifier<local::Service>),
}

impl Iceoryx2NotifierPort {
    fn notify(&self) -> Result<()> {
        match self {
            Self::Ipc(n) => {
                n.notify()
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
            }
            Self::Local(n) => {
                n.notify()
                    .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
            }
        }
        Ok(())
    }
}

/// Graph-thread state for a publisher: the ports created at `start()` plus the
/// cycle counter driving periodic connection refreshes.
///
/// Single-threaded interior mutability (`Rc<RefCell<..>>`), never a lock — the
/// no-locks-on-the-graph-path invariant.
struct PubState<P> {
    service_name: String,
    node: Option<Iceoryx2NodeHandle>,
    port: Option<P>,
    notifier: Option<Iceoryx2NotifierPort>,
    cycles: u64,
}

impl<P> PubState<P> {
    fn new(service_name: String) -> Self {
        Self {
            service_name,
            node: None,
            port: None,
            notifier: None,
            cycles: 0,
        }
    }
}

/// Releases the publisher, notifier and node at teardown.
struct PubGuard<P>(Rc<RefCell<PubState<P>>>);

impl<P> Drop for PubGuard<P> {
    fn drop(&mut self) {
        let mut st = self.0.borrow_mut();
        st.port = None;
        st.notifier = None;
        st.node = None;
    }
}

// ---------------------------------------------------------------------------
// Typed sink
// ---------------------------------------------------------------------------

/// Extension trait providing a fluent API for publishing typed streams over
/// iceoryx2 shared memory.
///
/// Implemented for `Stream<Burst<T>>` where `T` is a `#[repr(C)]`,
/// `ZeroCopySend`, self-contained payload. Bring it in with
/// `use wingfoil_next::adapters::iceoryx2::Iceoryx2SinkOps;`.
pub trait Iceoryx2SinkOps {
    /// Publish this stream to `service_name` with [`Iceoryx2PubOpts::default`]
    /// (the `Ipc` variant). Returns the sink `Stream<()>`.
    fn iceoryx2_pub(&self, service_name: &str) -> Stream<()>;

    /// Publish this stream to `service_name` using the selected service variant.
    fn iceoryx2_pub_with(&self, service_name: &str, variant: Iceoryx2ServiceVariant) -> Stream<()>;

    /// Publish this stream to `service_name` with explicit options.
    ///
    /// One shared-memory sample is loaned, written and sent per value in the
    /// burst; a non-empty burst is followed by a notification on the service's
    /// `<name>.signal` Event service so a
    /// [`Signaled`](super::Iceoryx2Mode::Signaled) subscriber wakes. The sink
    /// ticks only when it published something.
    ///
    /// The ports are created at graph `start()` (legacy parity), so an invalid
    /// service name or a contract mismatch aborts the *run*, not wiring; a
    /// loan/send failure aborts the run with context from the firing cycle.
    /// Unlike the telemetry sinks, this one publishes under **either** run mode
    /// — legacy parity (see the module docs).
    fn iceoryx2_pub_opts(&self, service_name: &str, opts: Iceoryx2PubOpts) -> Stream<()>;
}

impl<T> Iceoryx2SinkOps for Stream<Burst<T>>
where
    T: ZeroCopySend + Clone + Copy + Default + Send + std::fmt::Debug + 'static,
{
    fn iceoryx2_pub(&self, service_name: &str) -> Stream<()> {
        self.iceoryx2_pub_opts(service_name, Iceoryx2PubOpts::default())
    }

    fn iceoryx2_pub_with(&self, service_name: &str, variant: Iceoryx2ServiceVariant) -> Stream<()> {
        self.iceoryx2_pub_opts(
            service_name,
            Iceoryx2PubOpts {
                variant,
                ..Default::default()
            },
        )
    }

    fn iceoryx2_pub_opts(&self, service_name: &str, opts: Iceoryx2PubOpts) -> Stream<()> {
        let state: Rc<RefCell<PubState<Iceoryx2PublisherPort<T>>>> =
            Rc::new(RefCell::new(PubState::new(service_name.to_string())));
        let start_state = state.clone();
        self.wire(move |b, h| {
            let sink = b.register_op1(
                h,
                "iceoryx2_pub",
                Activation::NONE,
                state,
                || (),
                move |state: &mut Rc<RefCell<PubState<Iceoryx2PublisherPort<T>>>>,
                      _s: &mut (),
                      burst: &Burst<T>,
                      _ctx: &mut Ctx<'_>| {
                    let st = &mut *state.borrow_mut();
                    let Some(port) = &st.port else {
                        return Ok(Tick::Quiet);
                    };
                    st.cycles += 1;
                    if st.cycles.is_multiple_of(UPDATE_CONNECTIONS_EVERY) {
                        port.update_connections()?;
                    }
                    let mut sent_any = false;
                    for data in burst.iter() {
                        port.send(data)?;
                        sent_any = true;
                    }
                    if sent_any && let Some(n) = &st.notifier {
                        n.notify()?;
                    }
                    Ok(if sent_any {
                        Tick::Value(())
                    } else {
                        Tick::Quiet
                    })
                },
            );
            // Create the publisher, notifier and node at graph `start()`, as
            // legacy's `start` does — wiring stays pure, and a bad service name
            // or contract mismatch aborts the run with node context.
            b.compose_spawn_at_start(sink.index(), move |_run_mode, _run_for, _start_time| {
                {
                    let st = &mut *start_state.borrow_mut();
                    let (node, port, notifier) = match opts.variant {
                        Iceoryx2ServiceVariant::Ipc => {
                            let (node, p, n) = create_publisher_and_notifier!(
                                ipc::Service,
                                &st.service_name,
                                opts.variant,
                                opts.history_size,
                                T
                            );
                            (
                                Iceoryx2NodeHandle::Ipc(node),
                                Iceoryx2PublisherPort::Ipc(p),
                                Iceoryx2NotifierPort::Ipc(n),
                            )
                        }
                        Iceoryx2ServiceVariant::Local => {
                            let (node, p, n) = create_publisher_and_notifier!(
                                local::Service,
                                &st.service_name,
                                opts.variant,
                                opts.history_size,
                                T
                            );
                            (
                                Iceoryx2NodeHandle::Local(node),
                                Iceoryx2PublisherPort::Local(p),
                                Iceoryx2NotifierPort::Local(n),
                            )
                        }
                    };
                    st.node = Some(node);
                    st.port = Some(port);
                    st.notifier = Some(notifier);
                }
                Ok(StopHandle::new(PubGuard(start_state.clone())))
            });
            sink
        })
    }
}

// ---------------------------------------------------------------------------
// Slice sink
// ---------------------------------------------------------------------------

/// Extension trait providing a fluent API for publishing byte-slice streams over
/// iceoryx2 shared memory.
///
/// Implemented for `Stream<Burst<Vec<u8>>>`, so variable-length payloads need no
/// `ZeroCopySend` type of their own. Bring it in with
/// `use wingfoil_next::adapters::iceoryx2::Iceoryx2SliceSinkOps;`.
pub trait Iceoryx2SliceSinkOps {
    /// Publish this stream to a `[u8]` service with
    /// [`Iceoryx2PubSliceOpts::default`] (the `Ipc` variant).
    fn iceoryx2_pub_slice(&self, service_name: &str) -> Stream<()>;

    /// Publish this stream to a `[u8]` service using the selected variant.
    fn iceoryx2_pub_slice_with(
        &self,
        service_name: &str,
        variant: Iceoryx2ServiceVariant,
    ) -> Stream<()>;

    /// Publish this stream to a `[u8]` service with explicit options.
    ///
    /// Each `Vec<u8>` in the burst is loaned as a slice sample of exactly its
    /// length (bounded by `initial_max_slice_len`), written and sent; see
    /// [`Iceoryx2SinkOps::iceoryx2_pub_opts`] for the shared lifecycle and error
    /// semantics.
    fn iceoryx2_pub_slice_opts(&self, service_name: &str, opts: Iceoryx2PubSliceOpts)
    -> Stream<()>;
}

impl Iceoryx2SliceSinkOps for Stream<Burst<Vec<u8>>> {
    fn iceoryx2_pub_slice(&self, service_name: &str) -> Stream<()> {
        self.iceoryx2_pub_slice_opts(service_name, Iceoryx2PubSliceOpts::default())
    }

    fn iceoryx2_pub_slice_with(
        &self,
        service_name: &str,
        variant: Iceoryx2ServiceVariant,
    ) -> Stream<()> {
        self.iceoryx2_pub_slice_opts(
            service_name,
            Iceoryx2PubSliceOpts {
                variant,
                ..Default::default()
            },
        )
    }

    fn iceoryx2_pub_slice_opts(
        &self,
        service_name: &str,
        opts: Iceoryx2PubSliceOpts,
    ) -> Stream<()> {
        let state: Rc<RefCell<PubState<Iceoryx2SlicePublisherPort>>> =
            Rc::new(RefCell::new(PubState::new(service_name.to_string())));
        let start_state = state.clone();
        self.wire(move |b, h| {
            let sink = b.register_op1(
                h,
                "iceoryx2_pub_slice",
                Activation::NONE,
                state,
                || (),
                move |state: &mut Rc<RefCell<PubState<Iceoryx2SlicePublisherPort>>>,
                      _s: &mut (),
                      burst: &Burst<Vec<u8>>,
                      _ctx: &mut Ctx<'_>| {
                    let st = &mut *state.borrow_mut();
                    let Some(port) = &st.port else {
                        return Ok(Tick::Quiet);
                    };
                    st.cycles += 1;
                    if st.cycles.is_multiple_of(UPDATE_CONNECTIONS_EVERY) {
                        port.update_connections()?;
                    }
                    let mut sent_any = false;
                    for data in burst.iter() {
                        port.send(data)?;
                        sent_any = true;
                    }
                    if sent_any && let Some(n) = &st.notifier {
                        n.notify()?;
                    }
                    Ok(if sent_any {
                        Tick::Value(())
                    } else {
                        Tick::Quiet
                    })
                },
            );
            b.compose_spawn_at_start(sink.index(), move |_run_mode, _run_for, _start_time| {
                {
                    let st = &mut *start_state.borrow_mut();
                    let (node, port, notifier) = match opts.variant {
                        Iceoryx2ServiceVariant::Ipc => {
                            let (node, p, n) = create_publisher_and_notifier!(
                                ipc::Service,
                                &st.service_name,
                                opts.variant,
                                opts.history_size,
                                [u8],
                                opts.initial_max_slice_len
                            );
                            (
                                Iceoryx2NodeHandle::Ipc(node),
                                Iceoryx2SlicePublisherPort::Ipc(p),
                                Iceoryx2NotifierPort::Ipc(n),
                            )
                        }
                        Iceoryx2ServiceVariant::Local => {
                            let (node, p, n) = create_publisher_and_notifier!(
                                local::Service,
                                &st.service_name,
                                opts.variant,
                                opts.history_size,
                                [u8],
                                opts.initial_max_slice_len
                            );
                            (
                                Iceoryx2NodeHandle::Local(node),
                                Iceoryx2SlicePublisherPort::Local(p),
                                Iceoryx2NotifierPort::Local(n),
                            )
                        }
                    };
                    st.node = Some(node);
                    st.port = Some(port);
                    st.notifier = Some(notifier);
                }
                Ok(StopHandle::new(PubGuard(start_state.clone())))
            });
            sink
        })
    }
}
