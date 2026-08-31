//! Word-boundary-aware media-type classification for the sidebar TYPE facet
//! and the `media_type` list/filter query param.
//!
//! This exists because naive `LIKE '%pattern%'` substring matching (the
//! previous implementation) is dangerously imprecise: a glob like `%s%e%%`
//! intended to approximate "SxxExx" season/episode markers actually matches
//! any string containing an 's' anywhere before an 'e' anywhere later, which
//! is most English text. Registered as the `tng_media_type_match` SQLite
//! scalar function (see `cache::db`) so the same logic backs both the
//! per-torrent facet counts and the `media_type` list filter.

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// True if `needle` occurs in `haystack_lower` as a whole "word" - i.e. not
/// immediately preceded or followed by another alphanumeric character.
/// `needle` may itself contain non-alphanumeric characters (e.g. "web-dl");
/// only the boundary outside the match is checked.
fn contains_word(haystack_lower: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let needle_lower = needle.to_ascii_lowercase();
    for (idx, _) in haystack_lower.match_indices(&needle_lower) {
        let before_ok = haystack_lower.as_bytes()[..idx]
            .last()
            .map(|&b| !is_word_byte(b))
            .unwrap_or(true);
        let end = idx + needle_lower.len();
        let after_ok = haystack_lower.as_bytes()[end..]
            .first()
            .map(|&b| !is_word_byte(b))
            .unwrap_or(true);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// True if `ext_with_dot` (e.g. ".epub") occurs in `haystack_lower` followed
/// by a non-alphanumeric character or end of string. No left-boundary check
/// (filenames always have an alphanumeric character before an extension).
fn contains_extension(haystack_lower: &str, ext_with_dot: &str) -> bool {
    for (idx, _) in haystack_lower.match_indices(ext_with_dot) {
        let end = idx + ext_with_dot.len();
        let after_ok = haystack_lower.as_bytes()[end..]
            .first()
            .map(|&b| !is_word_byte(b))
            .unwrap_or(true);
        if after_ok {
            return true;
        }
    }
    false
}

fn contains_any_word(haystack_lower: &str, words: &[&str]) -> bool {
    words.iter().any(|w| contains_word(haystack_lower, w))
}

fn contains_any_ext(haystack_lower: &str, exts: &[&str]) -> bool {
    exts.iter().any(|e| contains_extension(haystack_lower, e))
}

fn is_ebook(haystack_lower: &str) -> bool {
    contains_any_word(
        haystack_lower,
        &["ebook", "ebooks", "book", "books", "audiobook"],
    ) || contains_any_ext(
        haystack_lower,
        &[".epub", ".mobi", ".azw3", ".pdf", ".cbz", ".cbr"],
    )
}

/// True if `haystack_lower` contains a whole SxxExx-style season/episode
/// marker, e.g. "S01E05", "s1e1", "S12E345". Requires 's', 1-2 digits,
/// 'e', 1-3 digits, with non-alphanumeric boundaries around the marker -
/// unlike a bare `%s%e%` substring glob this rejects ordinary English text
/// such as "Stackpole" or "Assumption of Risk".
fn contains_season_episode(haystack_lower: &str) -> bool {
    let bytes = haystack_lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b's' && (i == 0 || !is_word_byte(bytes[i - 1])) {
            let season_start = i + 1;
            let mut j = season_start;
            while j < bytes.len() && bytes[j].is_ascii_digit() && j - season_start < 2 {
                j += 1;
            }
            if j > season_start && j < bytes.len() && bytes[j] == b'e' {
                let ep_start = j + 1;
                let mut k = ep_start;
                while k < bytes.len() && bytes[k].is_ascii_digit() && k - ep_start < 3 {
                    k += 1;
                }
                if k > ep_start && (k == bytes.len() || !is_word_byte(bytes[k])) {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// Classify a torrent (by name/category/directory) against one of the
/// sidebar TYPE facet buckets. Mirrors the client-side icon heuristic in
/// `webui/src/components/TorrentTable.tsx` but with correct word-boundary
/// matching instead of raw substring search.
pub fn matches(name: &str, category: &str, directory: &str, tags: &str, media_type: &str) -> bool {
    let haystack = format!("{name} {category} {tags} {directory}").to_ascii_lowercase();
    match media_type {
        "ebook" => is_ebook(&haystack),
        "tv" => {
            contains_any_word(
                &haystack,
                &["season", "episode", "hdtv", "web-dl", "webrip", "tv"],
            ) || contains_season_episode(&haystack)
        }
        "video" => {
            contains_any_word(
                &haystack,
                &[
                    "movie", "movies", "film", "bluray", "bdrip", "dvdrip", "x264", "x265",
                    "2160p", "1080p", "720p",
                ],
            ) || contains_any_ext(&haystack, &[".mkv", ".mp4", ".avi", ".mov", ".wmv", ".m4v"])
        }
        "audio" => {
            contains_any_word(&haystack, &["music", "album", "discography"])
                || contains_any_ext(
                    &haystack,
                    &[".flac", ".mp3", ".aac", ".ogg", ".opus", ".wav", ".m4a"],
                )
        }
        "image" => {
            contains_any_word(
                &haystack,
                &["installer", "image", "linux", "ubuntu", "debian", "fedora"],
            ) || contains_any_ext(&haystack, &[".iso", ".img", ".dmg"])
        }
        // Match the client-side precedence: a clearly identified ebook is
        // not also treated as a game just because its title contains the
        // whole word "games" (e.g. "Empire Games 02.epub").
        "game" => {
            !is_ebook(&haystack)
                && contains_any_word(
                    &haystack,
                    &[
                        "game", "games", "gog", "steam", "switch", "ps4", "ps5", "xbox",
                    ],
                )
        }
        "software" => {
            contains_any_word(
                &haystack,
                &[
                    "app", "software", "source", "code", "github", "windows", "macos",
                ],
            ) || contains_any_ext(
                &haystack,
                &[
                    ".exe", ".msi", ".pkg", ".deb", ".rpm", ".zip", ".tar", ".gz", ".xz", ".7z",
                    ".rar",
                ],
            )
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ebook_matches_by_extension_not_by_stray_book_substring() {
        assert!(matches(
            "Assumption of Risk - Michael A. Stackpole.epub",
            "books",
            "",
            "",
            "ebook"
        ));
        // "book" inside "Facebook" must not count as the word "book".
        assert!(!matches(
            "Facebook - The Inside Story.mkv",
            "",
            "",
            "",
            "ebook"
        ));
    }

    #[test]
    fn tv_requires_a_real_season_episode_marker_or_keyword() {
        assert!(matches("Show.Name.S01E05.1080p.mkv", "", "", "", "tv"));
        assert!(matches("Some.Show.Season.2.Episode.4", "", "", "", "tv"));
        // These were false positives under the old `%s%e%` glob.
        assert!(!matches(
            "Assumption of Risk - Michael A. Stackpole.epub",
            "books",
            "",
            "",
            "tv"
        ));
        assert!(!matches(
            "#1 Hits of the 90's [FLAC]",
            "redacted",
            "",
            "",
            "tv"
        ));
        assert!(!matches(
            "(3) Life Reset-Hobnobbing.epub",
            "books",
            "",
            "",
            "tv"
        ));
    }

    #[test]
    fn game_matches_whole_word_not_substring_inside_other_words() {
        assert!(matches(
            "Half-Life: Alyx GAME REPACK",
            "games",
            "",
            "",
            "game"
        ));
        // A title word is not enough to override a strong ebook signal.
        assert!(!matches(
            "Charles Stross - Empire Games 02.epub",
            "books",
            "",
            "",
            "game"
        ));
        assert!(!matches(
            "Endgamemanagement Handbook.pdf",
            "books",
            "",
            "",
            "game"
        ));
    }

    #[test]
    fn season_episode_marker_rejects_ordinary_text() {
        assert!(!contains_season_episode("stackpole"));
        assert!(!contains_season_episode("assumption of risk"));
        assert!(contains_season_episode("show.name.s01e05.mkv"));
        assert!(contains_season_episode("s1e1"));
        assert!(contains_season_episode("s12e345"));
        assert!(!contains_season_episode("xS01E05"));
        assert!(!contains_season_episode("S01E0512"));
        assert!(!contains_season_episode("S01E05final"));
    }

    #[test]
    fn type_hints_include_tags() {
        assert!(matches("untitled", "", "", "tv", "tv"));
    }
}
