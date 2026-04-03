use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum SeedRequest {
    Register { name: String, address: String },
    Lookup { name: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum SeedResponse {
    Ok,
    Found { address: String },
    NotFound,
    Error { message: String },
}

/// Handle to a running seed node. The seed stops when this is dropped.
pub struct SeedHandle {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for SeedHandle {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Start a seed node bound to `bind_address` (e.g. `"tcp://0.0.0.0:7777"`).
///
/// The seed runs in a background thread and stops when the returned
/// [`SeedHandle`] is dropped.
pub fn start_seed(bind_address: &str) -> anyhow::Result<SeedHandle> {
    let context = zmq::Context::new();
    let socket = context.socket(zmq::REP)?;
    socket.bind(bind_address)?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let thread = std::thread::spawn(move || run_seed(socket, running_clone));

    Ok(SeedHandle {
        running,
        thread: Some(thread),
    })
}

fn run_seed(socket: zmq::Socket, running: Arc<AtomicBool>) {
    let mut registry: HashMap<String, String> = HashMap::new();

    while running.load(Ordering::Relaxed) {
        let mut items = [socket.as_poll_item(zmq::POLLIN)];
        match zmq::poll(&mut items, 10) {
            Ok(_) => {}
            Err(_) => break,
        }

        if !items[0].is_readable() {
            continue;
        }

        let bytes = match socket.recv_bytes(0) {
            Ok(b) => b,
            Err(_) => break,
        };

        let response = match bincode::deserialize::<SeedRequest>(&bytes) {
            Ok(SeedRequest::Register { name, address }) => {
                registry.insert(name, address);
                SeedResponse::Ok
            }
            Ok(SeedRequest::Lookup { name }) => match registry.get(&name) {
                Some(addr) => SeedResponse::Found {
                    address: addr.clone(),
                },
                None => SeedResponse::NotFound,
            },
            Err(e) => SeedResponse::Error {
                message: e.to_string(),
            },
        };

        let reply = match bincode::serialize(&response) {
            Ok(b) => b,
            Err(_) => break,
        };
        if socket.send(reply, 0).is_err() {
            break;
        }
    }
}

fn req_socket_for(seed: &str) -> anyhow::Result<zmq::Socket> {
    let context = zmq::Context::new();
    let socket = context.socket(zmq::REQ)?;
    socket.set_linger(0)?;
    socket.set_rcvtimeo(3000)?;
    socket.set_sndtimeo(3000)?;
    socket.connect(seed)?;
    Ok(socket)
}

/// Register `name` → `address` with all seeds. Returns `Ok` on first success,
/// `Err` if no seed could be reached.
pub(crate) fn register_with_seeds(
    name: &str,
    address: &str,
    seeds: &[String],
) -> anyhow::Result<()> {
    let mut last_err = anyhow::anyhow!("no seeds provided");

    for seed in seeds {
        let socket = match req_socket_for(seed) {
            Ok(s) => s,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        let req = SeedRequest::Register {
            name: name.to_string(),
            address: address.to_string(),
        };
        let bytes = bincode::serialize(&req)?;
        if socket.send(bytes, 0).is_err() {
            continue;
        }
        match socket.recv_bytes(0) {
            Ok(reply) => match bincode::deserialize::<SeedResponse>(&reply) {
                Ok(SeedResponse::Ok) => return Ok(()),
                Ok(other) => {
                    last_err = anyhow::anyhow!("unexpected response from seed {seed}: {other:?}");
                }
                Err(e) => {
                    last_err = anyhow::anyhow!("failed to deserialize seed response: {e}");
                }
            },
            Err(e) => {
                last_err = anyhow::anyhow!("no response from seed {seed}: {e}");
            }
        }
    }

    Err(last_err)
}

/// Query seeds for `name`. Returns the resolved address on first successful
/// lookup, or `Err` if no seed can resolve it.
pub(crate) fn query_seeds(name: &str, seeds: &[&str]) -> anyhow::Result<String> {
    let mut last_err = anyhow::anyhow!("no seeds provided");

    for &seed in seeds {
        let socket = match req_socket_for(seed) {
            Ok(s) => s,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        let req = SeedRequest::Lookup {
            name: name.to_string(),
        };
        let bytes = bincode::serialize(&req)?;
        if socket.send(bytes, 0).is_err() {
            continue;
        }
        match socket.recv_bytes(0) {
            Ok(reply) => match bincode::deserialize::<SeedResponse>(&reply) {
                Ok(SeedResponse::Found { address }) => return Ok(address),
                Ok(SeedResponse::NotFound) => {
                    last_err =
                        anyhow::anyhow!("no publisher named '{name}' registered with seed {seed}");
                }
                Ok(other) => {
                    last_err = anyhow::anyhow!("unexpected response from seed {seed}: {other:?}");
                }
                Err(e) => {
                    last_err = anyhow::anyhow!("failed to deserialize seed response: {e}");
                }
            },
            Err(e) => {
                last_err = anyhow::anyhow!("no response from seed {seed}: {e}");
            }
        }
    }

    Err(last_err)
}
