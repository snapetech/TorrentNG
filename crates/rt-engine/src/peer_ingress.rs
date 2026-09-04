use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_TRACKED_PEER_INGRESS_IPS: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIngressConfig {
    pub max_global_handshakes: usize,
    pub max_handshakes_per_ip: usize,
    pub per_ip_window: Duration,
    pub handshake_timeout: Duration,
}

impl Default for PeerIngressConfig {
    fn default() -> Self {
        Self {
            max_global_handshakes: 256,
            max_handshakes_per_ip: 16,
            per_ip_window: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Default)]
pub struct PeerIngressStats {
    pub accepted: u64,
    pub rejected_global_budget: u64,
    pub rejected_ip_budget: u64,
}

#[derive(Debug)]
pub struct PeerIngressBudget {
    config: PeerIngressConfig,
    global: Arc<Semaphore>,
    per_ip: Arc<Mutex<HashMap<IpAddr, VecDeque<Instant>>>>,
    accepted: AtomicU64,
    rejected_global_budget: AtomicU64,
    rejected_ip_budget: AtomicU64,
}

#[derive(Debug)]
pub struct PeerIngressPermit {
    _global: OwnedSemaphorePermit,
    per_ip: Arc<Mutex<HashMap<IpAddr, VecDeque<Instant>>>>,
    ip: IpAddr,
    admitted_at: Instant,
}

impl PeerIngressBudget {
    pub fn new(config: PeerIngressConfig) -> Self {
        Self {
            config,
            global: Arc::new(Semaphore::new(config.max_global_handshakes.max(1))),
            per_ip: Arc::new(Mutex::new(HashMap::new())),
            accepted: AtomicU64::new(0),
            rejected_global_budget: AtomicU64::new(0),
            rejected_ip_budget: AtomicU64::new(0),
        }
    }

    pub fn config(&self) -> PeerIngressConfig {
        self.config
    }

    pub fn try_begin(
        &self,
        peer_addr: SocketAddr,
        now: Instant,
    ) -> Result<PeerIngressPermit, PeerIngressReject> {
        self.prune_ip_window(peer_addr.ip(), now);
        if !self.reserve_ip_slot(peer_addr.ip(), now) {
            self.rejected_ip_budget.fetch_add(1, Ordering::Relaxed);
            return Err(PeerIngressReject::PerIpBudget);
        }

        match self.global.clone().try_acquire_owned() {
            Ok(permit) => {
                self.accepted.fetch_add(1, Ordering::Relaxed);
                Ok(PeerIngressPermit {
                    _global: permit,
                    per_ip: Arc::clone(&self.per_ip),
                    ip: peer_addr.ip(),
                    admitted_at: now,
                })
            }
            Err(_) => {
                // The per-IP reservation is a rate-window admission record,
                // but this connection never became an admitted handshake.
                // Roll it back so a saturated global budget cannot permanently
                // poison an otherwise healthy source IP until the window ends.
                self.release_ip_slot(peer_addr.ip(), now);
                self.rejected_global_budget.fetch_add(1, Ordering::Relaxed);
                Err(PeerIngressReject::GlobalBudget)
            }
        }
    }

    pub fn stats(&self) -> PeerIngressStats {
        PeerIngressStats {
            accepted: self.accepted.load(Ordering::Relaxed),
            rejected_global_budget: self.rejected_global_budget.load(Ordering::Relaxed),
            rejected_ip_budget: self.rejected_ip_budget.load(Ordering::Relaxed),
        }
    }

    fn prune_ip_window(&self, ip: IpAddr, now: Instant) {
        let mut per_ip = self
            .per_ip
            .lock()
            .expect("peer ingress budget mutex poisoned");
        if let Some(events) = per_ip.get_mut(&ip) {
            while events
                .front()
                .copied()
                .is_some_and(|then| now.saturating_duration_since(then) > self.config.per_ip_window)
            {
                events.pop_front();
            }
            if events.is_empty() {
                per_ip.remove(&ip);
            }
        }
    }

    fn reserve_ip_slot(&self, ip: IpAddr, now: Instant) -> bool {
        let mut per_ip = self
            .per_ip
            .lock()
            .expect("peer ingress budget mutex poisoned");
        if !per_ip.contains_key(&ip) && per_ip.len() >= MAX_TRACKED_PEER_INGRESS_IPS {
            return false;
        }
        let events = per_ip.entry(ip).or_default();
        if events.len() >= self.config.max_handshakes_per_ip.max(1) {
            return false;
        }
        events.push_back(now);
        true
    }

    fn release_ip_slot(&self, ip: IpAddr, now: Instant) {
        release_ip_slot(&self.per_ip, ip, now);
    }
}

impl PeerIngressPermit {
    /// Roll back the rate-window admission when a later, process-wide
    /// connection budget rejects the same socket. Normally an admitted
    /// handshake keeps its per-IP attempt record for the configured window;
    /// this explicit cancellation is only for an attempt that never reached
    /// the handshake task.
    pub fn cancel(self) {
        release_ip_slot(&self.per_ip, self.ip, self.admitted_at);
        // Dropping self releases the global semaphore permit.
    }
}

fn release_ip_slot(
    per_ip: &Arc<Mutex<HashMap<IpAddr, VecDeque<Instant>>>>,
    ip: IpAddr,
    now: Instant,
) {
    let mut per_ip = per_ip.lock().expect("peer ingress budget mutex poisoned");
    let Some(events) = per_ip.get_mut(&ip) else {
        return;
    };
    if let Some(index) = events.iter().rposition(|event| *event == now) {
        events.remove(index);
    }
    if events.is_empty() {
        per_ip.remove(&ip);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerIngressReject {
    GlobalBudget,
    PerIpBudget,
}

impl std::fmt::Display for PeerIngressReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerIngressReject::GlobalBudget => {
                write!(f, "global inbound handshake budget exhausted")
            }
            PeerIngressReject::PerIpBudget => {
                write!(f, "per-IP inbound handshake budget exhausted")
            }
        }
    }
}

impl std::error::Error for PeerIngressReject {}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([192, 0, 2, 10], port))
    }

    #[test]
    fn global_budget_limits_unrouted_handshakes() {
        let budget = PeerIngressBudget::new(PeerIngressConfig {
            max_global_handshakes: 1,
            max_handshakes_per_ip: 10,
            per_ip_window: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(5),
        });
        let now = Instant::now();
        let permit = budget.try_begin(addr(1), now).unwrap();
        assert!(matches!(
            budget.try_begin(addr(2), now),
            Err(PeerIngressReject::GlobalBudget)
        ));
        drop(permit);
        assert!(budget.try_begin(addr(3), now).is_ok());
        assert_eq!(budget.stats().rejected_global_budget, 1);
    }

    #[test]
    fn per_ip_budget_limits_connection_storms() {
        let budget = PeerIngressBudget::new(PeerIngressConfig {
            max_global_handshakes: 100,
            max_handshakes_per_ip: 2,
            per_ip_window: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(5),
        });
        let now = Instant::now();
        let _a = budget.try_begin(addr(1), now).unwrap();
        let _b = budget.try_begin(addr(2), now).unwrap();
        assert!(matches!(
            budget.try_begin(addr(3), now),
            Err(PeerIngressReject::PerIpBudget)
        ));
        assert_eq!(budget.stats().rejected_ip_budget, 1);

        assert!(budget
            .try_begin(addr(4), now + Duration::from_secs(31))
            .is_ok());
    }

    #[test]
    fn global_rejection_does_not_consume_per_ip_slot() {
        let budget = PeerIngressBudget::new(PeerIngressConfig {
            max_global_handshakes: 1,
            max_handshakes_per_ip: 1,
            per_ip_window: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(5),
        });
        let now = Instant::now();
        let first = budget.try_begin(addr(1), now).unwrap();
        assert!(matches!(
            budget.try_begin(SocketAddr::from(([192, 0, 2, 11], 2)), now),
            Err(PeerIngressReject::GlobalBudget)
        ));
        drop(first);
        assert!(budget
            .try_begin(SocketAddr::from(([192, 0, 2, 11], 2)), now)
            .is_ok());
    }

    #[test]
    fn cancelled_admission_does_not_consume_per_ip_slot() {
        let budget = PeerIngressBudget::new(PeerIngressConfig {
            max_global_handshakes: 1,
            max_handshakes_per_ip: 1,
            per_ip_window: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(5),
        });
        let now = Instant::now();
        let permit = budget.try_begin(addr(1), now).unwrap();
        permit.cancel();
        assert!(budget.try_begin(addr(2), now).is_ok());
    }
}
