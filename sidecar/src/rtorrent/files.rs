use anyhow::{Context, Result};
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

        let rows = result.into_array();
        let mut out = Vec::with_capacity(rows.len());
        for (i, row) in rows.into_iter().enumerate() {
            let f = row.into_array();
            if f.len() < 7 {
                continue;
            }
            out.push(RawFile {
                index: i,
                path: sf(&f, 0),
                size_bytes: nf(&f, 1),
                size_chunks: nf(&f, 2),
                completed_chunks: nf(&f, 3),
                priority: nf(&f, 4),
                is_created: bf(&f, 5),
                is_open: bf(&f, 6),
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

fn sf(f: &[XmlValue], i: usize) -> String {
    f.get(i).and_then(|v| v.as_str()).unwrap_or("").to_owned()
}
fn nf(f: &[XmlValue], i: usize) -> i64 {
    f.get(i).and_then(|v| v.as_i64()).unwrap_or(0)
}
fn bf(f: &[XmlValue], i: usize) -> bool {
    f.get(i).and_then(|v| v.as_bool()).unwrap_or(false)
}
