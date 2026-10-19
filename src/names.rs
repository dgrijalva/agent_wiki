use std::path::{Component, Path};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

/// Everything except RFC 3986 unreserved characters.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub fn norm(s: &str) -> String {
    s.replace(['-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Strips a leading `YYYY-MM-DD` followed by `-`, `_` or a space, if anything remains.
pub fn strip_date(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    if b.len() < 11 {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let shaped = digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && matches!(b[10], b'-' | b'_' | b' ');
    if !shaped {
        return None;
    }
    // Byte 10 is ASCII, so byte 11 is a char boundary.
    let rest = &s[11..];
    (!rest.is_empty()).then_some(rest)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileKeys {
    pub full: String,
    pub stripped: String,
}

impl FileKeys {
    pub fn from_rel(rel: &str) -> FileKeys {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let stem = name.strip_suffix(".md").unwrap_or(name);
        FileKeys {
            full: norm(stem),
            stripped: norm(strip_date(stem).unwrap_or(stem)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTarget {
    pub raw: String,
    pub name_key: String,
    pub qualifier: Vec<String>,
    /// The heading after the first `#`, if any.
    pub fragment: Option<String>,
}

impl LinkTarget {
    pub fn parse(raw: &str) -> Option<LinkTarget> {
        let (target, fragment) = match raw.split_once('#') {
            Some((target, fragment)) => (target.trim(), Some(fragment.to_string())),
            None => (raw.trim(), None),
        };
        if target.is_empty() {
            return None;
        }
        let mut segments: Vec<&str> = target.split('/').collect();
        let name = segments.pop().unwrap_or("");
        Some(LinkTarget {
            raw: target.to_string(),
            name_key: norm(name),
            qualifier: segments
                .into_iter()
                .map(norm)
                .filter(|s| !s.is_empty())
                .collect(),
            fragment,
        })
    }
}

pub fn path_to_url(rel: &str) -> String {
    encode_path(rel.strip_suffix(".md").unwrap_or(rel))
}

/// `/`-joined path with each segment percent-encoded, with a leading `/`.
pub fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|seg| format!("/{}", encode_component(seg)))
        .collect()
}

/// Percent-encodes everything except RFC 3986 unreserved characters (including `/`).
pub fn encode_component(s: &str) -> String {
    utf8_percent_encode(s, SEGMENT).to_string()
}

/// URL of a folder index (with trailing slash); `""` is the root, `/`.
pub fn folder_url(folder: &str) -> String {
    if folder.is_empty() {
        "/".to_string()
    } else {
        format!("{}/", encode_path(folder))
    }
}

/// Decodes a URL path (leading `/` optional) into a `/`-separated rel path.
/// `None` if a segment is empty, hidden, not UTF-8, or decodes to contain `/`.
pub fn url_to_path(url_path: &str) -> Option<String> {
    let path = url_path.strip_prefix('/').unwrap_or(url_path);
    let segments = path
        .split('/')
        .map(|seg| {
            let seg = percent_decode_str(seg).decode_utf8().ok()?;
            let ok = !seg.is_empty() && !seg.starts_with('.') && !seg.contains('/');
            ok.then(|| seg.into_owned())
        })
        .collect::<Option<Vec<_>>>()?;
    Some(segments.join("/"))
}

/// The page rel (`.md` appended) for a page URL path.
pub fn url_to_rel(url_path: &str) -> Option<String> {
    url_to_path(url_path).map(|path| format!("{path}.md"))
}

/// The content type of an image file extension (case-insensitive); `None`
/// for anything that is not a served image type.
pub fn image_type(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

/// The image content type of `path`'s last segment, by its extension.
pub fn image_type_of(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (_, ext) = name.rsplit_once('.')?;
    image_type(ext)
}

pub fn is_hidden(rel: &Path) -> bool {
    rel.components().any(|c| match c {
        Component::Normal(s) => s.to_string_lossy().starts_with('.'),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_folds_case_separators_and_whitespace() {
        for s in ["Foo-Example", "foo_example", "FOO  example "] {
            assert_eq!(norm(s), "foo example");
        }
    }

    #[test]
    fn strip_date_cases() {
        assert_eq!(strip_date("2021-01-01-foo-example"), Some("foo-example"));
        assert_eq!(strip_date("2024-03-05 Daily"), Some("Daily"));
        assert_eq!(strip_date("2021-01-01"), None);
        assert_eq!(strip_date("20210101-foo"), None);
    }

    #[test]
    fn strip_date_with_multibyte_at_byte_11() {
        // `–` (en dash) and each CJK char are multibyte and straddle byte 11.
        for s in ["Meetings – Q3", "会議メモの記録"] {
            assert_eq!(strip_date(s), None, "{s}");
            assert_eq!(FileKeys::from_rel(&format!("{s}.md")).full, norm(s));
        }
        assert_eq!(strip_date("2024-03-05 会議メモ"), Some("会議メモ"));
    }

    #[test]
    fn file_keys_for_dated_file() {
        let keys = FileKeys::from_rel("2021-01-01-foo-example.md");
        assert_eq!(keys.full, "2021 01 01 foo example");
        assert_eq!(keys.stripped, "foo example");
    }

    #[test]
    fn link_target_parse() {
        let t = LinkTarget::parse("Projects/Alpha/Shared Name#Sec").unwrap();
        assert_eq!(t.raw, "Projects/Alpha/Shared Name");
        assert_eq!(t.name_key, "shared name");
        assert_eq!(t.qualifier, ["projects", "alpha"]);
        assert_eq!(t.fragment.as_deref(), Some("Sec"));
        assert_eq!(LinkTarget::parse("Page").unwrap().fragment, None);
        assert_eq!(LinkTarget::parse("#Sec"), None);
    }

    #[test]
    fn url_round_trip() {
        assert_eq!(
            path_to_url("Projects/Alpha/Shared Name.md"),
            "/Projects/Alpha/Shared%20Name"
        );
        assert_eq!(
            url_to_rel("/Projects/Alpha/Shared%20Name").as_deref(),
            Some("Projects/Alpha/Shared Name.md")
        );
    }

    #[test]
    fn url_to_rel_rejects_unsafe_paths() {
        for url in [
            "../x",
            "a/./b",
            ".hidden/Secret",
            "a//b",
            "/..%2FCargo",
            "/",
        ] {
            assert_eq!(url_to_rel(url), None, "{url}");
        }
    }

    #[test]
    fn url_to_path_keeps_extensions() {
        assert_eq!(
            url_to_path("/Projects/Alpha/diagram.png").as_deref(),
            Some("Projects/Alpha/diagram.png")
        );
        assert_eq!(url_to_path("/.hidden/x.png"), None);
    }

    #[test]
    fn image_types() {
        assert_eq!(image_type("PNG"), Some("image/png"));
        assert_eq!(image_type("jpeg"), Some("image/jpeg"));
        assert_eq!(image_type("svg"), Some("image/svg+xml"));
        assert_eq!(image_type("txt"), None);
        assert_eq!(image_type_of("a.b/x.Gif"), Some("image/gif"));
        assert_eq!(image_type_of("a.png/x"), None);
        assert_eq!(image_type_of("png"), None);
    }

    #[test]
    fn hidden_paths() {
        assert!(is_hidden(Path::new(".hidden/Secret.md")));
        assert!(is_hidden(Path::new("a/.obsidian/x.md")));
        assert!(!is_hidden(Path::new("Projects/Alpha/Shared Name.md")));
    }
}
