//! The MCP server: the wiki's tools, the adapter that turns any [`Tool`]
//! into an rmcp route, and the endpoint the router mounts at
//! `{base}/_wiki/mcp`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::future::Future;
use std::sync::Arc;

use anyhow::{Result, bail};
use rmcp::handler::server::common::schema_for_input;
use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::data::Folder;
use crate::document::{DocumentTree, OutlineHeading};
use crate::index::{FsKind, Resolution};
use crate::names::{LinkTarget, path_to_url};
use crate::parse::{Host, parse};
use crate::reference;
use crate::{Plain, Wiki};

/// A tool an agent can call. Implementors hold whatever they need (the
/// built-in tools hold a [`Wiki`]); [`route`] adapts one to rmcp.
pub trait Tool: Send + Sync + 'static {
    /// The arguments, deserialized from the call's JSON; their schema is
    /// the tool's input schema.
    type Args: DeserializeOwned + JsonSchema + Send + 'static;
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    /// The result as text for the agent. An error is returned to the agent
    /// as an error result, not a protocol error.
    fn call(&self, args: Self::Args) -> impl Future<Output = Result<String>> + Send;
}

/// `tool` as a route for any rmcp server type `S`. The route holds the tool;
/// it needs nothing from the server.
pub fn route<S: Send + Sync + 'static, T: Tool>(tool: T) -> ToolRoute<S> {
    let schema = schema_for_input::<T::Args>()
        .unwrap_or_else(|e| panic!("the arguments of tool `{}`: {e}", T::NAME));
    let attr = rmcp::model::Tool::new(T::NAME, T::DESCRIPTION, schema);
    let tool = Arc::new(tool);
    ToolRoute::new_dyn(attr, move |cx: ToolCallContext<'_, S>| {
        let tool = tool.clone();
        let args = Value::Object(cx.arguments.clone().unwrap_or_default());
        let origin = cx
            .request_context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(origin_of);
        Box::pin(async move {
            let args: T::Args = serde_json::from_value(args)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let result = match ORIGIN.scope(origin, tool.call(args)).await {
                Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
                Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))]),
            };
            Ok(CallToolResponse::from(result))
        })
    })
}

tokio::task_local! {
    /// The origin (`scheme://host`) of the MCP request a tool is running
    /// for; see [`route`].
    static ORIGIN: Option<String>;
}

/// `scheme://host` of an HTTP request: its `Host` header, with
/// `X-Forwarded-Proto` or else `http`.
fn origin_of(parts: &axum::http::request::Parts) -> Option<String> {
    let host = parts.headers.get(axum::http::header::HOST)?.to_str().ok()?;
    let scheme = parts
        .headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    Some(format!("{scheme}://{host}"))
}

/// The origin of the MCP request the current tool call is for, if it came
/// through [`route`] over HTTP.
fn request_origin() -> Option<String> {
    ORIGIN.try_with(Clone::clone).ok().flatten()
}

impl<P: Host> Wiki<P> {
    /// The default tool collection, for an rmcp server of type `S`; edit it
    /// or merge it into a host's own server.
    pub fn tools<S: Send + Sync + 'static>(&self) -> ToolRouter<S> {
        ToolRouter::new()
            .with_route(route(Search::new(self.clone())))
            .with_route(route(Overview::new(self.clone())))
            .with_route(route(ReadDocument::new(self.clone())))
            .with_route(route(OpenFiles::new(self.clone())))
            .with_route(route(PageUrl::new(self.clone())))
    }
}

/// The server the router mounts at `{base}/_wiki/mcp`: the tools set with
/// [`WikiBuilder::tools`](crate::WikiBuilder::tools), else the default ones.
pub struct McpServer<P: Host> {
    tools: ToolRouter<McpServer<P>>,
    instructions: Arc<str>,
}

impl<P: Host> ServerHandler for McpServer<P> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("agent_wiki", env!("CARGO_PKG_VERSION")))
            .with_instructions(&*self.instructions)
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.list_all()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.tools
            .call(ToolCallContext::new(self, request, context))
            .await
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        self.tools.get(name).cloned()
    }
}

/// What the agent is told once, on connecting: where the wiki root is,
/// since results give paths relative to it, and where the reference is,
/// which says the rest.
fn instructions(root: Option<&std::path::Path>) -> String {
    let root = root.map_or_else(String::new, |root| {
        format!(" The wiki root is `{}`;", root.display())
    });
    format!(
        "A wiki of markdown files.{root} paths in results are relative to the wiki root. \
         How to use this wiki and its tools is in its reference: read it with `read_document` \
         at path `{}`.",
        reference::PATH
    )
}

/// What a reference path's results are prefixed with.
fn reference_prefix() -> String {
    format!("{}/", reference::PATH)
}

impl<P: Host> Wiki<P> {
    /// The reference wiki and the rest of `path` in it, if `path` (relative
    /// to the wiki root) is in the reference.
    fn in_reference<'a>(&'a self, path: &'a str) -> Option<(&'a Wiki<Plain>, &'a str)> {
        let reference = self.inner.reference.as_ref()?;
        Some((reference, reference::within(path)?))
    }
}

/// Makes the collection the endpoint serves; see
/// [`WikiBuilder::tools`](crate::WikiBuilder::tools).
pub(crate) type ToolsFn<P> = Arc<dyn Fn(&Wiki<P>) -> ToolRouter<McpServer<P>> + Send + Sync>;

/// The HTTP endpoint for the wiki's tools, with no sessions.
pub(crate) fn service<P: Host>(
    wiki: Wiki<P>,
) -> StreamableHttpService<McpServer<P>, NeverSessionManager> {
    let mut config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    // The router's own check covers the endpoint when it is set.
    if wiki.inner.allowed_hosts.is_some() {
        config = config.disable_allowed_hosts();
    }
    let instructions: Arc<str> = instructions(wiki.inner.root.disk()).into();
    StreamableHttpService::new(
        move || {
            Ok(McpServer {
                tools: (wiki.inner.tools)(&wiki),
                instructions: instructions.clone(),
            })
        },
        Arc::new(NeverSessionManager::default()),
        config,
    )
}

/// Searches titles, tags and body text, as the web view's search does.
pub struct Search<P: Host> {
    wiki: Wiki<P>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Words to search for.
    pub query: String,
    /// Only documents under this folder, relative to the wiki root. The
    /// reference is only searched when this is in it.
    pub folder: Option<String>,
}

impl<P: Host> Search<P> {
    pub fn new(wiki: Wiki<P>) -> Self {
        Search { wiki }
    }
}

impl<P: Host> Tool for Search<P> {
    type Args = SearchArgs;
    const NAME: &'static str = "search";
    const DESCRIPTION: &'static str = "Search the wiki's documents by title, tag and body text. \
Results are ranked title matches, then tag matches, then body matches. Each result is the \
document's path, title and tags, then its summary.";

    async fn call(&self, args: SearchArgs) -> Result<String> {
        let folder = args.folder.as_deref().unwrap_or("");
        match self.wiki.in_reference(folder) {
            Some((reference, rest)) => {
                search(reference, &reference_prefix(), &args.query, rest).await
            }
            None => search(&self.wiki, "", &args.query, folder).await,
        }
    }
}

/// `wiki`'s documents under `folder` matching `query`, with each path
/// given as `prefix` and the path in `wiki`.
async fn search<P: Host>(
    wiki: &Wiki<P>,
    prefix: &str,
    query: &str,
    folder: &str,
) -> Result<String> {
    let index = &wiki.inner.index;
    let folder = Some(folder.trim_matches('/'))
        .filter(|f| !f.is_empty())
        .map(|f| format!("{f}/"));
    let pages: Vec<_> = index
        .search(query)
        .await?
        .into_iter()
        .filter(|p| folder.as_ref().is_none_or(|f| p.path.starts_with(f)))
        .collect();
    if pages.is_empty() {
        return Ok("No matches.".to_string());
    }
    let paths: Vec<String> = pages.iter().map(|p| p.path.clone()).collect();
    let details = index.details(&paths).await?;
    let mut out = String::new();
    for page in &pages {
        write!(out, "{prefix}{} — {}", page.path, page.title)?;
        let details = details.get(&page.path);
        if let Some(details) = details {
            for tag in &details.tags {
                write!(out, "  #{tag}")?;
            }
        }
        out.push('\n');
        if let Some(details) = details.filter(|d| !d.summary.is_empty()) {
            writeln!(out, "  {}", details.summary)?;
        }
    }
    Ok(out)
}

/// A document's shape without its full text: title, summary, frontmatter,
/// outline with line numbers, and the documents it links to.
pub struct Overview<P: Host> {
    wiki: Wiki<P>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OverviewArgs {
    /// The document's path, relative to the wiki root; `.md` is optional.
    pub path: String,
}

impl<P: Host> Overview<P> {
    pub fn new(wiki: Wiki<P>) -> Self {
        Overview { wiki }
    }
}

impl<P: Host> Tool for Overview<P> {
    type Args = OverviewArgs;
    const NAME: &'static str = "overview";
    const DESCRIPTION: &'static str = "Inspect a document without reading all of it: its \
title, size in lines, summary, frontmatter, outline (each heading with the line it starts on \
and its anchor), and the documents its wikilinks resolve to. Use the outline to read just the \
lines or sections you need with `read_document`.";

    async fn call(&self, args: OverviewArgs) -> Result<String> {
        match self.wiki.in_reference(&args.path) {
            Some((reference, rest)) => overview(reference, &reference_prefix(), rest).await,
            None => overview(&self.wiki, "", &args.path).await,
        }
    }
}

/// The overview of the document at `path` in `wiki`, with each path given
/// as `prefix` and the path in `wiki`.
async fn overview<P: Host>(wiki: &Wiki<P>, prefix: &str, path: &str) -> Result<String> {
    let inner = &wiki.inner;
    let (rel, source, doc) = load(wiki, prefix, path).await?;
    let ex = doc.extract();
    let name = rel.rsplit('/').next().unwrap_or(&rel);

    let mut out = String::new();
    writeln!(out, "path: {prefix}{rel}")?;
    writeln!(
        out,
        "title: {}",
        ex.title(name.strip_suffix(".md").unwrap_or(name))
    )?;
    writeln!(out, "lines: {}", source.lines().count())?;
    let summary = ex.summary();
    if !summary.is_empty() {
        writeln!(out, "summary: {summary}")?;
    }
    if let Some(frontmatter) = doc.frontmatter.as_ref().filter(|f| !is_empty(f)) {
        writeln!(out, "frontmatter:")?;
        for line in serde_norway::to_string(frontmatter)?.lines() {
            writeln!(out, "  {line}")?;
        }
    }
    if !ex.outline.is_empty() {
        writeln!(out, "outline:")?;
        // The levels of the headings enclosing the current one.
        let mut open: Vec<u8> = Vec::new();
        for heading in &ex.outline {
            while open.last().is_some_and(|&l| l >= heading.level) {
                open.pop();
            }
            let line = heading
                .line
                .map_or_else(|| "L?".to_string(), |l| format!("L{l}"));
            writeln!(
                out,
                "  {line:<6}{}{} {}  #{}",
                "  ".repeat(open.len()),
                "#".repeat(heading.level as usize),
                heading.text,
                heading.anchor
            )?;
            open.push(heading.level);
        }
    }
    let mut seen = HashSet::new();
    let mut references = String::new();
    for raw in ex.wikilinks.iter().filter(|raw| seen.insert(raw.as_str())) {
        let Some(target) = LinkTarget::parse(raw) else {
            continue;
        };
        let to = match inner.index.resolve(&target).await? {
            Resolution::Unique(page) => format!("{prefix}{}", page.path),
            Resolution::Ambiguous(pages) => format!("ambiguous ({} matches)", pages.len()),
            Resolution::Missing => "no match".to_string(),
        };
        writeln!(references, "  [[{raw}]] → {to}")?;
    }
    if !references.is_empty() {
        writeln!(out, "references:")?;
        out.push_str(&references);
    }
    Ok(out)
}

/// A document's text, whole, a line range of it, or some of its sections;
/// or a folder's index.
pub struct ReadDocument<P: Host> {
    wiki: Wiki<P>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadDocumentArgs {
    /// The document's path, relative to the wiki root; `.md` is optional. A
    /// folder's path (or `""` for the root) reads the folder's index.
    pub path: String,
    /// A line range: `12-40`, `L12-L40`, or to the end of the document,
    /// `12-` or `L12-end`. Not with `sections`.
    pub lines: Option<String>,
    /// Sections to read, by the anchors `overview` lists: each is its heading
    /// and everything up to the next heading of the same or a higher level.
    /// Not with `lines`.
    pub sections: Option<Vec<String>>,
}

impl<P: Host> ReadDocument<P> {
    pub fn new(wiki: Wiki<P>) -> Self {
        ReadDocument { wiki }
    }
}

impl<P: Host> Tool for ReadDocument<P> {
    type Args = ReadDocumentArgs;
    const NAME: &'static str = "read_document";
    const DESCRIPTION: &'static str = "Read a document: all of it, a line range (`lines`), or \
some of its sections by anchor (`sections`), as `overview` lists them. A folder's path reads its \
index: its subfolders, and its documents with their titles, tags and summaries.";

    async fn call(&self, args: ReadDocumentArgs) -> Result<String> {
        if args.lines.is_some() && args.sections.is_some() {
            bail!("give lines or sections, not both");
        }
        match self.wiki.in_reference(&args.path) {
            Some((reference, rest)) => read(reference, &reference_prefix(), rest, &args).await,
            None => read(&self.wiki, "", &args.path, &args).await,
        }
    }
}

/// `read_document` on `path` in `wiki`, with each path given as `prefix`
/// and the path in `wiki`.
async fn read<P: Host>(
    wiki: &Wiki<P>,
    prefix: &str,
    path: &str,
    args: &ReadDocumentArgs,
) -> Result<String> {
    let trimmed = path.trim().trim_start_matches('/');
    let folder = trimmed.trim_end_matches('/');
    let is_folder = if trimmed.is_empty() || trimmed.ends_with('/') {
        true
    } else {
        let doc = document_rel(trimmed);
        let is_doc = match &doc {
            Some(rel) => matches!(wiki.inner.root.kind(rel).await, Ok(FsKind::File)),
            None => false,
        };
        !is_doc && matches!(wiki.inner.root.kind(folder).await, Ok(FsKind::Dir))
    };
    if is_folder {
        if args.lines.is_some() || args.sections.is_some() {
            bail!("a folder's index has no lines or sections");
        }
        return folder_index(wiki, prefix, folder).await;
    }
    let (rel, source, doc) = load(wiki, prefix, path).await?;
    let lines: Vec<&str> = source.lines().collect();
    if let Some(range) = &args.lines {
        let (start, end) = parse_lines(range)?;
        if start > lines.len() {
            bail!("{prefix}{rel} has {} lines", lines.len());
        }
        let end = end.unwrap_or(lines.len()).min(lines.len());
        return Ok(text_of(&lines[start - 1..end]));
    }
    if let Some(anchors) = &args.sections {
        let outline = doc.extract().outline;
        let mut out = Vec::new();
        for anchor in anchors {
            let anchor = anchor.trim().trim_start_matches('#');
            let (start, end) = section_lines(&outline, anchor, lines.len())
                .ok_or_else(|| anyhow::anyhow!("no section #{anchor} in {prefix}{rel}"))?;
            out.push(text_of(&lines[start - 1..end]));
        }
        return Ok(out.join("\n"));
    }
    Ok(source)
}

/// `lines` joined, each ending in a newline.
fn text_of(lines: &[&str]) -> String {
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// A line range: `12-40`, `L12-L40`, `12-` or `L12-end`, as 1-based
/// inclusive lines; the end is `None` to the end of the document.
fn parse_lines(range: &str) -> Result<(usize, Option<usize>)> {
    let bad = || anyhow::anyhow!("not a line range: {range} (e.g. 12-40, L12-L40, 12- or L12-end)");
    let (start, end) = range.trim().split_once('-').ok_or_else(bad)?;
    let number = |s: &str| {
        let s = s.trim();
        s.strip_prefix('L')
            .unwrap_or(s)
            .parse::<usize>()
            .map_err(|_| bad())
    };
    let start = number(start)?;
    let end = match end.trim() {
        "" | "end" => None,
        end => Some(number(end)?),
    };
    if start == 0 {
        bail!("lines start at 1");
    }
    if end.is_some_and(|end| end < start) {
        bail!("the range {range} ends before it starts");
    }
    Ok((start, end))
}

/// The lines of the section headed by `anchor`: from its heading's line to
/// the line before the next heading of the same or a higher level, or the
/// last line.
fn section_lines(outline: &[OutlineHeading], anchor: &str, last: usize) -> Option<(usize, usize)> {
    let at = outline.iter().position(|h| h.anchor == anchor)?;
    let heading = &outline[at];
    let start = heading.line?;
    let end = outline[at + 1..]
        .iter()
        .find(|h| h.level <= heading.level)
        .and_then(|h| h.line)
        .map_or(last, |line| line - 1);
    Some((start, end))
}

/// A folder's index, like the web view's: the reference's prompt at its
/// root, the subfolders, then each document with its title, tags and
/// summary, as `search` gives them.
async fn folder_index<P: Host>(wiki: &Wiki<P>, prefix: &str, folder: &str) -> Result<String> {
    if !folder.is_empty() && !matches!(wiki.inner.root.kind(folder).await, Ok(FsKind::Dir)) {
        bail!("no folder at {prefix}{folder}");
    }
    let folder = Folder::new(wiki.clone(), folder.to_string());
    let mut out = String::new();
    let shown = if folder.path().is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}{}/", folder.path())
    };
    writeln!(
        out,
        "folder: {}",
        if shown.is_empty() { "/" } else { &shown }
    )?;
    if let Some(prompt) = wiki.inner.prompt.filter(|_| folder.path().is_empty()) {
        writeln!(out)?;
        out.push_str(prompt.trim_end());
        out.push('\n');
    }
    let folders = folder.folders().await?;
    if !folders.is_empty() {
        writeln!(out, "\nfolders:")?;
        for sub in &folders {
            writeln!(out, "  {prefix}{}/", sub.path())?;
        }
    }
    let documents = folder.documents().await?;
    if !documents.is_empty() {
        writeln!(out, "\ndocuments:")?;
        for doc in &documents {
            write!(out, "  {prefix}{} — {}", doc.path(), doc.title())?;
            for tag in doc.tags() {
                write!(out, "  #{tag}")?;
            }
            out.push('\n');
            let summary = doc.summary();
            if !summary.is_empty() {
                writeln!(out, "    {summary}")?;
            }
        }
    }
    if folders.is_empty() && documents.is_empty() {
        writeln!(out, "\nNo documents.")?;
    }
    Ok(out)
}

/// The documents open in the web view, most recently active first: the
/// tool that lets an agent see what the user is looking at.
pub struct OpenFiles<P: Host> {
    wiki: Wiki<P>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OpenFilesArgs {}

impl<P: Host> OpenFiles<P> {
    pub fn new(wiki: Wiki<P>) -> Self {
        OpenFiles { wiki }
    }
}

impl<P: Host> Tool for OpenFiles<P> {
    type Args = OpenFilesArgs;
    const NAME: &'static str = "open_files";
    const DESCRIPTION: &'static str = "The documents the user has open in the wiki's web view, \
most recently focused or selected in first. Marks the one whose window has focus, and gives \
the selection as a source line range, e.g. `selected L12-L18` (`L40-end` runs to the end of \
the file).";

    async fn call(&self, _: OpenFilesArgs) -> Result<String> {
        let files = self.wiki.inner.tabs.open_files();
        if files.is_empty() {
            return Ok("No documents open.".to_string());
        }
        let mut out = String::new();
        for file in files {
            write!(out, "{}", file.rel)?;
            if file.focused {
                write!(out, "  (focused)")?;
            }
            if let Some(sel) = file.selection {
                match sel.end {
                    Some(end) => write!(out, "  selected L{}-L{end}", sel.start)?,
                    None => write!(out, "  selected L{}-end", sel.start)?,
                }
            }
            out.push('\n');
        }
        Ok(out)
    }
}

/// A page's URL in the web view, for linking the user to it.
pub struct PageUrl<P: Host> {
    wiki: Wiki<P>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PageUrlArgs {
    /// The document's path, relative to the wiki root; `.md` is optional.
    pub path: String,
    /// A source line to link to.
    pub line: Option<usize>,
    /// A heading to link to, by its text or its anchor.
    pub heading: Option<String>,
}

impl<P: Host> PageUrl<P> {
    pub fn new(wiki: Wiki<P>) -> Self {
        PageUrl { wiki }
    }
}

impl<P: Host> Tool for PageUrl<P> {
    type Args = PageUrlArgs;
    const NAME: &'static str = "page_url";
    const DESCRIPTION: &'static str = "The URL of a document in the wiki's web view, to give \
the user a link. Optionally to a line (a line link, which highlights the block holding it) or \
to a heading (by its text or anchor), not both.";

    async fn call(&self, args: PageUrlArgs) -> Result<String> {
        match self.wiki.in_reference(&args.path) {
            Some((reference, rest)) => page_url(reference, &reference_prefix(), rest, &args).await,
            None => page_url(&self.wiki, "", &args.path, &args).await,
        }
    }
}

/// The URL of the document at `path` in `wiki`, to the line or heading in
/// `args`; errors give paths as `prefix` and the path in `wiki`.
async fn page_url<P: Host>(
    wiki: &Wiki<P>,
    prefix: &str,
    path: &str,
    args: &PageUrlArgs,
) -> Result<String> {
    let inner = &wiki.inner;
    let (rel, _, doc) = load(wiki, prefix, path).await?;
    let fragment = match (args.line, args.heading.as_deref()) {
        (Some(_), Some(_)) => bail!("give a line or a heading, not both"),
        (Some(0), None) => bail!("lines start at 1"),
        (Some(line), None) => format!("#:{line}"),
        (None, Some(heading)) => {
            let want = heading.trim().trim_start_matches('#').trim().to_lowercase();
            let ex = doc.extract();
            let Some(found) = ex
                .outline
                .iter()
                .find(|h| h.text.to_lowercase() == want || h.anchor == want)
            else {
                bail!("no heading \"{heading}\" in {prefix}{rel}");
            };
            format!("#{}", found.anchor)
        }
        (None, None) => String::new(),
    };
    let origin = request_origin().unwrap_or_default();
    Ok(format!(
        "{origin}{}{}{fragment}",
        inner.base,
        path_to_url(&rel)
    ))
}

/// The document at `path` (relative to the wiki root, `.md` optional): its
/// rel, its text and its parsed tree. Fails for a path that isn't a
/// document file reached without symlinks; errors give paths as `prefix`
/// and the path in `wiki`.
async fn load<P: Host>(
    wiki: &Wiki<P>,
    prefix: &str,
    path: &str,
) -> Result<(String, String, DocumentTree<P>)> {
    let Some(rel) = document_rel(path) else {
        bail!("not a document path: {prefix}{path}");
    };
    let inner = &wiki.inner;
    if !matches!(inner.root.kind(&rel).await, Ok(FsKind::File)) {
        bail!("no document at {prefix}{rel}");
    }
    let source = inner.root.read(&rel).await?;
    let doc = parse(&*inner.host, &source);
    Ok((rel, source, doc))
}

/// The rel of the document at `path` (relative to the wiki root, `.md`
/// optional), or `None` if a segment is empty or hidden.
fn document_rel(path: &str) -> Option<String> {
    let path = path.trim().trim_start_matches('/');
    let ok = path
        .split('/')
        .all(|seg| !seg.is_empty() && !seg.starts_with('.'));
    if !ok {
        return None;
    }
    Some(if path.ends_with(".md") {
        path.to_string()
    } else {
        format!("{path}.md")
    })
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::document_rel;

    #[test]
    fn document_rels() {
        assert_eq!(document_rel("a/b").as_deref(), Some("a/b.md"));
        assert_eq!(document_rel("/a/b.md").as_deref(), Some("a/b.md"));
        assert_eq!(document_rel("../a"), None);
        assert_eq!(document_rel("a//b"), None);
        assert_eq!(document_rel("a/.hidden/b"), None);
        assert_eq!(document_rel(""), None);
    }
}
