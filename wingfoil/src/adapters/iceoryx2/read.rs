//! iceoryx2 subscriber (read) implementation

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

use super::{Iceoryx2Mode, Iceoryx2ServiceVariant, Iceoryx2SubOpts};

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
    subscriber: Option<Iceoryx2SubscriberPort<T>>,
    // Threaded mode state
    receiver: Option<ChannelReceiver<T>>,
    running: Arc<AtomicBool>,
    // Common state
    value: Burst<T>,
}

impl<T> Iceoryx2ReceiverStream<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    fn new(service_name: String, opts: Iceoryx2SubOpts) -> Self {
        Self {
            service_name,
            opts,
            subscriber: None,
            receiver: None,
            running: Arc::new(AtomicBool::new(false)),
            value: Burst::default(),
        }
    }
}

impl<T> crate::MutableNode for Iceoryx2ReceiverStream<T>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    fn cycle(&mut self, _state: &mut crate::GraphState) -> anyhow::Result<bool> {
        self.value.clear();

        match self.opts.mode {
            Iceoryx2Mode::Spin => {
                let Some(subscriber) = &self.subscriber else {
                    return Ok(false);
                };

                match subscriber {
                    Iceoryx2SubscriberPort::Ipc(subscriber) => {
                        while let Ok(Some(sample)) = subscriber.receive() {
                            self.value.push(*sample);
                            drop(sample);
                        }
                    }
                    Iceoryx2SubscriberPort::Local(subscriber) => {
                        while let Ok(Some(sample)) = subscriber.receive() {
                            self.value.push(*sample);
                            drop(sample);
                        }
                    }
                }
            }
            Iceoryx2Mode::Threaded => {
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
                // Continuously poll in realtime graphs.
                state.always_callback();

                match self.opts.variant {
                    Iceoryx2ServiceVariant::Ipc => {
                        let node = NodeBuilder::new().create::<ipc::Service>()?;
                        let service = node
                            .service_builder(&self.service_name.as_str().try_into()?)
                            .publish_subscribe::<T>()
                            .open_or_create()?;
                        let subscriber = service.subscriber_builder().create()?;
                        subscriber.update_connections()?;
                        self.subscriber = Some(Iceoryx2SubscriberPort::Ipc(subscriber));
                    }
                    Iceoryx2ServiceVariant::Local => {
                        let node = NodeBuilder::new().create::<local::Service>()?;
                        let service = node
                            .service_builder(&self.service_name.as_str().try_into()?)
                            .publish_subscribe::<T>()
                            .open_or_create()?;
                        let subscriber = service.subscriber_builder().create()?;
                        subscriber.update_connections()?;
                        self.subscriber = Some(Iceoryx2SubscriberPort::Local(subscriber));
                    }
                }
            }
            Iceoryx2Mode::Threaded => {
                // Continuously poll the channel in realtime graphs.
                state.always_callback();

                let (sender, receiver) = channel_pair(None);
                self.receiver = Some(receiver);
                self.running.store(true, Ordering::SeqCst);

                let service_name = self.service_name.clone();
                let variant = self.opts.variant;
                let running = self.running.clone();

                thread::spawn(move || {
                    if let Err(e) =
                        run_subscriber_thread::<T>(service_name, sender, variant, running)
                    {
                        log::error!("iceoryx2 subscriber thread error: {:?}", e);
                    }
                });
            }
        }

        Ok(())
    }

    fn stop(&mut self, _state: &mut crate::GraphState) -> anyhow::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        self.subscriber = None;
        self.receiver = None;
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

fn run_subscriber_thread<T>(
    service_name: String,
    channel_sender: ChannelSender<T>,
    variant: Iceoryx2ServiceVariant,
    running: Arc<AtomicBool>,
) -> anyhow::Result<()>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    match variant {
        Iceoryx2ServiceVariant::Ipc => {
            run_subscriber_thread_service::<ipc::Service, T>(service_name, channel_sender, running)
        }
        Iceoryx2ServiceVariant::Local => run_subscriber_thread_service::<local::Service, T>(
            service_name,
            channel_sender,
            running,
        ),
    }
}

fn run_subscriber_thread_service<S, T>(
    service_name: String,
    channel_sender: ChannelSender<T>,
    running: Arc<AtomicBool>,
) -> anyhow::Result<()>
where
    S: iceoryx2::service::Service,
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static,
{
    let node = NodeBuilder::new().create::<S>()?;

    let service = node
        .service_builder(&service_name.as_str().try_into()?)
        .publish_subscribe::<T>()
        .open_or_create()?;

    let subscriber = service.subscriber_builder().create()?;
    subscriber.update_connections()?;

    while running.load(Ordering::SeqCst) {
        let mut received = false;
        // Receive all available samples
        while let Ok(Some(sample)) = subscriber.receive() {
            let data = *sample;
            let _ = channel_sender.send_message(Message::RealtimeValue(data));
            drop(sample);
            received = true;
        }

        if !received {
            // Tighter spin/sleep loop than 1ms, but still avoiding 100% CPU if not needed.
            // Using a very small sleep to yield.
            thread::sleep(std::time::Duration::from_micros(10));
        }
    }

    let _ = channel_sender.send_message(Message::EndOfStream);
    Ok(())
}
