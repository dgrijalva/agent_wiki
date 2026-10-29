//! The script filter: which scripts in a page's content may run.
//!
//! With a filter set, every page carries a Content-Security-Policy whose
//! `script-src` names each script it allows, by hash for code written in the
//! page and by URL for a file. The library's own scripts and the template's
//! head are allowed. A script in the htmx root runs only if the filter allows
//! it; one it denies is also taken out of the markup.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;

use base64::Engine;
use lol_html::errors::RewritingError;
use lol_html::html_content::{ContentType, Element};
use lol_html::{RewriteStrSettings, element, rewrite_str, text};
use percent_encoding::percent_decode_str;
use sha2::{Digest, Sha256};

use crate::template::PageKind;

/// A script in a page's content, which the host's filter allows or denies
/// ([`WikiBuilder::scripts`](crate::WikiBuilder::scripts)).
pub struct Script<'a> {
    pub(crate) page: &'a PageKind,
    pub(crate) url: &'a str,
    pub(crate) kind: ScriptKind<'a>,
}

impl<'a> Script<'a> {
    /// What kind of page the script is on.
    pub fn page(&self) -> &'a PageKind {
        self.page
    }

    /// The page's URL path, base included, e.g. `/docs/Projects/Alpha`.
    pub fn url(&self) -> &'a str {
        self.url
    }

    pub fn kind(&self) -> ScriptKind<'a> {
        self.kind
    }
}

/// What a [`Script`] is. Attribute values have their character references
/// decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind<'a> {
    /// A `<script>` element's code.
    Inline(&'a str),
    /// A `<script src>` element's URL, as written.
    External(&'a str),
    /// An event-handler attribute, such as `onerror` on an `<img>`.
    Handler { attribute: &'a str, code: &'a str },
    /// A `javascript:` URL, such as a link's `href`.
    Url(&'a str),
}

/// The policy for a response that runs no scripts, such as an image.
pub(crate) const NO_SCRIPTS: &str = "script-src 'none'; object-src 'none'";

/// What replaces a script the filter denies. Something must: removing the
/// element alone can join the text around it into a new tag.
const BLOCKED: &str = "<!-- script blocked -->";

/// Attributes whose `javascript:` URL runs when followed or submitted.
const URL_ATTRIBUTES: &[&str] = &["href", "xlink:href", "src", "action", "formaction", "data"];

/// The scripts a page may run, written out as its Content-Security-Policy.
#[derive(Debug, Default)]
pub(crate) struct Policy {
    sources: BTreeSet<String>,
    /// Allows hashed event handlers and `javascript:` URLs.
    unsafe_hashes: bool,
}

impl Policy {
    /// Allows `source` as written, e.g. a URL prefix ending in `/`.
    pub(crate) fn allow_source(&mut self, source: &str) {
        self.sources.insert(source.to_string());
    }

    /// Allows every script in `html`, markup the library or the host wrote.
    pub(crate) fn trust(&mut self, html: &str) {
        for found in find(html).unwrap_or_default() {
            self.allow(&found);
        }
    }

    /// `html` without the scripts `allows` denies, and allows the rest.
    /// Markup that can't be read unambiguously is left as it is, and none of
    /// its scripts are allowed.
    pub(crate) fn filter(&mut self, html: &str, allows: impl Fn(ScriptKind<'_>) -> bool) -> String {
        let Ok(found) = find(html) else {
            return html.to_string();
        };
        let keep: Vec<bool> = found.iter().map(|f| allows(f.kind())).collect();
        let out = if keep.iter().all(|&k| k) {
            html.to_string()
        } else {
            match remove(html, &keep) {
                Ok(out) => out,
                Err(_) => return html.to_string(),
            }
        };
        for (found, _) in found.iter().zip(&keep).filter(|(_, k)| **k) {
            self.allow(found);
        }
        out
    }

    fn allow(&mut self, found: &Found) {
        match found {
            Found::Inline(code) => {
                self.sources.insert(hash(code));
            }
            Found::External(src) => {
                if let Some(source) = source(src) {
                    self.sources.insert(source);
                }
            }
            Found::Handler { code, .. } => {
                self.unsafe_hashes = true;
                self.sources.insert(hash(code));
            }
            Found::Url(url) => {
                self.unsafe_hashes = true;
                // The browser hashes the URL percent-decoded.
                let url = url_text(url);
                let decoded = percent_decode_str(&url).decode_utf8_lossy();
                self.sources.insert(hash(&decoded));
            }
        }
    }

    /// The `Content-Security-Policy` value.
    pub(crate) fn csp(&self) -> String {
        let mut out = String::from("script-src");
        if self.sources.is_empty() {
            out.push_str(" 'none'");
        }
        if self.unsafe_hashes {
            out.push_str(" 'unsafe-hashes'");
        }
        for source in &self.sources {
            out.push(' ');
            out.push_str(source);
        }
        out.push_str("; object-src 'none'");
        out
    }
}

/// A script found in markup, owning its text.
#[derive(Debug, Clone, PartialEq)]
enum Found {
    Inline(String),
    External(String),
    Handler { attribute: String, code: String },
    Url(String),
}

impl Found {
    fn kind(&self) -> ScriptKind<'_> {
        match self {
            Found::Inline(code) => ScriptKind::Inline(code),
            Found::External(src) => ScriptKind::External(src),
            Found::Handler { attribute, code } => ScriptKind::Handler { attribute, code },
            Found::Url(url) => ScriptKind::Url(url),
        }
    }
}

/// Settings that tokenize as a browser does, and fail on markup a browser
/// could read either of two ways.
fn settings<'h>() -> RewriteStrSettings<'h, 'static> {
    RewriteStrSettings::new()
        .with_strict(true)
        .with_enable_esi_tags(false)
}

/// The scripts an element's start tag holds, in order: the element itself
/// when it is a `<script>` (its code still empty), then its event handlers
/// and `javascript:` URLs, each with its attribute's name.
fn scripts(el: &Element<'_, '_>) -> Vec<(Option<String>, Found)> {
    let mut out = Vec::new();
    let script = el.tag_name() == "script";
    if script {
        let found = match el.get_attribute("src") {
            Some(src) => Found::External(decode(&src)),
            None => Found::Inline(String::new()),
        };
        out.push((None, found));
    }
    for attr in el.attributes() {
        let name = attr.name();
        if name.starts_with("on") {
            let code = decode(&attr.value());
            out.push((
                Some(name.clone()),
                Found::Handler {
                    attribute: name,
                    code,
                },
            ));
        } else if URL_ATTRIBUTES.contains(&name.as_str()) && !(script && name == "src") {
            let url = decode(&attr.value());
            if is_javascript(&url) {
                out.push((Some(name), Found::Url(url)));
            }
        }
    }
    out
}

/// Every script in `html`, in document order.
fn find(html: &str) -> Result<Vec<Found>, RewritingError> {
    let found = RefCell::new(Vec::new());
    // The inline script whose code is being read.
    let open = Cell::new(None);
    rewrite_str(
        html,
        settings()
            .append_element_content_handler(element!("*", |el| {
                let mut found = found.borrow_mut();
                for (attribute, script) in scripts(el) {
                    if attribute.is_none() {
                        open.set(matches!(script, Found::Inline(_)).then_some(found.len()));
                    }
                    found.push(script);
                }
                Ok(())
            }))
            .append_element_content_handler(text!("script", |chunk| {
                if let Some(i) = open.get()
                    && let Some(Found::Inline(code)) = found.borrow_mut().get_mut(i)
                {
                    code.push_str(chunk.as_str());
                }
                Ok(())
            })),
    )?;
    Ok(found.into_inner())
}

/// `html` without each script whose `keep` entry, in `find`'s order, is
/// false: a `<script>` element is replaced by a comment, an attribute
/// removed.
fn remove(html: &str, keep: &[bool]) -> Result<String, RewritingError> {
    let mut next = 0;
    rewrite_str(
        html,
        settings().append_element_content_handler(element!("*", |el| {
            let mut denied = Vec::new();
            for (attribute, _) in scripts(el) {
                if !keep.get(next).copied().unwrap_or(false) {
                    denied.push(attribute);
                }
                next += 1;
            }
            for attribute in denied {
                match attribute {
                    Some(name) => el.remove_attribute(&name),
                    None => el.replace(BLOCKED, ContentType::Html),
                }
            }
            Ok(())
        })),
    )
}

/// `'sha256-…'` for script text, its newlines normalized as the browser
/// normalizes them.
fn hash(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let digest = Sha256::digest(text.as_bytes());
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// `url` as the URL parser reads it: without leading and trailing controls
/// and spaces, or any tab or newline.
fn url_text(url: &str) -> String {
    url.trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

fn is_javascript(url: &str) -> bool {
    url_text(url)
        .get(..11)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("javascript:"))
}

/// The policy source that allows the script at `src` and no other file:
/// the URL without its query or fragment, or `'self'` for a relative URL.
/// `None` when no source names it so narrowly: another scheme (`data:`), a
/// host that isn't plain ASCII, credentials, or a path that is empty or ends
/// in `/` (which a source reads as a prefix).
fn source(src: &str) -> Option<String> {
    let src = url_text(src);
    if src.contains('\\') {
        return None;
    }
    let (scheme, rest) = match scheme(&src) {
        Some(scheme)
            if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") =>
        {
            let rest = src[scheme.len() + 1..].strip_prefix("//")?;
            (Some(scheme.to_ascii_lowercase()), rest)
        }
        Some(_) => return None,
        None => match src.strip_prefix("//") {
            Some(rest) => (None, rest),
            None => return Some("'self'".to_string()),
        },
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    let port_ok = port.is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if !host_ok || !port_ok || path.len() <= 1 || path.ends_with('/') {
        return None;
    }
    let mut out = match scheme {
        Some(scheme) => format!("{scheme}://"),
        None => String::new(),
    };
    out.push_str(&host.to_ascii_lowercase());
    if let Some(port) = port {
        out.push(':');
        out.push_str(port);
    }
    out.push_str(&source_path(path));
    Some(out)
}

/// A URL's scheme, if it has one.
fn scheme(url: &str) -> Option<&str> {
    let (scheme, _) = url.split_once(':')?;
    let mut bytes = scheme.bytes();
    (bytes.next()?.is_ascii_alphabetic()
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')))
    .then_some(scheme)
}

/// `path` with every byte a source's path can't hold as written
/// percent-encoded; `;` and `,` would end the source or the policy.
fn source_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => out.push(b as char),
            b'-' | b'.' | b'_' | b'~' | b'!' | b'$' | b'&' | b'(' | b')' | b'*' | b'+' | b'='
            | b':' | b'@' | b'/' => out.push(b as char),
            b'%' if bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit) =>
            {
                out.push('%');
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// An attribute's value with its character references decoded as the
/// browser decodes them: numeric ones with or without `;`, named ones with
/// it.
fn decode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        match reference(rest) {
            Some((text, len)) => {
                out.push_str(&text);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character reference `s` starts with: its text and length.
fn reference(s: &str) -> Option<(String, usize)> {
    let body = s.strip_prefix('&')?;
    if let Some(num) = body.strip_prefix('#') {
        let (radix, digits) = match num.strip_prefix(['x', 'X']) {
            Some(hex) => (16, hex),
            None => (10, num),
        };
        let len = digits
            .bytes()
            .take_while(|b| (*b as char).is_digit(radix))
            .count();
        if len == 0 {
            return None;
        }
        let c = u32::from_str_radix(&digits[..len], radix)
            .ok()
            .and_then(char::from_u32)
            .filter(|&c| c != '\0')
            .unwrap_or('\u{FFFD}');
        let semicolon = usize::from(digits.as_bytes().get(len) == Some(&b';'));
        return Some((c.to_string(), s.len() - digits.len() + len + semicolon));
    }
    let len = body.bytes().take_while(u8::is_ascii_alphanumeric).count();
    if len == 0 || body.as_bytes().get(len) != Some(&b';') {
        return None;
    }
    let name = &s[..len + 2];
    let found = entities::ENTITIES.iter().find(|e| e.entity == name)?;
    Some((found.characters.to_string(), name.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(html: &str) -> Vec<Found> {
        find(html).unwrap()
    }

    #[test]
    fn finds_scripts_handlers_and_javascript_urls() {
        let html = "<p>x <script>a()</script></p>\
            <script SRC=\"https://e.com/x.js?v=1&amp;w=2\">ignored()</script>\
            <img src=x onerror=\"b(&quot;q&quot;)\" title=\"javascript:no\">\
            <a href=\" &#106;ava&#x09;script&colon;c()\">l</a>\
            <svg><script>d()</script><a xlink:href=\"javascript:e()\"></a></svg>\
            <form action=\"JavaScript:f()\"><button formaction=\"https://e.com/\" onclick=g()>";
        assert_eq!(
            found(html),
            vec![
                Found::Inline("a()".to_string()),
                Found::External("https://e.com/x.js?v=1&w=2".to_string()),
                Found::Handler {
                    attribute: "onerror".to_string(),
                    code: "b(\"q\")".to_string()
                },
                Found::Url(" java\tscript:c()".to_string()),
                Found::Inline("d()".to_string()),
                Found::Url("javascript:e()".to_string()),
                Found::Url("JavaScript:f()".to_string()),
                Found::Handler {
                    attribute: "onclick".to_string(),
                    code: "g()".to_string()
                },
            ]
        );
    }

    #[test]
    fn text_that_only_looks_like_a_script_is_not_one() {
        for html in [
            "<!-- <script>a()</script> -->",
            "<textarea><script>a()</script></textarea>",
            "<pre><code>&lt;script&gt;a()&lt;/script&gt;</code></pre>",
            "<style>a::after { content: '<script>a()</script>' }</style>",
            "<p title=\"<script>a()</script>\">x</p>",
        ] {
            assert_eq!(found(html), vec![], "{html}");
        }
    }

    #[test]
    fn denied_scripts_are_taken_out() {
        let html = "<p>a</p><script>x()</script>\
            <img src=\"p.png\" onerror=\"y()\" alt=\"i\">\
            <a href=\"javascript:z()\" title=\"t\">l</a>\
            <script src=\"https://e.com/x.js\"></script>";
        let mut policy = Policy::default();
        let out = policy.filter(html, |_| false);
        assert_eq!(
            out,
            "<p>a</p><!-- script blocked -->\
             <img src=\"p.png\" alt=\"i\">\
             <a title=\"t\">l</a>\
             <!-- script blocked -->"
        );
        assert_eq!(policy.csp(), "script-src 'none'; object-src 'none'");
    }

    #[test]
    fn allowed_scripts_stay_and_are_named_in_the_policy() {
        let html = "<script>x()</script><button onclick=\"y()\">b</button>\
            <script src=\"https://e.com/a.js\"></script><script src=\"https://f.com/b.js\"></script>";
        let mut policy = Policy::default();
        let out = policy.filter(html, |kind| match kind {
            ScriptKind::External(src) => src.starts_with("https://e.com/"),
            _ => true,
        });
        assert_eq!(
            out,
            "<script>x()</script><button onclick=\"y()\">b</button>\
             <script src=\"https://e.com/a.js\"></script><!-- script blocked -->"
        );
        let mut sources = [hash("x()"), hash("y()"), "https://e.com/a.js".to_string()];
        sources.sort();
        assert_eq!(
            policy.csp(),
            format!(
                "script-src 'unsafe-hashes' {}; object-src 'none'",
                sources.join(" ")
            )
        );
    }

    #[test]
    fn removing_a_script_cannot_join_its_neighbors_into_one() {
        let mut policy = Policy::default();
        let out = policy.filter("<<script></script>script>alert(1)</script>", |_| false);
        assert_eq!(out, "<<!-- script blocked -->script>alert(1)</script>");
        assert!(found(&out).is_empty(), "{out}");
    }

    #[test]
    fn ambiguous_markup_is_left_alone_and_allows_nothing() {
        let html = "<select><xmp><script>a()</script></xmp></select>";
        let mut policy = Policy::default();
        assert_eq!(policy.filter(html, |_| true), html);
        assert_eq!(policy.csp(), "script-src 'none'; object-src 'none'");
    }

    #[test]
    fn trusted_markup_is_allowed_whole() {
        let mut policy = Policy::default();
        policy.trust("<script src=\"/app.js\"></script><script>a()</script>");
        policy.allow_source("https://cdn.example/dist/");
        assert_eq!(
            policy.csp(),
            format!(
                "script-src 'self' {} https://cdn.example/dist/; object-src 'none'",
                hash("a()")
            )
        );
    }

    #[test]
    fn javascript_urls_are_hashed_percent_decoded() {
        let mut policy = Policy::default();
        policy.filter("<a href=\" javascript:a(%22b%20c%22)\">l</a>", |_| true);
        assert_eq!(
            policy.csp(),
            format!(
                "script-src 'unsafe-hashes' {}; object-src 'none'",
                hash("javascript:a(\"b c\")")
            )
        );
    }

    #[test]
    fn hashes_match_the_browsers() {
        // `printf '%s' 'hljs.highlightAll();' | openssl dgst -sha256 -binary | base64`
        assert_eq!(
            hash("hljs.highlightAll();"),
            "'sha256-O6piNkhLv4BI/Oje+MccCmgUSrS1sIp+CMaOsvD/VWU='"
        );
        assert_eq!(hash("a\r\nb\rc"), hash("a\nb\nc"));
    }

    #[test]
    fn sources_name_one_file() {
        for (src, want) in [
            ("https://E.com/x.js?v=1#f", Some("https://e.com/x.js")),
            ("http://e.com:8080/a/b.js", Some("http://e.com:8080/a/b.js")),
            ("//e.com/x.js", Some("e.com/x.js")),
            (
                "  https://e.com/x;y,z'.js ",
                Some("https://e.com/x%3By%2Cz%27.js"),
            ),
            ("https://e.com/a%20b.js", Some("https://e.com/a%20b.js")),
            ("/static/x.js", Some("'self'")),
            ("x.js", Some("'self'")),
            ("https://e.com/dir/", None),
            ("https://e.com/", None),
            ("https://e.com", None),
            ("https://user@e.com/x.js", None),
            ("https://e.com\\@f.com/x.js", None),
            ("https://*.e.com/x.js", None),
            ("https://bücher.de/x.js", None),
            ("https://e.com:x/x.js", None),
            ("data:text/javascript,alert(1)", None),
            ("javascript:alert(1)", None),
            ("https:e.com/x.js", None),
        ] {
            assert_eq!(source(src).as_deref(), want, "{src}");
        }
    }

    #[test]
    fn javascript_urls_after_decoding() {
        for (value, want) in [
            ("javascript:a()", true),
            ("  JAVASCRIPT:a()", true),
            ("java\nscript:a()", true),
            ("\u{1}javascript:a()", true),
            ("javascript", false),
            ("https://e.com/javascript:a()", false),
            ("/javascript:a()", false),
        ] {
            assert_eq!(is_javascript(value), want, "{value:?}");
        }
        assert_eq!(
            decode("&#106&#x61;v&#X61;&amp;&lt;&bogus;&#;&"),
            "java&<&bogus;&#;&"
        );
        assert_eq!(
            decode("&#0;&#x110000;&#99999999999;"),
            "\u{FFFD}\u{FFFD}\u{FFFD}"
        );
    }
}
