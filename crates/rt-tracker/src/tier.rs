use crate::state::TrackerState;

/// A single tier from the announce-list (BEP 12).
///
/// Within a tier, trackers are tried in order. On success, the successful
/// tracker moves to the front of the tier. Failure falls through to the next.
#[derive(Debug)]
pub struct Tier {
    pub trackers: Vec<TrackerState>,
    /// Index of the current active tracker within this tier.
    active: usize,
}

impl Tier {
    pub fn new(urls: Vec<String>) -> Self {
        Tier {
            trackers: urls.into_iter().map(TrackerState::new).collect(),
            active: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.trackers.is_empty()
    }

    pub fn active_tracker(&self) -> Option<&TrackerState> {
        self.trackers.get(self.active)
    }

    pub fn active_tracker_mut(&mut self) -> Option<&mut TrackerState> {
        self.trackers.get_mut(self.active)
    }

    /// BEP 12: on success, rotate the successful tracker to the front of the tier.
    pub fn promote_active(&mut self) {
        if self.active > 0 && self.active < self.trackers.len() {
            self.trackers.swap(0, self.active);
            self.active = 0;
        }
    }

    /// Advance to the next tracker in the tier (on failure).
    pub fn advance(&mut self) {
        if !self.trackers.is_empty() {
            self.active = (self.active + 1) % self.trackers.len();
        }
    }
}

/// All announce tiers for a torrent (BEP 12).
///
/// Tiers are tried in order. The first tier to succeed is the active tier.
/// For private torrents, tier switching has additional constraints (BEP 27).
#[derive(Debug)]
pub struct TierSet {
    pub tiers: Vec<Tier>,
    active_tier: usize,
}

impl TierSet {
    pub fn new(tier_urls: Vec<Vec<String>>) -> Self {
        TierSet {
            tiers: tier_urls.into_iter().map(Tier::new).collect(),
            active_tier: 0,
        }
    }

    /// Build a TierSet from a single announce URL (no announce-list).
    pub fn single(url: impl Into<String>) -> Self {
        TierSet {
            tiers: vec![Tier::new(vec![url.into()])],
            active_tier: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tiers.is_empty()
    }

    pub fn active_tier(&self) -> Option<&Tier> {
        self.tiers.get(self.active_tier)
    }

    pub fn active_tier_mut(&mut self) -> Option<&mut Tier> {
        self.tiers.get_mut(self.active_tier)
    }

    pub fn active_tier_index(&self) -> usize {
        self.active_tier
    }

    /// All tracker URLs in announcement order.
    pub fn all_tracker_urls(&self) -> Vec<&str> {
        self.tiers
            .iter()
            .flat_map(|t| t.trackers.iter().map(|ts| ts.url.as_str()))
            .collect()
    }

    /// Advance to the next tracker in the current tier, or the next tier.
    pub fn advance_on_failure(&mut self) {
        let n_tiers = self.tiers.len();
        let tier_len = self
            .tiers
            .get(self.active_tier)
            .map(|t| t.trackers.len().max(1))
            .unwrap_or(1);
        let current_active = self
            .tiers
            .get(self.active_tier)
            .map(|t| t.active)
            .unwrap_or(0);
        let next = (current_active + 1) % tier_len;
        if next == 0 && n_tiers > 1 {
            self.active_tier = (self.active_tier + 1) % n_tiers;
        } else if let Some(tier) = self.tiers.get_mut(self.active_tier) {
            tier.active = next;
        }
    }

    /// On success, promote this tracker to the front of its tier (BEP 12).
    pub fn on_success(&mut self) {
        if let Some(tier) = self.tiers.get_mut(self.active_tier) {
            tier.promote_active();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_tracker_tier() {
        let ts = TierSet::single("http://tracker.example.com/announce");
        assert_eq!(ts.tiers.len(), 1);
        assert_eq!(ts.active_tier().unwrap().trackers.len(), 1);
    }

    #[test]
    fn multi_tier_setup() {
        let ts = TierSet::new(vec![
            vec!["http://t1.example.com/announce".into()],
            vec!["http://t2.example.com/announce".into()],
        ]);
        assert_eq!(ts.tiers.len(), 2);
        assert_eq!(ts.active_tier_index(), 0);
    }

    #[test]
    fn promote_active_moves_to_front() {
        let mut tier = Tier::new(vec![
            "http://a.example.com/announce".into(),
            "http://b.example.com/announce".into(),
        ]);
        tier.active = 1;
        tier.promote_active();
        assert_eq!(tier.active, 0);
        assert!(tier.trackers[0].url.contains("//b."));
    }

    #[test]
    fn advance_wraps_within_tier() {
        let mut tier = Tier::new(vec![
            "http://a.example.com/announce".into(),
            "http://b.example.com/announce".into(),
        ]);
        assert_eq!(tier.active, 0);
        tier.advance();
        assert_eq!(tier.active, 1);
        tier.advance();
        assert_eq!(tier.active, 0); // wrapped
    }

    #[test]
    fn all_tracker_urls_covers_all_tiers() {
        let ts = TierSet::new(vec![
            vec!["http://t1.example.com/announce".into()],
            vec![
                "http://t2.example.com/announce".into(),
                "http://t3.example.com/announce".into(),
            ],
        ]);
        let urls = ts.all_tracker_urls();
        assert_eq!(urls.len(), 3);
    }
}
