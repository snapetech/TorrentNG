use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_TRACKED_PEER_INGRESS_IPS: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IngressReservation {
    id: u64,
    admitted_at: Instant,
}

type PerIpReservations = HashMap<IpAddr, VecDeque<IngressReservation>>;

fn lock_reservations(reservations: &Mutex<PerIpReservations>) -> MutexGuard<'_, PerIpReservations> {
    match reservations.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            // These reservations enforce the per-source admission window.
            // Preserve them during recovery so a panic cannot reset the cap.
            let guard = poisoned.into_inner();
            reservations.clear_poison();
            guard
        }
    }
}

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerIngressStats {
    /// Handshake slots admitted before peer-wire validation completes.
    pub accepted: u64,
    pub rejected_global_budget: u64,
    pub rejected_ip_budget: u64,
    pub rejected_peer_connection_budget: u64,
    pub handshake_read_errors: u64,
    pub handshake_timeouts: u64,
    pub malformed_handshakes: u64,
}

#[derive(Debug)]
pub struct PeerIngressBudget {
    config: PeerIngressConfig,
    global: Arc<Semaphore>,
    per_ip: Arc<Mutex<PerIpReservations>>,
    next_reservation_id: AtomicU64,
    accepted: AtomicU64,
    rejected_global_budget: AtomicU64,
    rejected_ip_budget: AtomicU64,
    rejected_peer_connection_budget: AtomicU64,
    handshake_read_errors: AtomicU64,
    handshake_timeouts: AtomicU64,
    malformed_handshakes: AtomicU64,
}

#[derive(Debug)]
pub struct PeerIngressPermit {
    _global: OwnedSemaphorePermit,
    per_ip: Arc<Mutex<PerIpReservations>>,
    ip: IpAddr,
    reservation_id: u64,
}

impl PeerIngressBudget {
    pub fn new(config: PeerIngressConfig) -> Self {
        Self {
            config,
            global: Arc::new(Semaphore::new(config.max_global_handshakes.max(1))),
            per_ip: Arc::new(Mutex::new(HashMap::new())),
            next_reservation_id: AtomicU64::new(1),
            accepted: AtomicU64::new(0),
            rejected_global_budget: AtomicU64::new(0),
            rejected_ip_budget: AtomicU64::new(0),
            rejected_peer_connection_budget: AtomicU64::new(0),
            handshake_read_errors: AtomicU64::new(0),
            handshake_timeouts: AtomicU64::new(0),
            malformed_handshakes: AtomicU64::new(0),
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
        let reservation_id = self.next_reservation_id.fetch_add(1, Ordering::Relaxed);
        if !self.reserve_ip_slot(peer_addr.ip(), now, reservation_id) {
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
                    reservation_id,
                })
            }
            Err(_) => {
                // The per-IP reservation is a rate-window admission record,
                // but this connection never became an admitted handshake.
                // Roll it back so a saturated global budget cannot permanently
                // poison an otherwise healthy source IP until the window ends.
                self.release_ip_slot(peer_addr.ip(), reservation_id);
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
            rejected_peer_connection_budget: self
                .rejected_peer_connection_budget
                .load(Ordering::Relaxed),
            handshake_read_errors: self.handshake_read_errors.load(Ordering::Relaxed),
            handshake_timeouts: self.handshake_timeouts.load(Ordering::Relaxed),
            malformed_handshakes: self.malformed_handshakes.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn record_peer_connection_budget_rejection(&self) {
        self.rejected_peer_connection_budget
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_handshake_timeout(&self) {
        self.handshake_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_handshake_read_error(&self) {
        self.handshake_read_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_malformed_handshake(&self) {
        self.malformed_handshakes.fetch_add(1, Ordering::Relaxed);
    }

    fn reserve_ip_slot(&self, ip: IpAddr, now: Instant, reservation_id: u64) -> bool {
        let mut per_ip = lock_reservations(&self.per_ip);

        // Prune the requested IP while holding the same lock used for the
        // admission check. This avoids a second lock acquisition and keeps
        // the check-and-reserve operation atomic under reconnect bursts.
        if let Some(events) = per_ip.get_mut(&ip) {
            while events.front().copied().is_some_and(|event| {
                now.saturating_duration_since(event.admitted_at) >= self.config.per_ip_window
            }) {
                events.pop_front();
            }
            if events.is_empty() {
                per_ip.remove(&ip);
            }
        }

        if !per_ip.contains_key(&ip) && per_ip.len() >= MAX_TRACKED_PEER_INGRESS_IPS {
            // An IP that never reconnects cannot trigger the normal per-IP
            // pruning path. Without sweeping here, the bounded map becomes
            // permanently saturated after one attempt from enough unique
            // addresses and every new peer is rejected forever.
            let window = self.config.per_ip_window;
            per_ip.retain(|_, events| {
                while events
                    .front()
                    .copied()
                    .is_some_and(|event| now.saturating_duration_since(event.admitted_at) >= window)
                {
                    events.pop_front();
                }
                !events.is_empty()
            });
            if per_ip.len() >= MAX_TRACKED_PEER_INGRESS_IPS {
                return false;
            }
        }
        let events = per_ip.entry(ip).or_default();
        if events.len() >= self.config.max_handshakes_per_ip.max(1) {
            return false;
        }
        events.push_back(IngressReservation {
            id: reservation_id,
            admitted_at: now,
        });
        true
    }

    fn release_ip_slot(&self, ip: IpAddr, reservation_id: u64) {
        release_ip_slot(&self.per_ip, ip, reservation_id);
    }
}

impl PeerIngressPermit {
    /// Roll back the rate-window admission when a later, process-wide
    /// connection budget rejects the same socket. Normally an admitted
    /// handshake keeps its per-IP attempt record for the configured window;
    /// this explicit cancellation is only for an attempt that never reached
    /// the handshake task.
    pub fn cancel(self) {
        release_ip_slot(&self.per_ip, self.ip, self.reservation_id);
        // Dropping self releases the global semaphore permit.
    }
}

fn release_ip_slot(per_ip: &Arc<Mutex<PerIpReservations>>, ip: IpAddr, reservation_id: u64) {
    let mut per_ip = lock_reservations(per_ip);
    let Some(events) = per_ip.get_mut(&ip) else {
        return;
    };
    if let Some(index) = events.iter().position(|event| event.id == reservation_id) {
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

    #[test]
    fn release_uses_reservation_identity_when_timestamps_collide() {
        let budget = PeerIngressBudget::new(PeerIngressConfig {
            max_global_handshakes: 10,
            max_handshakes_per_ip: 2,
            per_ip_window: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(5),
        });
        let now = Instant::now();
        let first = budget.try_begin(addr(1), now).unwrap();
        let second = budget.try_begin(addr(2), now).unwrap();

        first.cancel();
        let per_ip = lock_reservations(&budget.per_ip);
        let reservations = per_ip.get(&addr(1).ip()).unwrap();
        assert_eq!(reservations.len(), 1);
        assert_eq!(reservations[0].id, second.reservation_id);
        drop(per_ip);
        drop(second);
    }

    #[test]
    fn per_ip_slot_expires_at_window_boundary() {
        let window = Duration::from_secs(30);
        let budget = PeerIngressBudget::new(PeerIngressConfig {
            max_global_handshakes: 10,
            max_handshakes_per_ip: 1,
            per_ip_window: window,
            handshake_timeout: Duration::from_secs(5),
        });
        let now = Instant::now();
        let permit = budget.try_begin(addr(1), now).unwrap();
        drop(permit);

        assert!(budget.try_begin(addr(2), now + window).is_ok());
    }

    #[test]
    fn stale_unique_ips_are_reclaimed_for_new_sources() {
        let budget = PeerIngressBudget::new(PeerIngressConfig {
            max_global_handshakes: MAX_TRACKED_PEER_INGRESS_IPS + 1,
            max_handshakes_per_ip: 1,
            per_ip_window: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(5),
        });
        let now = Instant::now();
        for raw_ip in 0..MAX_TRACKED_PEER_INGRESS_IPS {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from(raw_ip as u32));
            let permit = budget.try_begin(SocketAddr::new(ip, 6881), now).unwrap();
            drop(permit);
        }

        let new_ip = IpAddr::V4(std::net::Ipv4Addr::from(
            (MAX_TRACKED_PEER_INGRESS_IPS + 1) as u32,
        ));
        assert!(budget
            .try_begin(SocketAddr::new(new_ip, 6881), now + Duration::from_secs(31))
            .is_ok());
    }

    #[test]
    fn poisoned_per_ip_state_preserves_admission_reservations() {
        let budget = PeerIngressBudget::new(PeerIngressConfig {
            max_global_handshakes: 10,
            max_handshakes_per_ip: 1,
            per_ip_window: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(5),
        });
        let now = Instant::now();
        let first = budget.try_begin(addr(1), now).unwrap();

        let poisoner = Arc::clone(&budget.per_ip);
        assert!(std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the per-IP admission map");
        })
        .join()
        .is_err());
        assert!(budget.per_ip.is_poisoned());

        assert!(matches!(
            budget.try_begin(addr(2), now),
            Err(PeerIngressReject::PerIpBudget)
        ));
        assert!(!budget.per_ip.is_poisoned());
        drop(first);
        assert!(budget
            .try_begin(addr(3), now + Duration::from_secs(31))
            .is_ok());
    }

    #[test]
    fn handshake_rejection_counters_are_snapshotted() {
        let budget = PeerIngressBudget::new(PeerIngressConfig::default());
        budget.record_peer_connection_budget_rejection();
        budget.record_handshake_read_error();
        budget.record_handshake_timeout();
        budget.record_malformed_handshake();

        let stats = budget.stats();
        assert_eq!(stats.rejected_peer_connection_budget, 1);
        assert_eq!(stats.handshake_read_errors, 1);
        assert_eq!(stats.handshake_timeouts, 1);
        assert_eq!(stats.malformed_handshakes, 1);
    }
}
