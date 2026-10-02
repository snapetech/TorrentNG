use http::Uri;

/// Query extractors materialize the complete request target before handlers
/// can bound individual parameters. Keep request-line allocation bounded at
/// every HTTP facade, including standalone native/compatibility routers.
pub const MAX_API_URI_BYTES: usize = 16 * 1024;

pub fn api_uri_is_bounded(uri: &Uri) -> bool {
    uri.path()
        .len()
        .saturating_add(uri.query().map_or(0, |query| query.len().saturating_add(1)))
        <= MAX_API_URI_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_targets_are_bounded_before_parameter_parsing() {
        let oversized: Uri = format!("/api/v1/torrents?filter={}", "x".repeat(MAX_API_URI_BYTES))
            .parse()
            .expect("valid test URI");
        assert!(!api_uri_is_bounded(&oversized));

        let normal: Uri = "/api/v1/torrents?limit=100"
            .parse()
            .expect("valid test URI");
        assert!(api_uri_is_bounded(&normal));
    }
}
