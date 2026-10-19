//! The default resolution of ordinary markdown links and images (`[x](url)`,
//! `![x](url)`, `![[target]]`): relative to the page's folder, or from the
//! root with a leading `/`, and never outside the root.

use percent_encoding::percent_decode_str;

use crate::names::{encode_path, folder_url, path_to_url};

/// Where a link or image URL points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// Left as written: empty, a fragment, a query only (`?x=1`, which the
    /// browser resolves against the page itself), protocol-relative, or has
    /// a scheme.
    Unchanged,
    /// The path climbs above the root, or can't be resolved because it is
    /// not valid UTF-8 once decoded.
    OutsideRoot,
    /// A path inside the root.
    Local {
        /// Root-relative, `/`-separated, decoded; `""` is the root.
        rel: String,
        /// The `?…` or `#…` part, verbatim.
        suffix: String,
        /// The URL's path ended with `/`.
        folder: bool,
    },
}

/// Resolves `url`, written on a page in `page_folder` (`""` at the root).
/// Whether the target exists is not checked.
pub fn resolve(page_folder: &str, url: &str) -> Resolved {
    if url.is_empty() || url.starts_with('#') || url.starts_with("//") || has_scheme(url) {
        return Resolved::Unchanged;
    }
    let split = url.find(['?', '#']).unwrap_or(url.len());
    let (path, suffix) = url.split_at(split);
    if path.is_empty() {
        // Only a `?…` remains: the `#…`-only case returned above.
        return Resolved::Unchanged;
    }
    // A relative URL left as written would be resolved by the browser, out
    // of reach of the root check; one that can't be decoded is confined by
    // treating it as outside the root.
    let Ok(path) = percent_decode_str(path).decode_utf8() else {
        return Resolved::OutsideRoot;
    };
    let mut segments: Vec<&str> = Vec::new();
    if !path.starts_with('/') {
        segments.extend(page_folder.split('/').filter(|s| !s.is_empty()));
    }
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Resolved::OutsideRoot;
                }
            }
            _ => segments.push(segment),
        }
    }
    Resolved::Local {
        rel: segments.join("/"),
        suffix: suffix.to_string(),
        folder: path.ends_with('/'),
    }
}

/// The href of a [`Resolved::Local`] target under `base` (no trailing
/// slash): a `.md` file is its page URL, a folder its index URL, anything
/// else its encoded path; then the suffix.
pub fn local_href(base: &str, rel: &str, suffix: &str, folder: bool) -> String {
    let path = if folder {
        folder_url(rel)
    } else if rel.ends_with(".md") {
        path_to_url(rel)
    } else {
        encode_path(rel)
    };
    format!("{base}{path}{suffix}")
}

/// `^[A-Za-z][A-Za-z0-9+.-]*:`
fn has_scheme(url: &str) -> bool {
    let mut chars = url.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(rel: &str, suffix: &str, folder: bool) -> Resolved {
        Resolved::Local {
            rel: rel.to_string(),
            suffix: suffix.to_string(),
            folder,
        }
    }

    #[test]
    fn resolve_from_a_subfolder() {
        let from = "Projects/Alpha";
        for (url, want) in [
            (
                "../Beta/shared_name.md",
                local("Projects/Beta/shared_name.md", "", false),
            ),
            (
                "Shared%20Name.md",
                local("Projects/Alpha/Shared Name.md", "", false),
            ),
            (
                "/Home.md#home-heading",
                local("Home.md", "#home-heading", false),
            ),
            ("../Beta/", local("Projects/Beta", "", true)),
            ("../../Home.md", local("Home.md", "", false)),
            ("../../../outside.md", Resolved::OutsideRoot),
            ("/../Home.md", Resolved::OutsideRoot),
            (
                "./diagram.png",
                local("Projects/Alpha/diagram.png", "", false),
            ),
            ("https://x/y.md", Resolved::Unchanged),
            ("mailto:a@b", Resolved::Unchanged),
            ("#frag", Resolved::Unchanged),
            ("//cdn/x", Resolved::Unchanged),
            ("", Resolved::Unchanged),
        ] {
            assert_eq!(resolve(from, url), want, "{url}");
        }
    }

    #[test]
    fn resolve_keeps_query_and_confines_invalid_utf8() {
        assert_eq!(resolve("", "a.md?x=1#y"), local("a.md", "?x=1#y", false));
        for url in [
            "%FF.md",
            "../../../../admin%FF",
            "../../../%FF.png",
            "x%FF/../../../../api/logout",
            "/%C3%28.md",
        ] {
            assert_eq!(resolve("a/b", url), Resolved::OutsideRoot, "{url}");
        }
    }

    #[test]
    fn query_only_urls_are_left_for_the_browser() {
        assert_eq!(resolve("a/b", "?x=1"), Resolved::Unchanged);
        assert_eq!(resolve("a/b", "?x=1#y"), Resolved::Unchanged);
        assert_eq!(resolve("", "?"), Resolved::Unchanged);
    }

    #[test]
    fn local_hrefs() {
        assert_eq!(
            local_href("/docs", "Projects/Alpha/Shared Name.md", "#h", false),
            "/docs/Projects/Alpha/Shared%20Name#h"
        );
        assert_eq!(local_href("", "Projects/Beta", "", true), "/Projects/Beta/");
        assert_eq!(local_href("", "", "", true), "/");
        assert_eq!(
            local_href("", "assets/my logo.svg", "", false),
            "/assets/my%20logo.svg"
        );
    }
}
