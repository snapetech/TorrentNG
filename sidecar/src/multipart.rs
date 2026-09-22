use anyhow::{bail, Context, Result};
use axum::extract::multipart::Field;

pub(crate) const MAX_MULTIPART_TORRENT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_MULTIPART_ENVELOPE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_MULTIPART_REQUEST_BODY_BYTES: usize =
    MAX_MULTIPART_TORRENT_BYTES + MAX_MULTIPART_ENVELOPE_BYTES;
/// Keep ordinary JSON/Form routes at Axum's established 2 MiB default instead
/// of inheriting the much larger multipart upload ceiling. Individual routes
/// can opt into the upload limit above.
pub(crate) const MAX_DEFAULT_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_AUTH_REQUEST_BODY_BYTES: usize = 16 * 1024;

pub(crate) async fn read_bounded_multipart_bytes(
    mut field: Field<'_>,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let mut data = Vec::with_capacity(max_bytes.min(4 * 1024));
    while let Some(chunk) = field.chunk().await.context("read multipart field chunk")? {
        if chunk.len() > max_bytes.saturating_sub(data.len()) {
            bail!("multipart field exceeds the {max_bytes}-byte limit");
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

pub(crate) async fn read_bounded_multipart_text(
    field: Field<'_>,
    max_bytes: usize,
) -> Result<String> {
    String::from_utf8(read_bounded_multipart_bytes(field, max_bytes).await?)
        .context("multipart text field is not valid UTF-8")
}
