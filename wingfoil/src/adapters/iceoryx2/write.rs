//! iceoryx2 publisher (write) implementation

use std::rc::Rc;

use crate::types::{Element, IntoNode, UpStreams};
use crate::{Burst, GraphState, MutableNode, Node, Stream};

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

enum Iceoryx2PublisherPort<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    Ipc(Publisher<ipc::Service, T, ()>),
    Local(Publisher<local::Service, T, ()>),
}

struct Iceoryx2Publisher<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    upstream: Rc<dyn Stream<Burst<T>>>,
    service_name: String,
    variant: Iceoryx2ServiceVariant,
    publisher: Option<Iceoryx2PublisherPort<T>>,
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
        Ok(sent_any)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }

    fn setup(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        Ok(())
    }

    fn start(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        match self.variant {
            Iceoryx2ServiceVariant::Ipc => {
                let node = NodeBuilder::new().create::<ipc::Service>()?;
                let service = node
                    .service_builder(&self.service_name.as_str().try_into()?)
                    .publish_subscribe::<T>()
                    .open_or_create()?;
                let publisher = service.publisher_builder().create()?;
                publisher.update_connections()?;
                self.publisher = Some(Iceoryx2PublisherPort::Ipc(publisher));
            }
            Iceoryx2ServiceVariant::Local => {
                let node = NodeBuilder::new().create::<local::Service>()?;
                let service = node
                    .service_builder(&self.service_name.as_str().try_into()?)
                    .publish_subscribe::<T>()
                    .open_or_create()?;
                let publisher = service.publisher_builder().create()?;
                publisher.update_connections()?;
                self.publisher = Some(Iceoryx2PublisherPort::Local(publisher));
            }
        }

        Ok(())
    }

    fn stop(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        self.publisher = None;
        Ok(())
    }

    fn teardown(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        Ok(())
    }
}
