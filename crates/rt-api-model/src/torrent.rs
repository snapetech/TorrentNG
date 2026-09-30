/// API-facing torrent representation (snake_case JSON).
use std::marker::PhantomData;

use serde::{de, Deserialize, Deserializer, Serialize};

const MAX_API_LIST_ITEMS: usize = 16_384;
const MAX_API_TAG_ITEMS: usize = 1_024;
const MAX_API_TAG_BYTES: usize = 256;

/// Summary returned by `GET /api/v1/torrents`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TorrentSummary {
    pub info_hash: String,
    pub name: String,
    pub state: String,
    pub total_length: i64,
    /// Cumulative transfer bytes downloaded; use `amount_left` for current
    /// payload progress after rechecks.
    pub downloaded: i64,
    /// Live bytes still missing from the payload. Unlike `downloaded`, this
    /// is not cumulative transfer accounting and remains correct after a
    /// recheck discovers missing pieces.
    #[serde(default)]
    pub amount_left: i64,
    pub uploaded: i64,
    pub ratio: f64,
    pub save_path: String,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub added_at: i64,
    pub completed_at: Option<i64>,
    pub num_peers: u32,
    pub num_seeds: u32,
    /// The active tracker's failure/warning message, if any -- a torrent
    /// can be actively seeding or downloading fine while its tracker
    /// rejects announces (e.g. "torrent not registered with this
    /// tracker"), which `state` alone never reflects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracker_message: Option<String>,
}

/// Full detail returned by `GET /api/v1/torrents/{hash}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TorrentDetail {
    #[serde(flatten)]
    pub summary: TorrentSummary,
    pub piece_length: i64,
    pub piece_count: i64,
    pub is_private: bool,
    pub trackers: Vec<String>,
    pub files: Vec<FileInfo>,
}

/// Single file within a torrent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileInfo {
    pub file_index: u32,
    pub path: String,
    pub length: i64,
    pub priority: u8,
}

/// JSON request body for `POST /api/v1/torrents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddTorrentRequest {
    /// Base64-encoded .torrent file content.
    pub torrent_b64: Option<String>,
    /// Magnet link (alternative to torrent_b64).
    pub magnet: Option<String>,
    pub save_path: String,
    #[serde(default, deserialize_with = "deserialize_bounded_optional_label")]
    pub category: Option<String>,
    #[serde(default, deserialize_with = "deserialize_bounded_optional_tags")]
    pub tags: Option<Vec<String>>,
    /// Start immediately after adding.
    pub start: Option<bool>,
}

/// JSON response from `POST /api/v1/torrents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddTorrentResponse {
    pub info_hash: String,
}

/// Request body for `POST /api/v1/torrents/{hash}/pause` and `/resume`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HashListRequest {
    #[serde(deserialize_with = "deserialize_bounded_vec")]
    pub hashes: Vec<String>,
}

fn deserialize_bounded_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedVecVisitor<T>(PhantomData<T>);

    impl<'de, T> de::Visitor<'de> for BoundedVecVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bounded array")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut values = Vec::with_capacity(
                sequence
                    .size_hint()
                    .unwrap_or_default()
                    .min(MAX_API_LIST_ITEMS),
            );
            while let Some(value) = sequence.next_element::<T>()? {
                if values.len() >= MAX_API_LIST_ITEMS {
                    return Err(de::Error::custom(format!(
                        "array exceeds maximum of {MAX_API_LIST_ITEMS} items"
                    )));
                }
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(BoundedVecVisitor(PhantomData))
}

struct BoundedApiLabel(String);

impl<'de> Deserialize<'de> for BoundedApiLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LabelVisitor;

        impl de::Visitor<'_> for LabelVisitor {
            type Value = BoundedApiLabel;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a label with at most 256 UTF-8 bytes")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_API_TAG_BYTES {
                    return Err(E::custom(format!(
                        "label exceeds maximum of {MAX_API_TAG_BYTES} UTF-8 bytes"
                    )));
                }
                Ok(BoundedApiLabel(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_API_TAG_BYTES {
                    return Err(E::custom(format!(
                        "label exceeds maximum of {MAX_API_TAG_BYTES} UTF-8 bytes"
                    )));
                }
                Ok(BoundedApiLabel(value))
            }
        }

        deserializer.deserialize_string(LabelVisitor)
    }
}

fn deserialize_bounded_optional_label<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalLabelVisitor;

    impl<'de> de::Visitor<'de> for OptionalLabelVisitor {
        type Value = Option<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("null or a label with at most 256 UTF-8 bytes")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            BoundedApiLabel::deserialize(deserializer).map(|label| Some(label.0))
        }
    }

    deserializer.deserialize_option(OptionalLabelVisitor)
}

fn deserialize_bounded_tags<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct TagsVisitor;

    impl<'de> de::Visitor<'de> for TagsVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an array of at most 1024 labels, each at most 256 UTF-8 bytes")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut tags = Vec::with_capacity(
                sequence
                    .size_hint()
                    .unwrap_or_default()
                    .min(MAX_API_TAG_ITEMS),
            );
            while let Some(tag) = sequence.next_element::<BoundedApiLabel>()? {
                if tags.len() >= MAX_API_TAG_ITEMS {
                    return Err(de::Error::custom(format!(
                        "array exceeds maximum of {MAX_API_TAG_ITEMS} labels"
                    )));
                }
                tags.push(tag.0);
            }
            Ok(tags)
        }
    }

    deserializer.deserialize_seq(TagsVisitor)
}

fn deserialize_bounded_optional_tags<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalTagsVisitor;

    impl<'de> de::Visitor<'de> for OptionalTagsVisitor {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("null or a bounded label array")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            deserialize_bounded_tags(deserializer).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalTagsVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torrent_summary_serializes() {
        let s = TorrentSummary {
            info_hash: "a".repeat(40),
            name: "test".into(),
            state: "seeding".into(),
            total_length: 1_000_000,
            downloaded: 1_000_000,
            amount_left: 0,
            uploaded: 5_000_000,
            ratio: 5.0,
            save_path: "/data".into(),
            category: None,
            tags: vec!["hd".into()],
            added_at: 1_700_000_000,
            completed_at: None,
            num_peers: 3,
            num_seeds: 10,
            tracker_message: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: TorrentSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn add_request_optional_fields() {
        let json = r#"{"save_path":"/data","torrent_b64":"abc"}"#;
        let req: AddTorrentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.save_path, "/data");
        assert!(req.category.is_none());
        assert!(req.start.is_none());
    }

    #[test]
    fn add_request_rejects_oversized_tag_lists_during_deserialization() {
        let json = serde_json::json!({
            "save_path": "/data",
            "tags": vec!["tag"; MAX_API_TAG_ITEMS + 1],
        });
        assert!(serde_json::from_value::<AddTorrentRequest>(json).is_err());
    }

    #[test]
    fn add_request_rejects_oversized_labels_during_deserialization() {
        let category = serde_json::json!({
            "save_path": "/data",
            "category": "x".repeat(MAX_API_TAG_BYTES + 1),
        });
        assert!(serde_json::from_value::<AddTorrentRequest>(category).is_err());

        let tag = serde_json::json!({
            "save_path": "/data",
            "tags": ["x".repeat(MAX_API_TAG_BYTES + 1)],
        });
        assert!(serde_json::from_value::<AddTorrentRequest>(tag).is_err());
    }

    #[test]
    fn hash_list_rejects_oversized_lists_during_deserialization() {
        let json = serde_json::json!({
            "hashes": vec!["a"; MAX_API_LIST_ITEMS + 1],
        });
        assert!(serde_json::from_value::<HashListRequest>(json).is_err());
    }
}
