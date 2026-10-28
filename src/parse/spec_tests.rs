//! Spec measurement: the parse → draw pipeline with `Plain` over the
//! CommonMark 0.31.2 examples and the GFM extension examples, vendored in
//! `tests/spec/`. [`measure`] prints pass/fail per section, with the
//! deliberate differences counted apart, given the pipeline as `render`. A
//! measure, not a gate: an ignored test, run with
//! `cargo test --lib measure_spec -- --ignored --nocapture`.

use std::collections::BTreeMap;

fn spec(file: &str) -> String {
    let path = format!("{}/tests/spec/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn commonmark() -> String {
    spec("commonmark-0.31.2.txt")
}

fn gfm() -> String {
    spec("gfm-0.29.txt")
}

/// The CommonMark sections measured, by their headings in the spec.
const COMMONMARK_SECTIONS: &[&str] = &[
    "Tabs",
    "Thematic breaks",
    "ATX headings",
    "Setext headings",
    "Indented code blocks",
    "Fenced code blocks",
    "HTML blocks",
    "Link reference definitions",
    "Paragraphs",
    "Blank lines",
    "Block quotes",
    "List items",
    "Lists",
    "Backslash escapes",
    "Entity and numeric character references",
    "Code spans",
    "Emphasis and strong emphasis",
    "Links",
    "Images",
    "Autolinks",
    "Raw HTML",
    "Hard line breaks",
    "Soft line breaks",
    "Textual content",
];

/// The GFM extension sections measured.
const GFM_SECTIONS: &[&str] = &[
    "Tables (extension)",
    "Task list items (extension)",
    "Strikethrough (extension)",
    "Autolinks (extension)",
];

struct Example {
    number: usize,
    section: String,
    markdown: String,
    html: String,
}

/// The examples in a spec file, as the spec's own test runner reads them:
/// each belongs to the nearest heading above it, and `→` stands for a tab.
fn examples(spec: &str) -> Vec<Example> {
    let fence = "`".repeat(32);
    let mut out = Vec::new();
    let mut section = String::new();
    let mut lines = spec.lines();
    while let Some(line) = lines.next() {
        if line.starts_with(&format!("{fence} example")) {
            let (mut markdown, mut html) = (String::new(), String::new());
            let mut in_html = false;
            for line in lines.by_ref() {
                if line == fence {
                    break;
                }
                if line == "." && !in_html {
                    in_html = true;
                    continue;
                }
                let target = if in_html { &mut html } else { &mut markdown };
                target.push_str(&line.replace('→', "\t"));
                target.push('\n');
            }
            out.push(Example {
                number: out.len() + 1,
                section: section.clone(),
                markdown,
                html,
            });
        } else if let Some(heading) = heading(line) {
            section = heading.to_string();
        }
    }
    out
}

fn heading(line: &str) -> Option<&str> {
    let text = line.trim_start_matches('#');
    (text.len() < line.len()).then_some(())?;
    text.strip_prefix(' ').map(str::trim)
}

#[derive(Default)]
struct Tally {
    pass: usize,
    fail: usize,
    /// (a) soft breaks drawn as hard breaks.
    soft_breaks: usize,
    /// (b) HTML blocks ending at their closing tag.
    html_blocks: usize,
    /// (c) host blocks interrupting paragraphs; never with `Plain`.
    host_blocks: usize,
    /// (d) wiki behaviour: link and image destinations resolved as wiki
    /// paths, `#word` drawn as a tag.
    wiki: usize,
    /// (e) frontmatter: a document that starts with a `---` line and has
    /// another later is read as starting with frontmatter.
    frontmatter: usize,
    /// (f) GFM autolinks: bare URLs and email addresses linked, as GFM's
    /// autolink extension (and today's renderer) does.
    gfm_autolinks: usize,
    /// (g) task markup: GitHub's task-list classes and Obsidian's
    /// `data-task` on task lists, items and checkboxes.
    task_markup: usize,
}

/// Measures `sections` of `spec` through `render`, printing one row per
/// section and the failing example numbers.
fn measure(name: &str, spec: &str, sections: &[&str], render: impl Fn(&str) -> String) {
    let mut tallies: BTreeMap<usize, (&str, Tally)> = BTreeMap::new();
    let mut failing: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for ex in examples(spec) {
        let Some(order) = sections.iter().position(|s| *s == ex.section) else {
            continue;
        };
        let section = sections[order];
        let tally = &mut tallies
            .entry(order)
            .or_insert((section, Tally::default()))
            .1;
        let got = normalize(&render(&ex.markdown));
        if got == normalize(&ex.html) {
            tally.pass += 1;
        } else if got == normalize(&soft_breaks_as_hard(&ex.html)) {
            tally.soft_breaks += 1;
        } else if starts_with_frontmatter(&ex.markdown) {
            tally.frontmatter += 1;
        } else if unlink_autolinks(&got) == normalize(&ex.html) {
            tally.gfm_autolinks += 1;
        } else if strip_task_markup(&got) == normalize(&ex.html) {
            tally.task_markup += 1;
        } else if opens_html_block(&ex.markdown) {
            tally.html_blocks += 1;
        } else if has_tag(&ex.markdown) || has_wikilink(&ex.markdown) || has_wiki_url(&ex.html) {
            tally.wiki += 1;
        } else {
            tally.fail += 1;
            failing.entry(section).or_default().push(ex.number);
        }
    }
    println!("\n{name}\n");
    println!(
        "| Section | Pass | Fail | (a) soft breaks | (b) HTML blocks | (c) host blocks | (d) wiki | (e) frontmatter | (f) GFM autolinks | (g) task markup |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    let mut total = Tally::default();
    for (section, t) in tallies.values() {
        println!(
            "| {section} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            t.pass,
            t.fail,
            t.soft_breaks,
            t.html_blocks,
            t.host_blocks,
            t.wiki,
            t.frontmatter,
            t.gfm_autolinks,
            t.task_markup
        );
        total.pass += t.pass;
        total.fail += t.fail;
        total.soft_breaks += t.soft_breaks;
        total.html_blocks += t.html_blocks;
        total.host_blocks += t.host_blocks;
        total.wiki += t.wiki;
        total.frontmatter += t.frontmatter;
        total.gfm_autolinks += t.gfm_autolinks;
        total.task_markup += t.task_markup;
    }
    println!(
        "| **Total** | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        total.pass,
        total.fail,
        total.soft_breaks,
        total.html_blocks,
        total.host_blocks,
        total.wiki,
        total.frontmatter,
        total.gfm_autolinks,
        total.task_markup
    );
    for (section, numbers) in failing {
        println!("failing in {section}: {numbers:?}");
    }
}

/// `html` with soft breaks drawn as hard ones, the way Obsidian-style line
/// breaks draw them: each newline inside a paragraph's or heading's text,
/// and each newline between text inside a list item (a tight item's
/// paragraph), gets a `<br />`.
fn soft_breaks_as_hard(html: &str) -> String {
    let html = breaks_in(html, &["p", "h1", "h2", "h3", "h4", "h5", "h6"], false);
    breaks_in(&html, &["li"], true)
}

/// `html` with a `<br />` before each newline inside the elements named;
/// with `between_text`, only before newlines with text on both sides.
fn breaks_in(html: &str, names: &[&str], between_text: bool) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let open = names
            .iter()
            .filter_map(|n| Some((rest.find(&format!("<{n}>"))?, *n)))
            .min();
        let Some((start, name)) = open else {
            break;
        };
        let close = format!("</{name}>");
        let Some(len) = rest[start..].find(&close) else {
            break;
        };
        out.push_str(&rest[..start]);
        let element = &rest[start..start + len];
        let mut lines = element.split('\n').peekable();
        while let Some(line) = lines.next() {
            out.push_str(line);
            if let Some(next) = lines.peek() {
                let text = !line.ends_with('>') && !next.starts_with('<');
                if !line.ends_with("<br />") && (!between_text || text) {
                    out.push_str("<br />");
                }
                out.push('\n');
            }
        }
        rest = &rest[start + len..];
    }
    out.push_str(rest);
    out
}

/// `html` (normalized) with each link whose text is its own destination
/// (less an added `http://` or `mailto:`) replaced by that text: GFM's
/// extended autolinks undone. Classifies difference (f).
fn unlink_autolinks(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<a href=\"") {
        let after = &rest[start + "<a href=\"".len()..];
        let link = after.split_once("\">").and_then(|(href, after)| {
            let (text, after) = after.split_once("</a>")?;
            let bare = href
                .strip_prefix("mailto:")
                .or_else(|| {
                    href.strip_prefix("http://")
                        .filter(|h| h.starts_with("www."))
                })
                .unwrap_or(href);
            (text == bare || text == href).then_some((text, after))
        });
        match link {
            Some((text, after)) => {
                out.push_str(&rest[..start]);
                out.push_str(text);
                rest = after;
            }
            None => {
                out.push_str(&rest[..start + 1]);
                rest = &rest[start + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `html` (normalized) without the task-list classes and `data-task`
/// attributes, and with a space after each checkbox. Classifies
/// difference (g).
fn strip_task_markup(html: &str) -> String {
    let mut out = html.to_string();
    for class in [
        "contains-task-list",
        "task-list-item",
        "task-list-item-checkbox",
    ] {
        out = out.replace(&format!(" class=\"{class}\""), "");
    }
    while let Some(start) = out.find(" data-task=\"") {
        let value = start + " data-task=\"".len();
        let end = out[value..].find('"').map_or(out.len(), |i| value + i + 1);
        out.replace_range(start..end, "");
    }
    let mut from = 0;
    while let Some(i) = out[from..].find("<input") {
        let Some(end) = out[from + i..].find('>').map(|e| from + i + e + 1) else {
            break;
        };
        if !out[end..].starts_with([' ', '\n']) {
            out.insert(end, ' ');
        }
        from = end;
    }
    out
}

/// True when `markdown` has `[[…]]`, which the wiki reads as a wikilink. A
/// heuristic for difference (d).
fn has_wikilink(markdown: &str) -> bool {
    markdown
        .split_once("[[")
        .is_some_and(|(_, after)| after.contains("]]"))
}

/// True when `markdown` starts with a `---` line and has another later, so
/// the parser reads frontmatter there. Classifies difference (e).
fn starts_with_frontmatter(markdown: &str) -> bool {
    let mut lines = markdown.lines();
    let fence = |l: &str| l.trim_end() == "---";
    lines.next().is_some_and(fence) && lines.any(fence)
}

/// True when a line of `markdown` opens a CommonMark HTML block of type 6
/// (a block-level tag name) or 7 (a complete open or closing tag alone on
/// its line): the blocks that now end at their closing tag. A heuristic for
/// classifying difference (b); it looks through `>` prefixes and
/// indentation only.
fn opens_html_block(markdown: &str) -> bool {
    markdown.lines().any(|line| {
        let line = line.trim_start_matches(['>', ' ', '\t']);
        let Some(rest) = line.strip_prefix('<') else {
            return false;
        };
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        let len = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .unwrap_or(rest.len());
        let (name, after) = (rest[..len].to_ascii_lowercase(), &rest[len..]);
        if name.is_empty() {
            return false;
        }
        let type_6 = BLOCK_NAMES.contains(&name.as_str())
            && (after.is_empty() || after.starts_with([' ', '\t', '>']) || after.starts_with("/>"));
        let type_7 = !["pre", "script", "style", "textarea"].contains(&name.as_str())
            && after
                .find('>')
                .is_some_and(|end| after[end + 1..].trim().is_empty());
        type_6 || type_7
    })
}

/// True when `markdown` has a `#word` the wiki draws as a tag: after the
/// start or whitespace, not all digits. A heuristic for difference (d).
fn has_tag(markdown: &str) -> bool {
    let mut after_space = true;
    for (i, c) in markdown.char_indices() {
        if c == '#' && after_space {
            let body = &markdown[i + 1..];
            let len = body
                .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
                .unwrap_or(body.len());
            if len > 0 && !body[..len].chars().all(char::is_numeric) {
                return true;
            }
        }
        after_space = c.is_whitespace();
    }
    false
}

/// True when the expected `html` has an `href` or `src` the wiki rewrites:
/// a path, not a fragment, query, protocol-relative URL or one with a scheme.
/// A heuristic for difference (d).
fn has_wiki_url(html: &str) -> bool {
    ["href=\"", "src=\""].iter().any(|attr| {
        html.match_indices(attr).any(|(i, _)| {
            let url = &html[i + attr.len()..];
            let url = &url[..url.find('"').unwrap_or(url.len())];
            let scheme = url.find(':').is_some_and(|colon| {
                colon > 0
                    && url[..colon]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
            });
            !(url.is_empty() || url.starts_with(['#', '?']) || url.starts_with("//") || scheme)
        })
    })
}

/// The tag names that start a type 6 HTML block.
const BLOCK_NAMES: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

/// HTML normalized as the CommonMark spec's `test/normalize.py` does:
/// whitespace runs collapsed outside `<pre>`, whitespace around block-level
/// tags removed, attributes sorted, self-closing tags opened, and character
/// references decoded except `<`, `>`, `&` and `"`. Named references other
/// than those four are kept as written (the Python version decodes HTML 4's
/// names); both sides go through this, so that only matters when one side
/// writes a name and the other the character.
fn normalize(html: &str) -> String {
    let mut n = Normalizer::default();
    let mut rest = html;
    while !rest.is_empty() {
        let len = if rest.starts_with("<![CDATA[") {
            rest.find("]]>").map_or(rest.len(), |i| i + 3)
        } else if rest.starts_with('<') {
            rest.find('>').map_or(rest.len(), |i| i + 1)
        } else {
            rest.find('<').unwrap_or(rest.len())
        };
        let chunk = &rest[..len];
        if chunk.starts_with("<![CDATA[") {
            n.output.push_str(chunk);
        } else if chunk.starts_with('<') {
            n.markup(chunk);
        } else {
            n.text(chunk);
        }
        rest = &rest[len..];
    }
    n.output
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Last {
    StartTag,
    EndTag,
    Data,
    Other,
}

struct Normalizer {
    output: String,
    last: Last,
    last_tag: String,
    in_pre: bool,
}

impl Default for Normalizer {
    fn default() -> Self {
        Normalizer {
            output: String::new(),
            last: Last::StartTag,
            last_tag: String::new(),
            in_pre: false,
        }
    }
}

const BLOCK_TAGS: &[&str] = &[
    "article",
    "header",
    "aside",
    "hgroup",
    "blockquote",
    "hr",
    "iframe",
    "body",
    "li",
    "map",
    "button",
    "object",
    "canvas",
    "ol",
    "caption",
    "output",
    "col",
    "p",
    "colgroup",
    "pre",
    "dd",
    "progress",
    "div",
    "section",
    "dl",
    "table",
    "td",
    "dt",
    "tbody",
    "embed",
    "textarea",
    "fieldset",
    "tfoot",
    "figcaption",
    "th",
    "figure",
    "thead",
    "footer",
    "tr",
    "form",
    "ul",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "video",
    "script",
    "style",
];

fn is_block(tag: &str) -> bool {
    BLOCK_TAGS.contains(&tag)
}

impl Normalizer {
    fn markup(&mut self, chunk: &str) {
        let inner = &chunk[1..chunk.len().saturating_sub(1).max(1)];
        if let Some(comment) = chunk.strip_prefix("<!--") {
            self.output.push_str("<!--");
            self.output.push_str(comment);
            self.last = Last::Other;
        } else if inner.starts_with('!') || inner.starts_with('?') {
            self.output.push_str(chunk);
            self.last = Last::Other;
        } else if let Some(name) = inner.strip_prefix('/') {
            self.end_tag(&name.trim().to_ascii_lowercase());
        } else if inner.starts_with(|c: char| c.is_ascii_alphabetic()) {
            let self_closing = inner.ends_with('/');
            let body = inner.strip_suffix('/').unwrap_or(inner);
            let name_len = body
                .find(|c: char| c.is_whitespace() || c == '/')
                .unwrap_or(body.len());
            let name = body[..name_len].to_ascii_lowercase();
            let attrs = attributes(&body[name_len..]);
            self.start_tag(&name, attrs);
            if self_closing {
                self.last = Last::EndTag;
            }
        } else {
            // Not a tag to an HTML parser: text.
            self.text(chunk);
        }
    }

    fn start_tag(&mut self, name: &str, mut attrs: Vec<(String, Option<String>)>) {
        if name == "pre" {
            self.in_pre = true;
        }
        if is_block(name) {
            self.trim_end();
        }
        self.output.push('<');
        self.output.push_str(name);
        attrs.sort();
        for (key, value) in attrs {
            self.output.push(' ');
            self.output.push_str(&key);
            if let Some(value) = value {
                self.output.push_str("=\"");
                self.output.push_str(&escape(&decode(&value), true));
                self.output.push('"');
            }
        }
        self.output.push('>');
        self.last_tag = name.to_string();
        self.last = Last::StartTag;
    }

    fn end_tag(&mut self, name: &str) {
        if name == "pre" {
            self.in_pre = false;
        } else if is_block(name) {
            self.trim_end();
        }
        self.output.push_str("</");
        self.output.push_str(name);
        self.output.push('>');
        self.last_tag = name.to_string();
        self.last = Last::EndTag;
    }

    fn text(&mut self, data: &str) {
        let after_tag = matches!(self.last, Last::StartTag | Last::EndTag);
        let after_block_tag = after_tag && is_block(&self.last_tag);
        let mut data = escape(&decode(data), false);
        if after_tag && self.last_tag == "br" {
            data = data.trim_start_matches('\n').to_string();
        }
        if !self.in_pre {
            data = collapse(&data);
            if after_block_tag {
                data = match self.last {
                    Last::StartTag => data.trim_start().to_string(),
                    _ => data.trim().to_string(),
                };
            }
        }
        self.output.push_str(&data);
        self.last = Last::Data;
    }

    fn trim_end(&mut self) {
        let len = self.output.trim_end().len();
        self.output.truncate(len);
    }
}

/// An open tag's attributes, names lowercased, values unquoted.
fn attributes(mut s: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start();
        if s.is_empty() {
            return out;
        }
        let len = s
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(s.len());
        let key = s[..len].to_ascii_lowercase();
        s = s[len..].trim_start();
        let Some(after) = s.strip_prefix('=') else {
            out.push((key, None));
            continue;
        };
        s = after.trim_start();
        let value = match s.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let end = s[1..].find(q).map_or(s.len(), |i| i + 1);
                let value = &s[1..end];
                s = &s[(end + 1).min(s.len())..];
                value
            }
            _ => {
                let end = s.find(char::is_whitespace).unwrap_or(s.len());
                let value = &s[..end];
                s = &s[end..];
                value
            }
        };
        out.push((key, Some(value.to_string())));
    }
}

/// Decodes numeric character references and `&amp;`, `&lt;`, `&gt;`,
/// `&quot;`; other named references stay as written.
fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e <= 33) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let c = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            _ => name.strip_prefix('#').and_then(|num| {
                let n = match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse().ok(),
                };
                n.map(|n| char::from_u32(n).unwrap_or('\u{FFFD}'))
            }),
        };
        match c {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Escapes `&`, `<`, `>`, `"`, and in attributes `'`, as Python's
/// `html.escape` does. Named references kept by `decode` keep their `&`.
fn escape(s: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' if attribute => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

#[test]
#[ignore = "a measure, not a gate"]
fn measure_spec() {
    let render = |markdown: &str| {
        let doc = crate::parse::parse(&crate::parse::Plain, markdown);
        crate::draw::draw_blocks(&doc.tree, &doc.body, &crate::draw::DrawCx::default())
            .into_string()
    };
    measure(
        "CommonMark 0.31.2",
        &commonmark(),
        COMMONMARK_SECTIONS,
        render,
    );
    measure("GFM 0.29 extensions", &gfm(), GFM_SECTIONS, render);
}

#[test]
fn spec_examples_load() {
    let measured = |spec: &str, sections: &[&str]| {
        examples(spec)
            .iter()
            .filter(|ex| sections.contains(&ex.section.as_str()))
            .count()
    };
    assert_eq!(measured(&commonmark(), COMMONMARK_SECTIONS), 650);
    assert_eq!(measured(&gfm(), GFM_SECTIONS), 23);
}

#[test]
fn normalizer_matches_normalize_py() {
    for (html, want) in [
        ("<p>a  \t b</p>", "<p>a b</p>"),
        ("\n\t<p>\n\t\ta  b\t\t</p>\n\t", "<p>a b</p>"),
        ("<i>a  b</i> ", "<i>a b</i> "),
        ("<br />", "<br>"),
        (
            "<a title=\"bar\" HREF=\"foo\">x</a>",
            "<a href=\"foo\" title=\"bar\">x</a>",
        ),
        ("&amp;&gt;&lt;&quot;&#x41;", "&amp;&gt;&lt;&quot;A"),
        ("<pre>a  \n b</pre>", "<pre>a  \n b</pre>"),
    ] {
        assert_eq!(normalize(html), want, "{html:?}");
    }
    assert_eq!(
        soft_breaks_as_hard("<p>a\nb<br />\nc</p>\n<pre>x\ny</pre>"),
        "<p>a<br />\nb<br />\nc</p>\n<pre>x\ny</pre>"
    );
    assert!(starts_with_frontmatter("---\na\n---\n") && !starts_with_frontmatter("a\n---\n---\n"));
    assert_eq!(
        soft_breaks_as_hard("<h1>a\nb</h1>\n<li>c\n<ul>\n<li>d\ne</li>"),
        "<h1>a<br />\nb</h1>\n<li>c\n<ul>\n<li>d<br />\ne</li>"
    );
    assert_eq!(
        unlink_autolinks(
            "<p>&lt; <a href=\"https://e.org\">https://e.org</a> <a href=\"x\">y</a></p>"
        ),
        "<p>&lt; https://e.org <a href=\"x\">y</a></p>"
    );
    assert!(has_wikilink("[[a]]") && !has_wikilink("[a]"));
    assert!(has_tag("x #tag") && !has_tag("# heading") && !has_tag("#5 bolt"));
    assert!(has_wiki_url("<a href=\"/url\">") && !has_wiki_url("<a href=\"https://e\">"));
}
