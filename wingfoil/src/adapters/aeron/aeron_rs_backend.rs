//! Pure-Rust `aeron-rs` backend for the Aeron adapter.
//!
//! Requires the `aeron-rs` feature flag.  No C++ toolchain needed.
//!
//! `aeron-rs` is less mature than the rusteron C++ FFI backend.  Prefer
//! `aeron-rusteron` for production use.
//!
//! # Connection lifecycle
//!
//! Build an [`AeronRsHandle`] with [`AeronRsHandle::connect`], then derive
//! subscribers and publishers from it.

use crate::adapters::aeron::transport::{AeronPublisherBackend, AeronSubscriberBackend};
use aeron_rs::aeron::Aeron;
use aeron_rs::context::Context;
use aeron_rs::publication::Publication;
use aeron_rs::subscription::Subscription;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Connection handle
// ---------------------------------------------------------------------------

pub struct AeronRsHandle {
    aeron: Arc<Mutex<Aeron>>,
}

impl AeronRsHandle {
    pub fn connect() -> anyhow::Result<Self> {
        let ctx = Context::new();
        let aeron = Aeron::new(ctx)?;
        Ok(Self {
            aeron: Arc::new(Mutex::new(aeron)),
        })
    }

    pub fn subscription(
        &self,
        channel: &str,
        stream_id: i32,
        timeout: Duration,
    ) -> anyhow::Result<AeronRsSubscriber> {
        let mut aeron = self.aeron.lock().unwrap();
        let id = aeron.add_subscription(channel.to_string(), stream_id)?;
        let deadline = std::time::Instant::now() + timeout;
        let sub = loop {
            if let Some(sub) = aeron.find_subscription(id)? {
                break sub;
            }
            if std::time::Instant::now() > deadline {
                anyhow::bail!("timed out waiting for subscription on {channel}:{stream_id}");
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        Ok(AeronRsSubscriber { sub })
    }

    pub fn publication(
        &self,
        channel: &str,
        stream_id: i32,
        timeout: Duration,
    ) -> anyhow::Result<AeronRsPublisher> {
        let mut aeron = self.aeron.lock().unwrap();
        let id = aeron.add_publication(channel.to_string(), stream_id)?;
        let deadline = std::time::Instant::now() + timeout;
        let pub_ = loop {
            if let Some(p) = aeron.find_publication(id)? {
                break p;
            }
            if std::time::Instant::now() > deadline {
                anyhow::bail!("timed out waiting for publication on {channel}:{stream_id}");
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        Ok(AeronRsPublisher { pub_ })
    }
}

// ---------------------------------------------------------------------------
// Subscriber
// ---------------------------------------------------------------------------

pub struct AeronRsSubscriber {
    sub: Arc<Mutex<Subscription>>,
}

impl AeronSubscriberBackend for AeronRsSubscriber {
    fn poll(&mut self, handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize> {
        let mut count = 0usize;
        let mut sub = self.sub.lock().unwrap();
        sub.poll(
            &mut |buffer, _header| {
                handler(buffer.as_slice());
                count += 1;
            },
            256,
        )?;
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// Publisher
// ---------------------------------------------------------------------------

pub struct AeronRsPublisher {
    pub_: Arc<Mutex<Publication>>,
}

impl AeronPublisherBackend for AeronRsPublisher {
    fn offer(&mut self, buffer: &[u8]) -> anyhow::Result<()> {
        let mut pub_ = self.pub_.lock().unwrap();
        let position = pub_.offer_part(buffer, 0, buffer.len())?;
        if position < 0 {
            anyhow::bail!("Aeron offer back-pressure: position={position}");
        }
        Ok(())
    }
}
