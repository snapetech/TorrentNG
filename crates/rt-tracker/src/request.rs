use url::Url;

use crate::error::TrackerError;

/// SHA-1 or SHA-256 infohash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InfoHash {
    V1([u8; 20]),
    V2([u8; 32]),
}

impl InfoHash {
    pub fn as_v1(&self) -> Option<&[u8; 20]> {
        match self {
            InfoHash::V1(h) => Some(h),
            InfoHash::V2(_) => None,
        }
    }

    /// URL-encoded form for HTTP announces.
    pub fn url_encode(&self) -> String {
        match self {
            InfoHash::V1(h) => url_encode_bytes(h),
            InfoHash::V2(h) => url_encode_bytes(h),
        }
    }
}

fn url_encode_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| format!("%{b:02X}")).collect()
}

/// BEP 3 tracker event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerEvent {
    /// First announce for this torrent session.
    Started,
    /// Sent before client shuts down or removes torrent.
    Stopped,
    /// Sent when download completes.
    Completed,
    /// Regular re-announce (no event field sent).
    Empty,
}

impl TrackerEvent {
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            TrackerEvent::Started => Some("started"),
            TrackerEvent::Stopped => Some("stopped"),
            TrackerEvent::Completed => Some("completed"),
            TrackerEvent::Empty => None,
        }
    }
}

/// Parameters for a tracker announce.
#[derive(Debug, Clone)]
pub struct AnnounceRequest {
    pub info_hash: InfoHash,
    pub peer_id: [u8; 20],
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    /// Bytes remaining; 0 if complete.
    pub left: u64,
    pub event: TrackerEvent,
    /// Request compact peer response (BEP 23).
    pub compact: bool,
    /// Optional: explicit number of peers to request.
    pub numwant: Option<u32>,
}

impl AnnounceRequest {
    /// Build the HTTP query string for an HTTP tracker announce.
    pub fn to_http_query(&self, tracker_url: &str) -> Result<String, TrackerError> {
        self.to_http_query_with_tracker_id(tracker_url, None)
    }

    /// Build the HTTP query string and echo a tracker-provided `tracker id`
    /// when one was returned by an earlier announce. BEP 3 trackers may use
    /// this value to associate subsequent announces with the original
    /// session; dropping it produces incorrect reannounce traffic for those
    /// trackers.
    pub fn to_http_query_with_tracker_id(
        &self,
        tracker_url: &str,
        tracker_id: Option<&[u8]>,
    ) -> Result<String, TrackerError> {
        let mut url =
            Url::parse(tracker_url).map_err(|e| TrackerError::InvalidUrl(e.to_string()))?;
        let existing_query = url.query().map(str::to_owned);
        let fragment = url.fragment().map(str::to_owned);
        url.set_query(None);
        url.set_fragment(None);

        let mut query_parts = Vec::new();
        if let Some(query) = existing_query.filter(|query| !query.is_empty()) {
            query_parts.push(query);
        }
        query_parts.push(format!("peer_id={}", url_encode_bytes(&self.peer_id)));
        query_parts.push(format!("port={}", self.port));
        query_parts.push(format!("uploaded={}", self.uploaded));
        query_parts.push(format!("downloaded={}", self.downloaded));
        query_parts.push(format!("left={}", self.left));
        query_parts.push(format!("compact={}", if self.compact { 1 } else { 0 }));
        if let Some(n) = self.numwant {
            query_parts.push(format!("numwant={n}"));
        }
        if let Some(tracker_id) = tracker_id.filter(|value| !value.is_empty()) {
            query_parts.push(format!("trackerid={}", url_encode_bytes(tracker_id)));
        }
        if let Some(ev) = self.event.as_str() {
            query_parts.push(format!("event={ev}"));
        }
        query_parts.push(format!("info_hash={}", self.info_hash.url_encode()));

        let mut out = url.to_string();
        out.push('?');
        out.push_str(&query_parts.join("&"));
        if let Some(fragment) = fragment {
            out.push('#');
            out.push_str(&fragment);
        }
        Ok(out)
    }
}

/// Build a BEP 48 HTTP scrape URL for trackers exposing `/scrape`.
pub fn to_http_scrape_url(tracker_url: &str, info_hash: InfoHash) -> Result<String, TrackerError> {
    let mut url = Url::parse(tracker_url).map_err(|e| TrackerError::InvalidUrl(e.to_string()))?;
    let path = url.path().to_owned();
    let Some((base, leaf)) = path.rsplit_once('/') else {
        return Err(TrackerError::Disabled);
    };
    let Some(suffix) = leaf.strip_prefix("announce") else {
        return Err(TrackerError::Disabled);
    };
    let scrape_path = format!("{base}/scrape{suffix}");
    let existing_query = url.query().map(str::to_owned);
    url.set_path(&scrape_path);
    url.set_fragment(None);
    let info_hash_query = format!("info_hash={}", info_hash.url_encode());
    let query = match existing_query.filter(|query| !query.is_empty()) {
        Some(query) => format!("{query}&{info_hash_query}"),
        None => info_hash_query,
    };
    url.set_query(Some(&query));
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request(event: TrackerEvent) -> AnnounceRequest {
        AnnounceRequest {
            info_hash: InfoHash::V1([0xABu8; 20]),
            peer_id: [0x2Du8; 20],
            port: 6881,
            uploaded: 1024,
            downloaded: 2048,
            left: 0,
            event,
            compact: true,
            numwant: Some(50),
        }
    }

    #[test]
    fn url_contains_required_fields() {
        let req = test_request(TrackerEvent::Started);
        let url = req
            .to_http_query("http://tracker.example.com/announce")
            .unwrap();
        assert!(url.contains("port=6881"));
        assert!(url.contains("uploaded=1024"));
        assert!(url.contains("downloaded=2048"));
        assert!(url.contains("left=0"));
        assert!(url.contains("compact=1"));
        assert!(url.contains("event=started"));
        assert!(url.contains("numwant=50"));
        assert!(url.contains("info_hash="));
    }

    #[test]
    fn tracker_id_is_echoed_only_when_provided() {
        let req = test_request(TrackerEvent::Empty);
        let without_id = req
            .to_http_query("http://tracker.example.com/announce")
            .unwrap();
        assert!(!without_id.contains("trackerid="));

        let with_id = req
            .to_http_query_with_tracker_id(
                "http://tracker.example.com/announce",
                Some(b"id-42".as_slice()),
            )
            .unwrap();
        assert!(with_id.contains("trackerid=%69%64%2D%34%32"));
    }

    #[test]
    fn private_tracker_accounting_values_are_exact_in_query() {
        let req = AnnounceRequest {
            info_hash: InfoHash::V1([0xCDu8; 20]),
            peer_id: [0x2Du8; 20],
            port: 6881,
            uploaded: 9_876_543_210,
            downloaded: 1_234_567_890,
            left: 42,
            event: TrackerEvent::Completed,
            compact: true,
            numwant: Some(0),
        };

        let url = req
            .to_http_query("https://private.example/announce?passkey=secret")
            .unwrap();

        assert!(url.contains("uploaded=9876543210"));
        assert!(url.contains("downloaded=1234567890"));
        assert!(url.contains("left=42"));
        assert!(url.contains("event=completed"));
        assert!(url.contains("passkey=secret"));
    }

    #[test]
    fn empty_event_not_in_query() {
        let req = test_request(TrackerEvent::Empty);
        let url = req
            .to_http_query("http://tracker.example.com/announce")
            .unwrap();
        assert!(!url.contains("event="));
    }

    #[test]
    fn stopped_event_in_query() {
        let req = test_request(TrackerEvent::Stopped);
        let url = req
            .to_http_query("http://tracker.example.com/announce")
            .unwrap();
        assert!(url.contains("event=stopped"));
    }

    #[test]
    fn info_hash_url_encoded() {
        let req = test_request(TrackerEvent::Empty);
        let url = req
            .to_http_query("http://tracker.example.com/announce")
            .unwrap();
        // 0xAB percent-encoded is %AB
        assert!(url.contains("%AB"));
    }

    #[test]
    fn v2_info_hash_url_encoded_for_http_announce() {
        let mut req = test_request(TrackerEvent::Started);
        req.info_hash = InfoHash::V2([0x22; 32]);

        let url = req
            .to_http_query("https://tracker.example.com/announce?passkey=abc")
            .unwrap();

        assert!(url.contains("?passkey=abc&peer_id="));
        assert!(url.contains("event=started"));
        assert!(url.contains(&format!("info_hash={}", "%22".repeat(32))));
        assert!(!url.contains("%2522"));
    }

    #[test]
    fn peer_id_is_not_double_encoded() {
        let req = test_request(TrackerEvent::Empty);
        let url = req
            .to_http_query("http://tracker.example.com/announce")
            .unwrap();
        assert!(url.contains("peer_id=%2D%2D"));
        assert!(!url.contains("peer_id=%252D"));
    }

    #[test]
    fn preserves_existing_query() {
        let req = test_request(TrackerEvent::Empty);
        let url = req
            .to_http_query("http://tracker.example.com/announce?passkey=abc")
            .unwrap();
        assert!(url.contains("?passkey=abc&peer_id="));
    }

    #[test]
    fn scrape_url_rewrites_announce_path() {
        let url = to_http_scrape_url(
            "http://tracker.example.com/path/announce?passkey=abc",
            InfoHash::V1([0x11; 20]),
        )
        .unwrap();
        assert!(url.starts_with("http://tracker.example.com/path/scrape?"));
        assert!(url.contains("info_hash="));
    }

    #[test]
    fn scrape_url_preserves_existing_query_and_hash_encoding() {
        let url = to_http_scrape_url(
            "http://tracker.example.com/path/announce.php?passkey=abc",
            InfoHash::V1([0x11; 20]),
        )
        .unwrap();
        assert!(url.starts_with("http://tracker.example.com/path/scrape.php?passkey=abc&"));
        assert!(url.contains("info_hash=%11%11"));
        assert!(!url.contains("%2511"));
    }

    #[test]
    fn scrape_url_accepts_v2_info_hash() {
        let url = to_http_scrape_url(
            "http://tracker.example.com/path/announce?passkey=abc",
            InfoHash::V2([0x33; 32]),
        )
        .unwrap();

        assert!(url.starts_with("http://tracker.example.com/path/scrape?passkey=abc&"));
        assert!(url.contains(&format!("info_hash={}", "%33".repeat(32))));
        assert!(!url.contains("%2533"));
    }

    #[test]
    fn scrape_url_rejects_non_announce_paths() {
        assert!(matches!(
            to_http_scrape_url(
                "http://tracker.example.com/path/not-announce?passkey=abc",
                InfoHash::V1([0x11; 20])
            ),
            Err(TrackerError::Disabled)
        ));
    }

    #[test]
    fn invalid_tracker_url() {
        let req = test_request(TrackerEvent::Empty);
        assert!(req.to_http_query("not a url !!").is_err());
    }
}
