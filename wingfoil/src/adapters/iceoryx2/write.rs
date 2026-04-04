//! iceoryx2 publisher (write) implementation

use std::rc::Rc;

use crate::types::{Element, IntoNode, UpStreams};
use crate::{Burst, GraphState, MutableNode, Node, Stream};

use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::update_connections::UpdateConnections;
use iceoryx2::prelude::*;

use super::Iceoryx2ServiceVariant;

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
    iceoryx2_pub_with(upstream, service_name, Iceoryx2ServiceVariant::Ipc)
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
    Iceoryx2Publisher::new(upstream, service_name.to_string(), variant).into_node()
}

/// Publish a stream of byte vectors to an iceoryx2 slice service.
pub fn iceoryx2_pub_slice(
    upstream: Rc<dyn Stream<Burst<Vec<u8>>>>,
    service_name: &str,
) -> Rc<dyn Node> {
    iceoryx2_pub_slice_with(upstream, service_name, Iceoryx2ServiceVariant::Ipc)
}

/// Publish a stream of byte vectors to an iceoryx2 slice service with a variant.
pub fn iceoryx2_pub_slice_with(
    upstream: Rc<dyn Stream<Burst<Vec<u8>>>>,
    service_name: &str,
    variant: Iceoryx2ServiceVariant,
) -> Rc<dyn Node> {
    Iceoryx2SlicePublisher::new(upstream, service_name.to_string(), variant).into_node()
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
    variant: Iceoryx2ServiceVariant,
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
        variant: Iceoryx2ServiceVariant,
    ) -> Self {
        Self {
            upstream,
            service_name,
            variant,
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
        if self.cycles.is_multiple_of(100) {
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
                    let sample = publisher.loan_uninit()?;
                    let sample = sample.write_payload(data);
                    sample.send()?;
                }
                Iceoryx2PublisherPort::Local(publisher) => {
                    let sample = publisher.loan_uninit()?;
                    let sample = sample.write_payload(data);
                    sample.send()?;
                }
            }
            sent_any = true;
        }

        if sent_any {
            if let Some(ref n) = self.notifier {
                match n {
                    Iceoryx2NotifierPort::Ipc(notifier) => {
                        notifier.notify()?;
                    }
                    Iceoryx2NotifierPort::Local(notifier) => {
                        notifier.notify()?;
                    }
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

        match self.variant {
            Iceoryx2ServiceVariant::Ipc => {
                let node = NodeBuilder::new().create::<ipc::Service>()?;
                let service = node
                    .service_builder(&self.service_name.as_str().try_into()?)
                    .publish_subscribe::<T>()
                    .history_size(5)
                    .open_or_create()?;
                let publisher = service.publisher_builder().create()?;
                publisher.update_connections()?;
                self.publisher = Some(Iceoryx2PublisherPort::Ipc(publisher));

                // Always create notifier just in case subscriber uses Signaled mode
                let event_service = node
                    .service_builder(&signal_name.as_str().try_into()?)
                    .event()
                    .open_or_create()?;
                let notifier = event_service.notifier_builder().create()?;
                self.notifier = Some(Iceoryx2NotifierPort::Ipc(notifier));
            }
            Iceoryx2ServiceVariant::Local => {
                let node = NodeBuilder::new().create::<local::Service>()?;
                let service = node
                    .service_builder(&self.service_name.as_str().try_into()?)
                    .publish_subscribe::<T>()
                    .history_size(5)
                    .open_or_create()?;
                let publisher = service.publisher_builder().create()?;
                publisher.update_connections()?;
                self.publisher = Some(Iceoryx2PublisherPort::Local(publisher));

                let event_service = node
                    .service_builder(&signal_name.as_str().try_into()?)
                    .event()
                    .open_or_create()?;
                let notifier = event_service.notifier_builder().create()?;
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
    variant: Iceoryx2ServiceVariant,
    publisher: Option<Iceoryx2SlicePublisherPort>,
    notifier: Option<Iceoryx2NotifierPort>,
}

enum Iceoryx2SlicePublisherPort {
    Ipc(Publisher<ipc::Service, [u8], ()>),
    Local(Publisher<local::Service, [u8], ()>),
}

impl Iceoryx2SlicePublisher {
    fn new(
        upstream: Rc<dyn Stream<Burst<Vec<u8>>>>,
        service_name: String,
        variant: Iceoryx2ServiceVariant,
    ) -> Self {
        Self {
            upstream,
            service_name,
            variant,
            publisher: None,
            notifier: None,
        }
    }
}

impl MutableNode for Iceoryx2SlicePublisher {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let Some(publisher) = &self.publisher else {
            return Ok(false);
        };

        let burst = self.upstream.peek_value();
        let mut sent_any = false;
        for data in burst {
            match publisher {
                Iceoryx2SlicePublisherPort::Ipc(publisher) => {
                    let sample = publisher.loan_slice_uninit(data.len())?;
                    let sample = sample.write_from_slice(&data);
                    sample.send()?;
                }
                Iceoryx2SlicePublisherPort::Local(publisher) => {
                    let sample = publisher.loan_slice_uninit(data.len())?;
                    let sample = sample.write_from_slice(&data);
                    sample.send()?;
                }
            }
            sent_any = true;
        }

        if sent_any {
            if let Some(ref n) = self.notifier {
                match n {
                    Iceoryx2NotifierPort::Ipc(notifier) => {
                        notifier.notify()?;
                    }
                    Iceoryx2NotifierPort::Local(notifier) => {
                        notifier.notify()?;
                    }
                }
            }
        }

        Ok(sent_any)
    }

    fn start(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        let signal_name = format!("{}.signal", self.service_name);
        match self.variant {
            Iceoryx2ServiceVariant::Ipc => {
                let node = NodeBuilder::new().create::<ipc::Service>()?;
                let service = node
                    .service_builder(&self.service_name.as_str().try_into()?)
                    .publish_subscribe::<[u8]>()
                    .history_size(5)
                    .open_or_create()?;
                let publisher = service
                    .publisher_builder()
                    .initial_max_slice_len(128 * 1024)
                    .create()?;
                self.publisher = Some(Iceoryx2SlicePublisherPort::Ipc(publisher));

                let event_service = node
                    .service_builder(&signal_name.as_str().try_into()?)
                    .event()
                    .open_or_create()?;
                let notifier = event_service.notifier_builder().create()?;
                self.notifier = Some(Iceoryx2NotifierPort::Ipc(notifier));
            }
            Iceoryx2ServiceVariant::Local => {
                let node = NodeBuilder::new().create::<local::Service>()?;
                let service = node
                    .service_builder(&self.service_name.as_str().try_into()?)
                    .publish_subscribe::<[u8]>()
                    .history_size(5)
                    .open_or_create()?;
                let publisher = service
                    .publisher_builder()
                    .initial_max_slice_len(128 * 1024)
                    .create()?;
                self.publisher = Some(Iceoryx2SlicePublisherPort::Local(publisher));

                let event_service = node
                    .service_builder(&signal_name.as_str().try_into()?)
                    .event()
                    .open_or_create()?;
                let notifier = event_service.notifier_builder().create()?;
                self.notifier = Some(Iceoryx2NotifierPort::Local(notifier));
            }
        }
        Ok(())
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}
