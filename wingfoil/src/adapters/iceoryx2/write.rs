//! iceoryx2 publisher (write) implementation

use std::rc::Rc;
use std::thread;
use std::time::Duration;

use crate::types::{Element, IntoNode, UpStreams};
use crate::{Burst, GraphState, MutableNode, Node, Stream};

use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::update_connections::UpdateConnections;
use iceoryx2::prelude::*;
use iceoryx2::service::service_name::ServiceNameError;

use super::{
    Iceoryx2Error, Iceoryx2PubOpts, Iceoryx2PubSliceOpts, Iceoryx2ServiceContract,
    Iceoryx2ServiceVariant,
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

/// Publish a `Burst<T>` stream to an iceoryx2 service.
///
/// # Type Parameters
/// - `T`: Must implement `ZeroCopySend`, `Clone`, `Copy`, `Debug`, `Default`, `'static`
///
/// # Arguments
/// - `upstream`: The stream to publish
/// - `service_name`: The iceoryx2 service name (e.g., "my/service")
///
/// # Returns
/// A node that publishes to the service.
pub fn iceoryx2_pub<T>(upstream: Rc<dyn Stream<Burst<T>>>, service_name: &str) -> Rc<dyn Node>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    iceoryx2_pub_opts(upstream, service_name, Iceoryx2PubOpts::default())
}

/// Publish a `Burst<T>` stream to an iceoryx2 service using the selected service variant.
pub fn iceoryx2_pub_with<T>(
    upstream: Rc<dyn Stream<Burst<T>>>,
    service_name: &str,
    variant: Iceoryx2ServiceVariant,
) -> Rc<dyn Node>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    iceoryx2_pub_opts(
        upstream,
        service_name,
        Iceoryx2PubOpts {
            variant,
            ..Default::default()
        },
    )
}

/// Publish a `Burst<T>` stream to an iceoryx2 service using explicit options.
pub fn iceoryx2_pub_opts<T>(
    upstream: Rc<dyn Stream<Burst<T>>>,
    service_name: &str,
    opts: Iceoryx2PubOpts,
) -> Rc<dyn Node>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    Iceoryx2Publisher::new(upstream, service_name.to_string(), opts).into_node()
}

/// Publish a stream of byte vectors to an iceoryx2 slice service.
pub fn iceoryx2_pub_slice(
    upstream: Rc<dyn Stream<Burst<Vec<u8>>>>,
    service_name: &str,
) -> Rc<dyn Node> {
    iceoryx2_pub_slice_opts(upstream, service_name, Iceoryx2PubSliceOpts::default())
}

/// Publish a stream of byte vectors to an iceoryx2 slice service with a variant.
pub fn iceoryx2_pub_slice_with(
    upstream: Rc<dyn Stream<Burst<Vec<u8>>>>,
    service_name: &str,
    variant: Iceoryx2ServiceVariant,
) -> Rc<dyn Node> {
    iceoryx2_pub_slice_opts(
        upstream,
        service_name,
        Iceoryx2PubSliceOpts {
            variant,
            ..Default::default()
        },
    )
}

/// Publish a stream of byte vectors to an iceoryx2 slice service with explicit options.
pub fn iceoryx2_pub_slice_opts(
    upstream: Rc<dyn Stream<Burst<Vec<u8>>>>,
    service_name: &str,
    opts: Iceoryx2PubSliceOpts,
) -> Rc<dyn Node> {
    Iceoryx2SlicePublisher::new(upstream, service_name.to_string(), opts).into_node()
}

enum Iceoryx2PublisherPort<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    Ipc(Publisher<ipc::Service, T, ()>),
    Local(Publisher<local::Service, T, ()>),
}

enum Iceoryx2NotifierPort {
    Ipc(Notifier<ipc::Service>),
    Local(Notifier<local::Service>),
}

struct Iceoryx2Publisher<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    upstream: Rc<dyn Stream<Burst<T>>>,
    service_name: String,
    opts: Iceoryx2PubOpts,
    publisher: Option<Iceoryx2PublisherPort<T>>,
    notifier: Option<Iceoryx2NotifierPort>,
    cycles: u64,
}

impl<T> Iceoryx2Publisher<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    fn new(
        upstream: Rc<dyn Stream<Burst<T>>>,
        service_name: String,
        opts: Iceoryx2PubOpts,
    ) -> Self {
        Self {
            upstream,
            service_name,
            opts,
            publisher: None,
            notifier: None,
            cycles: 0,
        }
    }
}

impl<T> MutableNode for Iceoryx2Publisher<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let Some(publisher) = &self.publisher else {
            return Ok(false);
        };

        self.cycles += 1;
        // Periodically update connections to handle late subscribers
        if self.cycles.is_multiple_of(10) {
            match publisher {
                Iceoryx2PublisherPort::Ipc(p) => p.update_connections()?,
                Iceoryx2PublisherPort::Local(p) => p.update_connections()?,
            }
        }

        let burst = self.upstream.peek_value();
        let mut sent_any = false;
        for data in burst {
            match publisher {
                Iceoryx2PublisherPort::Ipc(publisher) => {
                    let sample = publisher
                        .loan_uninit()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                    let sample = sample.write_payload(data);
                    sample
                        .send()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                }
                Iceoryx2PublisherPort::Local(publisher) => {
                    let sample = publisher
                        .loan_uninit()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                    let sample = sample.write_payload(data);
                    sample
                        .send()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                }
            }
            sent_any = true;
        }

        if sent_any && let Some(ref n) = self.notifier {
            match n {
                Iceoryx2NotifierPort::Ipc(notifier) => {
                    notifier
                        .notify()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                }
                Iceoryx2NotifierPort::Local(notifier) => {
                    notifier
                        .notify()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                }
            }
        }

        Ok(sent_any)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }

    fn start(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        let signal_name = format!("{}.signal", self.service_name);

        match self.opts.variant {
            Iceoryx2ServiceVariant::Ipc => {
                let node = NodeBuilder::new()
                    .create::<ipc::Service>()
                    .map_err(|e| Iceoryx2Error::NodeCreationFailed(e.to_string()))?;
                let contract = Iceoryx2ServiceContract::new(self.opts.history_size);
                let service = node
                    .service_builder(&self.service_name.as_str().try_into().map_err(
                        |e: ServiceNameError| Iceoryx2Error::Other(anyhow::anyhow!(e.to_string())),
                    )?)
                    .publish_subscribe::<T>()
                    .history_size(self.opts.history_size)
                    .subscriber_max_buffer_size(contract.subscriber_max_buffer_size)
                    .open_or_create()
                    .map_err(|e| {
                        service_open_err_with_context(
                            &self.service_name,
                            self.opts.variant,
                            contract,
                            e,
                        )
                    })?;
                let publisher = service
                    .publisher_builder()
                    .create()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                publisher
                    .update_connections()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                self.publisher = Some(Iceoryx2PublisherPort::Ipc(publisher));

                // Always create notifier just in case subscriber uses Signaled mode
                let event_service = {
                    let mut last_err: Option<String> = None;
                    let mut service = None;
                    for _ in 0..25 {
                        match node
                            .service_builder(&signal_name.as_str().try_into().map_err(
                                |e: ServiceNameError| {
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
                            &signal_name,
                            self.opts.variant,
                            Iceoryx2ServiceContract::new(0),
                            last_err.unwrap_or_else(|| "event open_or_create failed".to_string()),
                        )
                    })?
                };
                let notifier = event_service
                    .notifier_builder()
                    .create()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                self.notifier = Some(Iceoryx2NotifierPort::Ipc(notifier));
            }
            Iceoryx2ServiceVariant::Local => {
                let node = NodeBuilder::new()
                    .create::<local::Service>()
                    .map_err(|e| Iceoryx2Error::NodeCreationFailed(e.to_string()))?;
                let contract = Iceoryx2ServiceContract::new(self.opts.history_size);
                let service = node
                    .service_builder(&self.service_name.as_str().try_into().map_err(
                        |e: ServiceNameError| Iceoryx2Error::Other(anyhow::anyhow!(e.to_string())),
                    )?)
                    .publish_subscribe::<T>()
                    .history_size(self.opts.history_size)
                    .subscriber_max_buffer_size(contract.subscriber_max_buffer_size)
                    .open_or_create()
                    .map_err(|e| {
                        service_open_err_with_context(
                            &self.service_name,
                            self.opts.variant,
                            contract,
                            e,
                        )
                    })?;
                let publisher = service
                    .publisher_builder()
                    .create()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                publisher
                    .update_connections()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                self.publisher = Some(Iceoryx2PublisherPort::Local(publisher));

                let event_service = {
                    let mut last_err: Option<String> = None;
                    let mut service = None;
                    for _ in 0..25 {
                        match node
                            .service_builder(&signal_name.as_str().try_into().map_err(
                                |e: ServiceNameError| {
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
                            &signal_name,
                            self.opts.variant,
                            Iceoryx2ServiceContract::new(0),
                            last_err.unwrap_or_else(|| "event open_or_create failed".to_string()),
                        )
                    })?
                };
                let notifier = event_service
                    .notifier_builder()
                    .create()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                self.notifier = Some(Iceoryx2NotifierPort::Local(notifier));
            }
        }

        Ok(())
    }

    fn stop(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        self.publisher = None;
        self.notifier = None;
        Ok(())
    }
}

// Slice implementation
struct Iceoryx2SlicePublisher {
    upstream: Rc<dyn Stream<Burst<Vec<u8>>>>,
    service_name: String,
    opts: Iceoryx2PubSliceOpts,
    publisher: Option<Iceoryx2SlicePublisherPort>,
    notifier: Option<Iceoryx2NotifierPort>,
    cycles: u64,
}

enum Iceoryx2SlicePublisherPort {
    Ipc(Publisher<ipc::Service, [u8], ()>),
    Local(Publisher<local::Service, [u8], ()>),
}

impl Iceoryx2SlicePublisher {
    fn new(
        upstream: Rc<dyn Stream<Burst<Vec<u8>>>>,
        service_name: String,
        opts: Iceoryx2PubSliceOpts,
    ) -> Self {
        Self {
            upstream,
            service_name,
            opts,
            publisher: None,
            notifier: None,
            cycles: 0,
        }
    }
}

impl MutableNode for Iceoryx2SlicePublisher {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let Some(publisher) = &self.publisher else {
            return Ok(false);
        };

        self.cycles += 1;
        // Periodically update connections to handle late subscribers.
        // Slice pub/sub is often used by short-lived publishers (e.g., Python tests).
        if self.cycles.is_multiple_of(10) {
            match publisher {
                Iceoryx2SlicePublisherPort::Ipc(p) => p.update_connections()?,
                Iceoryx2SlicePublisherPort::Local(p) => p.update_connections()?,
            }
        }

        let burst = self.upstream.peek_value();
        let mut sent_any = false;
        for data in burst {
            match publisher {
                Iceoryx2SlicePublisherPort::Ipc(publisher) => {
                    let sample = publisher
                        .loan_slice_uninit(data.len())
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                    let sample = sample.write_from_slice(&data);
                    sample
                        .send()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                }
                Iceoryx2SlicePublisherPort::Local(publisher) => {
                    let sample = publisher
                        .loan_slice_uninit(data.len())
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                    let sample = sample.write_from_slice(&data);
                    sample
                        .send()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                }
            }
            sent_any = true;
        }

        if sent_any && let Some(ref n) = self.notifier {
            match n {
                Iceoryx2NotifierPort::Ipc(notifier) => {
                    notifier
                        .notify()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                }
                Iceoryx2NotifierPort::Local(notifier) => {
                    notifier
                        .notify()
                        .map_err(|e| Iceoryx2Error::TransmissionError(e.to_string()))?;
                }
            }
        }

        Ok(sent_any)
    }

    fn start(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        let signal_name = format!("{}.signal", self.service_name);
        match self.opts.variant {
            Iceoryx2ServiceVariant::Ipc => {
                let node = NodeBuilder::new()
                    .create::<ipc::Service>()
                    .map_err(|e| Iceoryx2Error::NodeCreationFailed(e.to_string()))?;
                let contract = Iceoryx2ServiceContract::new(self.opts.history_size);
                let service = node
                    .service_builder(&self.service_name.as_str().try_into().map_err(
                        |e: ServiceNameError| Iceoryx2Error::Other(anyhow::anyhow!(e.to_string())),
                    )?)
                    .publish_subscribe::<[u8]>()
                    .history_size(self.opts.history_size)
                    .subscriber_max_buffer_size(contract.subscriber_max_buffer_size)
                    .open_or_create()
                    .map_err(|e| {
                        service_open_err_with_context(
                            &self.service_name,
                            self.opts.variant,
                            contract,
                            e,
                        )
                    })?;
                let publisher = service
                    .publisher_builder()
                    .initial_max_slice_len(self.opts.initial_max_slice_len)
                    .create()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                publisher
                    .update_connections()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                self.publisher = Some(Iceoryx2SlicePublisherPort::Ipc(publisher));

                let event_service = {
                    let mut last_err: Option<String> = None;
                    let mut service = None;
                    for _ in 0..25 {
                        match node
                            .service_builder(&signal_name.as_str().try_into().map_err(
                                |e: ServiceNameError| {
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
                            &signal_name,
                            self.opts.variant,
                            Iceoryx2ServiceContract::new(0),
                            last_err.unwrap_or_else(|| "event open_or_create failed".to_string()),
                        )
                    })?
                };
                let notifier = event_service
                    .notifier_builder()
                    .create()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                self.notifier = Some(Iceoryx2NotifierPort::Ipc(notifier));
            }
            Iceoryx2ServiceVariant::Local => {
                let node = NodeBuilder::new()
                    .create::<local::Service>()
                    .map_err(|e| Iceoryx2Error::NodeCreationFailed(e.to_string()))?;
                let contract = Iceoryx2ServiceContract::new(self.opts.history_size);
                let service = node
                    .service_builder(&self.service_name.as_str().try_into().map_err(
                        |e: ServiceNameError| Iceoryx2Error::Other(anyhow::anyhow!(e.to_string())),
                    )?)
                    .publish_subscribe::<[u8]>()
                    .history_size(self.opts.history_size)
                    .subscriber_max_buffer_size(contract.subscriber_max_buffer_size)
                    .open_or_create()
                    .map_err(|e| {
                        service_open_err_with_context(
                            &self.service_name,
                            self.opts.variant,
                            contract,
                            e,
                        )
                    })?;
                let publisher = service
                    .publisher_builder()
                    .initial_max_slice_len(self.opts.initial_max_slice_len)
                    .create()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                publisher
                    .update_connections()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                self.publisher = Some(Iceoryx2SlicePublisherPort::Local(publisher));

                let event_service = {
                    let mut last_err: Option<String> = None;
                    let mut service = None;
                    for _ in 0..25 {
                        match node
                            .service_builder(&signal_name.as_str().try_into().map_err(
                                |e: ServiceNameError| {
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
                            &signal_name,
                            self.opts.variant,
                            Iceoryx2ServiceContract::new(0),
                            last_err.unwrap_or_else(|| "event open_or_create failed".to_string()),
                        )
                    })?
                };
                let notifier = event_service
                    .notifier_builder()
                    .create()
                    .map_err(|e| Iceoryx2Error::PortCreationFailed(e.to_string()))?;
                self.notifier = Some(Iceoryx2NotifierPort::Local(notifier));
            }
        }
        Ok(())
    }

    fn stop(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        self.publisher = None;
        self.notifier = None;
        Ok(())
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}
