use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use super::client::{Client, XmlValue};

#[derive(Debug, Clone, Serialize)]
pub struct RawFile {
    pub index: usize,
    pub path: String,
    pub size_bytes: i64,
    pub size_chunks: i64,
    pub completed_chunks: i64,
    pub priority: i64, // 0=off, 1=normal, 2=high
    pub is_created: bool,
    pub is_open: bool,
}

impl Client {
    pub async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>> {
        let result = self
            .call(
                "f.multicall",
                &[
                    hash.into(),
                    "".into(),
                    "f.path=".into(),
                    "f.size_bytes=".into(),
                    "f.size_chunks=".into(),
                    "f.completed_chunks=".into(),
                    "f.priority=".into(),
                    "f.is_created=".into(),
                    "f.is_open=".into(),
                ],
            )
            .await
            .with_context(|| format!("f.multicall {hash}"))?;

        let rows = result.try_into_array()?;
        let mut out = Vec::with_capacity(rows.len());
        for (i, row) in rows.into_iter().enumerate() {
            let f = row.try_into_array()?;
            if f.len() < 7 {
                bail!("rTorrent file row {i} returned {} fields", f.len());
            }
            let size_bytes = required_i64(&f, 1, "f.size_bytes")?;
            let size_chunks = required_i64(&f, 2, "f.size_chunks")?;
            let completed_chunks = required_i64(&f, 3, "f.completed_chunks")?;
            let priority = required_i64(&f, 4, "f.priority")?;
            validate_file_projection(
                i,
                size_bytes,
                size_chunks,
                completed_chunks,
                priority,
            )?;
            out.push(RawFile {
                index: i,
                path: required_path(&f, 0)?,
                size_bytes,
                size_chunks,
                completed_chunks,
                priority,
                is_created: required_bool(&f, 5, "f.is_created")?,
                is_open: required_bool(&f, 6, "f.is_open")?,
            });
        }
        Ok(out)
    }

    /// Set file priority. priority: 0=off, 1=normal, 2=high
    pub async fn set_file_priority(
        &self,
        hash: &str,
        file_index: usize,
        priority: i64,
    ) -> Result<()> {
        if !(0..=2).contains(&priority) {
            bail!("rTorrent file priority must be between 0 and 2");
        }
        // f.priority.set takes hash, index, priority
        self.call(
            "f.priority.set",
            &[
                hash.into(),
                XmlValue::Int(file_index as i64),
                XmlValue::Int(priority),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn rename_file(&self, hash: &str, file_index: usize, name: &str) -> Result<()> {
        self.call(
            "f.path.set",
            &[hash.into(), XmlValue::Int(file_index as i64), name.into()],
        )
        .await?;
        Ok(())
    }
}

fn required_path(fields: &[XmlValue], index: usize) -> Result<String> {
    fields
        .get(index)
        .and_then(XmlValue::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("rTorrent response omitted valid f.path"))
}

fn required_i64(fields: &[XmlValue], index: usize, name: &str) -> Result<i64> {
    fields
        .get(index)
        .and_then(XmlValue::as_i64)
        .ok_or_else(|| anyhow!("rTorrent response omitted valid {name}"))
}

fn validate_file_projection(
    index: usize,
    size_bytes: i64,
    size_chunks: i64,
    completed_chunks: i64,
    priority: i64,
) -> Result<()> {
    if size_bytes < 0 {
        bail!("rTorrent file {index} returned negative f.size_bytes");
    }
    if size_chunks < 0 {
        bail!("rTorrent file {index} returned negative f.size_chunks");
    }
    if completed_chunks < 0 {
        bail!("rTorrent file {index} returned negative f.completed_chunks");
    }
    if completed_chunks > size_chunks {
        bail!(
            "rTorrent file {index} completed chunks {completed_chunks} exceed size chunks {size_chunks}"
        );
    }
    if !(0..=2).contains(&priority) {
        bail!("rTorrent file {index} returned invalid f.priority {priority}");
    }
    Ok(())
}

fn required_bool(fields: &[XmlValue], index: usize, name: &str) -> Result<bool> {
    fields
        .get(index)
        .and_then(XmlValue::as_bool)
        .ok_or_else(|| anyhow!("rTorrent response omitted valid {name}"))
}

#[cfg(test)]
mod tests {
    use super::validate_file_projection;

    #[test]
    fn file_projection_rejects_inconsistent_numeric_fields() {
        assert!(validate_file_projection(0, -1, 0, 0, 0).is_err());
        assert!(validate_file_projection(0, 1, -1, 0, 0).is_err());
        assert!(validate_file_projection(0, 1, 1, -1, 0).is_err());
        assert!(validate_file_projection(0, 1, 1, 2, 0).is_err());
        assert!(validate_file_projection(0, 1, 1, 1, -1).is_err());
        assert!(validate_file_projection(0, 1, 1, 1, 3).is_err());
        assert!(validate_file_projection(0, 1, 1, 1, 2).is_ok());
    }
}
