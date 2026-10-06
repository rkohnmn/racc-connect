use crate::ImpairmentProfile;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Statistics for a real loopback UDP impairment proxy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProxyStats {
    /// Source datagrams received from the sender.
    pub received: u64,
    /// Datagrams successfully written to the viewer socket.
    pub forwarded: u64,
    /// Forwarding attempts rejected by the local socket.
    pub send_errors: u64,
    /// Datagrams dropped by the seeded loss profile.
    pub lost: u64,
    /// Additional copies forwarded.
    pub duplicated: u64,
    /// Datagram pairs forwarded in reverse order.
    pub reordered_pairs: u64,
}

/// UDP relay on loopback with deterministic seeded loss, duplication and reordering.
pub struct LoopbackUdpProxy {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    received: Arc<AtomicU64>,
    forwarded: Arc<AtomicU64>,
    send_errors: Arc<AtomicU64>,
    lost: Arc<AtomicU64>,
    duplicated: Arc<AtomicU64>,
    reordered: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl LoopbackUdpProxy {
    /// Starts forwarding from a loopback address to a viewer UDP endpoint.
    pub fn bind(
        target: SocketAddr,
        profile: ImpairmentProfile,
        seed: u64,
    ) -> std::io::Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.set_read_timeout(Some(Duration::from_millis(5)))?;
        let address = socket.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let received = Arc::new(AtomicU64::new(0));
        let forwarded = Arc::new(AtomicU64::new(0));
        let send_errors = Arc::new(AtomicU64::new(0));
        let lost = Arc::new(AtomicU64::new(0));
        let duplicated = Arc::new(AtomicU64::new(0));
        let reordered = Arc::new(AtomicU64::new(0));
        let thread_stop = Arc::clone(&stop);
        let thread_received = Arc::clone(&received);
        let thread_forwarded = Arc::clone(&forwarded);
        let thread_send_errors = Arc::clone(&send_errors);
        let thread_lost = Arc::clone(&lost);
        let thread_duplicated = Arc::clone(&duplicated);
        let thread_reordered = Arc::clone(&reordered);
        let worker = thread::Builder::new()
            .name("racc-loopback-proxy".to_owned())
            .spawn(move || {
                proxy_loop(
                    socket,
                    ProxyContext {
                        target,
                        profile,
                        seed,
                        stop: thread_stop,
                        received: thread_received,
                        forwarded: thread_forwarded,
                        send_errors: thread_send_errors,
                        lost: thread_lost,
                        duplicated: thread_duplicated,
                        reordered: thread_reordered,
                    },
                );
            })?;
        Ok(Self {
            address,
            stop,
            received,
            forwarded,
            send_errors,
            lost,
            duplicated,
            reordered,
            worker: Some(worker),
        })
    }

    /// The loopback address to which a video sender should send.
    pub const fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Current real-socket proxy counters.
    pub fn stats(&self) -> ProxyStats {
        ProxyStats {
            received: self.received.load(Ordering::Relaxed),
            forwarded: self.forwarded.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            lost: self.lost.load(Ordering::Relaxed),
            duplicated: self.duplicated.load(Ordering::Relaxed),
            reordered_pairs: self.reordered.load(Ordering::Relaxed),
        }
    }

    /// Stops and joins the proxy worker.
    pub fn close(&mut self) -> std::thread::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join()
        } else {
            Ok(())
        }
    }
}

impl Drop for LoopbackUdpProxy {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

struct ProxyContext {
    target: SocketAddr,
    profile: ImpairmentProfile,
    seed: u64,
    stop: Arc<AtomicBool>,
    received: Arc<AtomicU64>,
    forwarded: Arc<AtomicU64>,
    send_errors: Arc<AtomicU64>,
    lost: Arc<AtomicU64>,
    duplicated: Arc<AtomicU64>,
    reordered: Arc<AtomicU64>,
}

fn proxy_loop(socket: UdpSocket, context: ProxyContext) {
    let ProxyContext {
        target,
        profile,
        seed,
        stop,
        received,
        forwarded,
        send_errors,
        lost,
        duplicated,
        reordered,
    } = context;
    let mut rng = ProxyRng::new(seed);
    let mut pending: Option<Vec<u8>> = None;
    let mut buffer = [0u8; 2048];
    let send = |packet: &[u8]| {
        record_send_result(
            socket.send_to(packet, target),
            packet.len(),
            &forwarded,
            &send_errors,
        )
    };
    while !stop.load(Ordering::Acquire) {
        match socket.recv_from(&mut buffer) {
            Ok((length, _source)) => {
                received.fetch_add(1, Ordering::Relaxed);
                let Some(packet) = buffer.get(..length) else {
                    continue;
                };
                let bad = rng.probability(profile.bad_state_loss_ppm);
                if bad || rng.probability(profile.independent_loss_ppm) {
                    lost.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let packet = packet.to_vec();
                if rng.probability(profile.reorder_probability_ppm) {
                    if let Some(previous) = pending.take() {
                        let first_ok = send(&packet);
                        let second_ok = send(&previous);
                        if first_ok && second_ok {
                            reordered.fetch_add(1, Ordering::Relaxed);
                        }
                    } else {
                        pending = Some(packet);
                    }
                    continue;
                }
                if let Some(previous) = pending.take() {
                    let first_ok = send(packet.as_slice());
                    let second_ok = send(previous.as_slice());
                    if first_ok && second_ok {
                        reordered.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    let _ = send(packet.as_slice());
                }
                if rng.probability(profile.duplicate_probability_ppm) && send(packet.as_slice()) {
                    duplicated.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                if let Some(previous) = pending.take() {
                    let _ = send(&previous);
                }
            }
            Err(_) => break,
        }
    }
    if let Some(previous) = pending {
        let _ = send(&previous);
    }
}

fn record_send_result(
    result: io::Result<usize>,
    expected_len: usize,
    forwarded: &AtomicU64,
    send_errors: &AtomicU64,
) -> bool {
    match result {
        Ok(sent) if sent == expected_len => {
            forwarded.fetch_add(1, Ordering::Relaxed);
            true
        }
        Ok(_) | Err(_) => {
            send_errors.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_counters_distinguish_successful_writes_from_send_errors() {
        let forwarded = AtomicU64::new(0);
        let send_errors = AtomicU64::new(0);
        assert!(record_send_result(Ok(5), 5, &forwarded, &send_errors));
        assert!(!record_send_result(
            Err(io::Error::from(io::ErrorKind::WouldBlock)),
            5,
            &forwarded,
            &send_errors,
        ));
        assert!(!record_send_result(Ok(4), 5, &forwarded, &send_errors));
        assert_eq!(forwarded.load(Ordering::Relaxed), 1);
        assert_eq!(send_errors.load(Ordering::Relaxed), 2);
    }
}
#[derive(Clone, Debug)]
struct ProxyRng(u64);

impl ProxyRng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0xd1b5_4a32_d192_ed03
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn probability(&mut self, probability_ppm: u32) -> bool {
        self.next() % 1_000_000 < u64::from(probability_ppm.min(1_000_000))
    }
}
