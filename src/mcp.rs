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

use crate::Wiki;
use crate::index::{FsKind, Resolution, fs_kind};
use crate::names::LinkTarget;
use crate::parse::{Host, parse};

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
        Box::pin(async move {
            let args: T::Args = serde_json::from_value(args)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let result = match tool.call(args).await {
                Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
                Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))]),
            };
            Ok(CallToolResponse::from(result))
        })
    })
}

impl<P: Host> Wiki<P> {
    /// The default tool collection, for an rmcp server of type `S`; edit it
    /// or merge it into a host's own server.
    pub fn tools<S: Send + Sync + 'static>(&self) -> ToolRouter<S> {
        ToolRouter::new()
            .with_route(route(Search::new(self.clone())))
            .with_route(route(Overview::new(self.clone())))
            .with_route(route(OpenFiles::new(self.clone())))
    }
}

/// The server the router mounts: the default tools.
pub(crate) struct Server<P: Host> {
    tools: ToolRouter<Server<P>>,
    instructions: Arc<str>,
}

impl<P: Host> ServerHandler for Server<P> {
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

/// What the agent is told once, on connecting: how to use the tools, and
/// where the wiki root is, since results give paths relative to it.
fn instructions(root: &std::path::Path) -> String {
    format!(
        "A wiki of markdown files. The wiki root is `{}`; paths in results are relative to it. \
         {USAGE}",
        root.display()
    )
}

const USAGE: &str = "Use `search` to find documents, and `overview` to see a document's outline (with \
line numbers) before reading the part you need. Use `open_files` to see what the user has \
open in the wiki's web view, and what they have selected.";

/// The HTTP endpoint for the default tools, with no sessions.
pub(crate) fn service<P: Host>(
    wiki: Wiki<P>,
) -> StreamableHttpService<Server<P>, NeverSessionManager> {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    let instructions: Arc<str> = instructions(&wiki.inner.root).into();
    StreamableHttpService::new(
        move || {
            Ok(Server {
                tools: wiki.tools(),
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
    /// Only documents under this folder, relative to the wiki root.
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
        let index = &self.wiki.inner.index;
        let folder = args
            .folder
            .as_deref()
            .map(|f| f.trim_matches('/'))
            .filter(|f| !f.is_empty())
            .map(|f| format!("{f}/"));
        let pages: Vec<_> = index
            .search(&args.query)
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
            write!(out, "{} — {}", page.path, page.title)?;
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
title, summary, frontmatter, outline (each heading with the line it starts on and its anchor), \
and the documents its wikilinks resolve to. Use the outline's line numbers to read just the \
section you need.";

    async fn call(&self, args: OverviewArgs) -> Result<String> {
        let inner = &self.wiki.inner;
        let Some(rel) = document_rel(&args.path) else {
            bail!("not a document path: {}", args.path);
        };
        if !matches!(fs_kind(&inner.root, &rel).await, Ok(FsKind::File)) {
            bail!("no document at {rel}");
        }
        let source = tokio::fs::read_to_string(inner.root.join(&rel)).await?;
        let doc = parse(&*inner.host, &source);
        let ex = doc.extract();
        let name = rel.rsplit('/').next().unwrap_or(&rel);

        let mut out = String::new();
        writeln!(out, "path: {rel}")?;
        writeln!(
            out,
            "title: {}",
            ex.title(name.strip_suffix(".md").unwrap_or(name))
        )?;
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
                Resolution::Unique(page) => page.path,
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
