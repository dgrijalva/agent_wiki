//! List everything in agent_wiki's docs that is waiting on a decision.
//!
//! Read-only. Scans the standing docs for bracketed markers and the intake
//! files in docs/working/ for items awaiting a person, and reports them grouped by
//! file and section. Software instead of tokens: an agent or a person can ask
//! "what's pending in X" without reading the docs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;

use clap::Parser;
use regex::Regex;
use serde::Serialize;

const SKIP_DIRS: &[&str] = &[".git", ".claude", "tools", "target", "assets", "reference"];

static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.*?)\s*$").unwrap());
static MARKER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(OPEN|PROVISIONAL):").unwrap());
static MARKER_TAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[(OPEN|PROVISIONAL):.*").unwrap());
static STRIKE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"~~.*?~~").unwrap());
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
static LINK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap());
static INTAKE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^docs/working/intake-.*\.md$").unwrap());
// Matches preceded by `[` are dropped in `lint` (the regex crate has no lookbehind).
static LINT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(Open:|Open —|Proposed[a-z ]*:|Proposal —|still needs? (design review|confirmation)|still needs? to verify|remains? open|is open below)",
    )
    .unwrap()
});

/// List everything in agent_wiki's docs that is waiting on a decision.
///
/// Markers (docs/process.md): [OPEN: ...], [PROVISIONAL: ...].
/// Brackets are matched with nesting, so markdown links inside a marker are fine.
///
/// Usage:
///   pending                      everything, as markdown
///   pending docs/design          one path (or any path prefix)
///   pending --kind OPEN          only one marker kind (repeatable)
///   pending --section Wikilinks  only sections whose heading path matches
///   pending --json               machine-readable
///   pending --lint               report unbracketed text that reads as pending
///                                (unmarked prose is supposed to be decided)
#[derive(Parser)]
#[command(verbatim_doc_comment)]
struct Args {
    #[arg(default_value = "")]
    prefix: String,
    #[arg(long, value_parser = ["OPEN", "PROVISIONAL", "INTAKE"])]
    kind: Vec<String>,
    #[arg(long)]
    section: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    lint: bool,
}

#[derive(Serialize)]
struct Item {
    kind: String,
    file: String,
    line: usize,
    section: String,
    text: String,
}

struct LintHit {
    file: String,
    line: usize,
    matched: String,
    text: String,
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn md_files(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if path.is_dir() {
                // Symlinked directories are listed but not followed, as with os.walk.
                if !SKIP_DIRS.contains(&name.as_str()) && !entry.file_type().unwrap().is_symlink() {
                    dirs.push(name);
                }
            } else {
                files.push(name);
            }
        }
        files.sort();
        dirs.sort();
        for f in files.iter().filter(|f| f.ends_with(".md")) {
            let rel = dir.join(f);
            let rel = rel.strip_prefix(root).unwrap();
            out.push(
                rel.components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
        for d in dirs {
            walk(root, &dir.join(d), out);
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path))
        .unwrap()
        .replace("\r\n", "\n")
}

fn strip_md(s: &str) -> String {
    let s = TAG_RE.replace_all(s, "");
    let s = LINK_RE.replace_all(&s, "${1}");
    s.replace("**", "").replace("~~", "").trim().to_string()
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Return the bracketed span starting at `text[start] == '['`, honoring nesting.
fn extract(text: &str, start: usize) -> &str {
    let mut depth = 0;
    for (i, c) in text[start..].char_indices() {
        if c == '[' {
            depth += 1;
        } else if c == ']' {
            depth -= 1;
            if depth == 0 {
                return &text[start..start + i + 1];
            }
        }
    }
    &text[start..]
}

fn scan_markers(root: &Path, path: &str) -> Vec<Item> {
    let text = read(root, path);
    let mut offsets = Vec::new();
    let mut heading_at = Vec::new();
    let mut heads: Vec<(usize, String)> = Vec::new();
    let mut pos = 0;
    for line in text.split('\n') {
        offsets.push(pos);
        pos += line.len() + 1;
        if let Some(m) = HEADING_RE.captures(line) {
            let level = m[1].len();
            heads.retain(|h| h.0 < level);
            heads.push((level, strip_md(&m[2])));
        }
        heading_at.push(
            heads
                .iter()
                .map(|h| h.1.as_str())
                .collect::<Vec<_>>()
                .join(" > "),
        );
    }
    let mut items = Vec::new();
    for m in MARKER_RE.captures_iter(&text) {
        let whole = m.get(0).unwrap();
        if text[..whole.start()].ends_with('`') {
            continue; // a marker named in prose, e.g. `[OPEN: …]`
        }
        let kind = &m[1];
        let span = extract(&text, whole.start());
        let inner = &span[kind.len() + 2..];
        let inner = match inner.char_indices().last() {
            Some((i, _)) => &inner[..i],
            None => inner,
        };
        let line_no = offsets.partition_point(|&o| o <= whole.start()) - 1;
        items.push(Item {
            kind: kind.to_string(),
            file: path.to_string(),
            line: line_no + 1,
            section: heading_at[line_no].clone(),
            text: strip_md(inner),
        });
    }
    items
}

fn scan_intake(root: &Path, path: &str) -> Vec<Item> {
    let mut items = Vec::new();
    let mut group = String::new();
    for (n, line) in read(root, path).split('\n').enumerate() {
        let Some(m) = HEADING_RE.captures(line) else {
            continue;
        };
        let title = strip_md(&m[2]);
        match m[1].len() {
            2 => group = title,
            3 => items.push(Item {
                kind: "INTAKE".to_string(),
                file: path.to_string(),
                line: n + 1,
                section: group.clone(),
                text: title,
            }),
            _ => {}
        }
    }
    items
}

fn lint(root: &Path, path: &str) -> Vec<LintHit> {
    let mut out = Vec::new();
    for (n, line) in read(root, path).split('\n').enumerate() {
        let clean = MARKER_TAIL_RE.replace(line, "");
        let clean = STRIKE_RE.replace_all(&clean, "");
        let hit = LINT_RE
            .find_iter(&clean)
            .find(|m| !clean[..m.start()].ends_with('['));
        if let Some(m) = hit {
            out.push(LintHit {
                file: path.to_string(),
                line: n + 1,
                matched: m.as_str().to_string(),
                text: truncate(&strip_md(line), 160),
            });
        }
    }
    out
}

fn main() -> ExitCode {
    let args = Args::parse();
    let root = root();

    let files: Vec<String> = md_files(&root)
        .into_iter()
        .filter(|f| f.starts_with(&args.prefix))
        .collect();
    let content: Vec<&String> = files
        .iter()
        .filter(|f| !f.starts_with("docs/working/") && !f.starts_with("docs/plans/"))
        .filter(|f| {
            !["glossary.md", "process.md", "SKILL.md"]
                .iter()
                .any(|s| f.ends_with(s))
        })
        .collect();

    if args.lint {
        let found: Vec<LintHit> = content.iter().flat_map(|f| lint(&root, f)).collect();
        for x in &found {
            println!("{}:{}: '{}': {}", x.file, x.line, x.matched, x.text);
        }
        eprintln!("{} unmarked pending-looking lines", found.len());
        return if found.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    let mut items: Vec<Item> = content
        .iter()
        .flat_map(|f| scan_markers(&root, f))
        .collect();
    items.extend(
        files
            .iter()
            .filter(|f| INTAKE_RE.is_match(f))
            .flat_map(|f| scan_intake(&root, f)),
    );
    if !args.kind.is_empty() {
        items.retain(|x| args.kind.contains(&x.kind));
    }
    if let Some(section) = &args.section {
        let section = section.to_lowercase();
        items.retain(|x| x.section.to_lowercase().contains(&section));
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&items).unwrap());
        return ExitCode::SUCCESS;
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for x in &items {
        *counts.entry(&x.kind).or_default() += 1;
    }
    println!("# Pending\n");
    if counts.is_empty() {
        println!("nothing pending");
    } else {
        println!(
            "{}",
            counts
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let mut cur_file: Option<&str> = None;
    let mut cur_sec: Option<&str> = None;
    for x in &items {
        if cur_file != Some(&x.file) {
            cur_file = Some(&x.file);
            cur_sec = None;
            println!("\n## {}", x.file);
        }
        if cur_sec != Some(&x.section) {
            cur_sec = Some(&x.section);
            println!(
                "\n**{}**",
                if x.section.is_empty() {
                    "(top)"
                } else {
                    &x.section
                }
            );
        }
        let text = if x.text.chars().count() <= 200 {
            x.text.clone()
        } else {
            truncate(&x.text, 197) + "..."
        };
        println!("- `{}` L{}: {}", x.kind, x.line, text);
    }
    ExitCode::SUCCESS
}
