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
    if !path.ends_with("/announce") {
        return Err(TrackerError::Disabled);
    }
    let scrape_path = format!("{}scrape", path.trim_end_matches("announce"));
    url.set_path(&scrape_path);
    url.set_query(Some(&format!("info_hash={}", info_hash.url_encode())));
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
    fn invalid_tracker_url() {
        let req = test_request(TrackerEvent::Empty);
        assert!(req.to_http_query("not a url !!").is_err());
    }
}
