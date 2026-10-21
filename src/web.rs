use std::collections::HashMap;
use std::convert::Infallible;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use tokio_stream::StreamExt;

use crate::Wiki;
use crate::components::{DisambiguationBody, FolderIndexBody, NotFoundBody, SearchBody, TagBody};
use crate::data::{Document, Folder};
use crate::index::{FsKind, fs_kind};
use crate::links::local_href;
use crate::names::{encode_component, image_type_of, url_to_path, url_to_rel};
use crate::parse::Host;
use crate::tabs::PushedState;
use crate::template::{BodyCtx, DocumentMeta, HeadCtx, PageKind, Template};

const HLJS_CSS: &str =
    "https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@11.12.0/styles/rose-pine-dawn.min.css";
const HLJS_DARK_CSS: &str =
    "https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@11.12.0/styles/rose-pine.min.css";
const HLJS_JS: &str =
    "https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@11.12.0/highlight.min.js";
const MERMAID_JS: &str = "https://cdn.jsdelivr.net/npm/mermaid@12/dist/mermaid.esm.min.mjs";
const HTMX_JS: &str = "https://cdn.jsdelivr.net/npm/htmx.org@2.0.11/dist/htmx.min.js";
const HTMX_SSE_JS: &str = "https://cdn.jsdelivr.net/npm/htmx-ext-sse@2.2.4/dist/sse.min.js";

/// Rosé Pine: Dawn in light mode, the main variant in dark mode.
const STYLE: &str = "
:root { color-scheme: light dark;
  --fg: #575279; --bg: #faf4ed; --surface: #fffaf3; --muted: #797593; --faint: #9893a5;
  --link: #286983; --tag: #907aa9; --unresolved: #b4637a; --border: #dfdad9;
  --code-bg: #f2e9e1; --line-target: #ea9d3433;
  --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace; }
@media (prefers-color-scheme: dark) {
  :root { --fg: #e0def4; --bg: #191724; --surface: #1f1d2e; --muted: #908caa; --faint: #6e6a86;
    --link: #9ccfd8; --tag: #c4a7e7; --unresolved: #eb6f92; --border: #403d52;
    --code-bg: #26233a; --line-target: #f6c17726; }
}
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif;
  max-width: 48rem; margin: 0 auto; padding: 1.5rem 1rem 4rem; line-height: 1.7;
  color: var(--fg); background: var(--bg); }
form[role=search] { margin-bottom: 1rem; }
input[type=search] { font: inherit; width: 100%; max-width: 20rem; box-sizing: border-box;
  padding: 0.3rem 0.75rem; color: var(--fg); background: var(--surface);
  border: 1px solid var(--border); border-radius: 0.5rem; }
input[type=search]:focus { outline: 2px solid var(--link); outline-offset: -1px; }
header { margin-bottom: 1.5rem; color: var(--muted); }
header nav a, header .tags a { margin-right: 0.25rem; }
p, ul, ol, pre, table, blockquote, details { margin: 0 0 1.25rem; }
li { margin: 0.4rem 0; }
h1, h2, h3, h4, h5, h6 { line-height: 1.3; margin: 2rem 0 1rem; }
h1, h2 { padding-bottom: 0.3rem; border-bottom: 1px solid var(--border); }
a.anchor { margin-left: 0.4rem; color: var(--faint); text-decoration: none; opacity: 0; }
:is(h1, h2, h3, h4, h5, h6):hover > a.anchor, a.anchor:focus { opacity: 1; }
a { color: var(--link); text-underline-offset: 0.2em; }
a.unresolved { color: var(--unresolved); }
a.tag { color: var(--tag); text-decoration: none; }
a.tag:hover { text-decoration: underline; }
.muted { color: var(--muted); }
blockquote { margin-left: 0; padding-left: 1rem; border-left: 3px solid var(--border);
  color: var(--muted); }
hr { border: none; border-top: 1px solid var(--border); margin: 2rem 0; }
code { font-family: var(--mono); font-size: 0.875em; }
:not(pre) > code { background: var(--code-bg); padding: 0.1em 0.35em; border-radius: 0.3rem; }
pre { overflow-x: auto; border-radius: 0.5rem; background: var(--code-bg); }
pre > code { display: block; padding: 1rem; }
pre.mermaid { background: none; }
table { border-collapse: collapse; }
th, td { border: 1px solid var(--border); padding: 0.3rem 0.75rem; text-align: left; vertical-align: top; }
th { background: var(--surface); }
details summary { cursor: pointer; color: var(--muted); }
details table { margin-top: 0.75rem; }
.line-target { background: var(--line-target); }
.crumbs-row { display: flex; justify-content: space-between; align-items: baseline; gap: 1rem; }
button.copy-path { flex: none; font: inherit; font-size: 0.8rem; color: var(--muted);
  background: var(--surface); border: 1px solid var(--border); border-radius: 0.4rem;
  padding: 0.1rem 0.6rem; cursor: pointer; }
button.copy-path:hover { color: var(--fg); }
.index h2 { margin-top: 2.5rem; }
.recent h2, .top-tags h2 { font-size: 0.8rem; font-weight: 600; text-transform: uppercase;
  letter-spacing: 0.06em; color: var(--muted); border: none; margin: 1.5rem 0 0.5rem; }
.recent ul { list-style: none; padding: 0.5rem 1rem; background: var(--surface);
  border: 1px solid var(--border); border-radius: 0.5rem; }
.recent li { display: flex; align-items: baseline; gap: 0.6rem; margin: 0;
  font-size: 0.9rem; line-height: 1.9; }
.recent li a { flex: none; max-width: 60%; overflow: hidden; text-overflow: ellipsis;
  white-space: nowrap; }
.recent .file { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;
  white-space: nowrap; font-family: var(--mono); font-size: 0.8rem; color: var(--faint); }
.recent time { flex: none; font-size: 0.8rem; color: var(--faint); }
.listing { list-style: none; padding: 0; }
.listing > li { margin: 0; padding: 0.75rem 0; border-bottom: 1px solid var(--border); }
.listing > li:last-child { border-bottom: none; }
.listing .folder a { font-weight: 600; }
.entry-head { display: flex; justify-content: space-between; gap: 1rem; align-items: baseline; }
.entry-head a { font-weight: 600; }
.entry-head .file { margin-left: 0.4rem; font-family: var(--mono); font-size: 0.8rem;
  color: var(--faint); }
.entry-head time { flex: none; font-size: 0.85rem; color: var(--faint); }
.listing .summary { margin: 0.2rem 0 0; color: var(--muted); }
.listing .tags { margin: 0.2rem 0 0; font-size: 0.9rem; }
.listing .tags a, .top-tags a { margin-right: 0.6rem; }
.top-tags p { margin: 0; }
.top-tags .count { color: var(--faint); }
";

/// A `button.copy-path` (see `BodyCtx::copy_path`) copies its `data-path`
/// to the clipboard and says so for a moment.
const COPY_JS: &str = "
document.addEventListener('click', async (e) => {
  const button = e.target.closest('button.copy-path');
  if (!button) return;
  const label = button.textContent;
  try {
    await navigator.clipboard.writeText(button.dataset.path);
    button.textContent = 'Copied';
  } catch {
    button.textContent = 'Copy failed';
  }
  setTimeout(() => { button.textContent = label; }, 1200);
});
";

/// A document page pushes its state to `{base}/_wiki/state`: when its
/// live-update stream connects, and when focus or the selection changes.
/// The selection is by source line: the line of the block it starts in,
/// through the line before the first block after the one it ends in (`null`
/// when that is the last block). Only a selection inside the htmx root's
/// line-numbered blocks counts.
const STATE_JS: &str = "
(() => {
  const tab = document.body.dataset.tab;
  const url = document.body.dataset.base + '/_wiki/state';
  const root = () => document.getElementById('htmx-root');
  const block = (node) => {
    const el = (node.nodeType === 1 ? node : node.parentElement)?.closest('[data-line]');
    return el && root()?.contains(el) ? el : null;
  };
  const selection = () => {
    const sel = document.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) return null;
    const range = sel.getRangeAt(0);
    const first = block(range.startContainer);
    const last = block(range.endContainer);
    if (!first || !last) return null;
    const start = Number(first.dataset.line);
    const lastLine = Number(last.dataset.line);
    let end = null;
    for (const el of root().querySelectorAll('[data-line]')) {
      const n = Number(el.dataset.line);
      if (n > lastLine) { end = n - 1; break; }
    }
    return { start, end };
  };
  let sent = null;
  const push = () => {
    const state = { tab, focused: document.hasFocus(), selection: selection() };
    const body = JSON.stringify(state);
    if (body === sent) return;
    sent = body;
    fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body,
      keepalive: true }).catch(() => {});
  };
  let timer;
  const soon = () => { clearTimeout(timer); timer = setTimeout(push, 250); };
  document.addEventListener('htmx:sseOpen', () => { sent = null; push(); });
  addEventListener('focus', push);
  addEventListener('blur', push);
  document.addEventListener('visibilitychange', push);
  document.addEventListener('selectionchange', soon);
})();
";

/// Line links, `#:N`: the last block in the htmx root whose `data-line` is
/// at most N, and that is laid out, gets the `line-target` class and is
/// scrolled to, on load and on `hashchange`. A live-update swap marks it
/// again without scrolling.
const LINE_JS: &str = "
(() => {
  const mark = (scroll) => {
    document.querySelectorAll('.line-target').forEach((el) => el.classList.remove('line-target'));
    const m = /^#:(\\d+)$/.exec(location.hash);
    if (!m) return;
    const n = Number(m[1]);
    let hit = null;
    for (const el of document.querySelectorAll('#htmx-root [data-line]')) {
      if (Number(el.dataset.line) <= n && el.getClientRects().length) hit = el;
    }
    if (!hit) return;
    hit.classList.add('line-target');
    if (scroll) hit.scrollIntoView({ block: 'center' });
  };
  mark(true);
  addEventListener('hashchange', () => mark(true));
  document.addEventListener('htmx:afterSwap', (e) => {
    if (e.detail.target?.id === 'htmx-root') mark(false);
  });
})();
";

pub fn router<P: Host, S: Clone + Send + Sync + 'static>(wiki: Wiki<P>) -> Router<S> {
    let base = wiki.inner.base.clone();
    let mut router = Router::new()
        .route(&format!("{base}/"), get(root_index::<P>))
        .route(&format!("{base}/_wiki/link"), get(link_page::<P>))
        .route(&format!("{base}/_wiki/tag/{{*tag}}"), get(tag_page::<P>))
        .route(&format!("{base}/_wiki/search"), get(search_page::<P>))
        .route(&format!("{base}/_wiki/events"), get(page_events::<P>))
        .route(&format!("{base}/_wiki/state"), post(push_state::<P>))
        .route_service(
            &format!("{base}/_wiki/mcp"),
            crate::mcp::service(wiki.clone()),
        )
        .route(&format!("{base}/{{*path}}"), get(path_page::<P>));
    if !base.is_empty() {
        let to = format!("{base}/");
        router = router.route(&base, get(move || async move { Redirect::permanent(&to) }));
    }
    router.with_state(wiki)
}

struct Failure(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for Failure {
    fn from(e: E) -> Failure {
        Failure(e.into())
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", self.0)).into_response()
    }
}

type Result<T = Response> = std::result::Result<T, Failure>;

/// The content a page's template embeds.
type Content<'a, P> = dyn Fn(&BodyCtx<'_, P>) -> Markup + Sync + 'a;

/// What the library knows about a page before its template draws it.
struct Page<'a> {
    kind: PageKind,
    title: String,
    /// The folder the breadcrumbs lead down to, or `None` for none.
    crumbs: Option<&'a str>,
    /// The page's URL path, base included, which wires the htmx root to
    /// re-fetch itself on that page's SSE `changed` events.
    live: Option<String>,
}

/// The whole page: the library's skeleton, with the template's head markup
/// and its body inside the htmx root.
async fn draw_page<P: Host>(
    wiki: &Wiki<P>,
    request: &Parts,
    page: Page<'_>,
    content: &Content<'_, P>,
) -> String {
    let base = &wiki.inner.base;
    let template = &wiki.inner.template;
    let mut head = HeadCtx::new(&page.kind, request, base, page.title);
    let host_head = template.head(&mut head).await;
    let body = BodyCtx::new(wiki, &page.kind, request, page.crumbs);
    let root = template.body(&body, content).await;
    let live = page.live.as_deref();
    // Only a document page counts as open, for the `open_files` tool.
    let document = matches!(page.kind, PageKind::Document(_));
    let tab = live.filter(|_| document).map(|_| wiki.inner.tabs.new_id());
    let sse = live.map(|_| "sse");
    let events = live.map(|url| {
        let mut events = format!("{base}/_wiki/events?url={}", encode_component(url));
        if let Some(tab) = &tab {
            events.push_str(&format!("&tab={tab}"));
        }
        events
    });
    let refetch = live;
    let trigger = live.map(|_| "sse:changed");
    let select = live.map(|_| "#htmx-root");
    let swap = live.map(|_| "outerHTML");
    // Keeps the root's hx attributes off its descendants, so a host element
    // making its own htmx request isn't given `hx-select="#htmx-root"`.
    let disinherit = live.map(|_| "*");
    let mermaid = format!(
        "
import mermaid from '{MERMAID_JS}';
const dark = window.matchMedia('(prefers-color-scheme: dark)').matches;
mermaid.initialize({{ startOnLoad: true, theme: dark ? 'dark' : 'default' }});
document.addEventListener('htmx:afterSwap', (e) => {{
  if (e.detail.target?.id === 'htmx-root') {{ hljs.highlightAll(); mermaid.run(); }}
}});
"
    );
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (head.title()) }
                link rel="stylesheet" href=(HLJS_CSS) media="(prefers-color-scheme: light)";
                link rel="stylesheet" href=(HLJS_DARK_CSS) media="(prefers-color-scheme: dark)";
                @if !head.default_styles_suppressed() {
                    style { (PreEscaped(STYLE)) }
                }
                @if let Some(css) = &wiki.inner.stylesheet {
                    style { (PreEscaped(css)) }
                }
                script src=(HTMX_JS) {}
                script src=(HTMX_SSE_JS) {}
                (host_head)
            }
            body hx-ext=[sse] sse-connect=[events] data-tab=[tab.as_deref()] data-base=(base) {
                div #htmx-root hx-get=[refetch] hx-trigger=[trigger] hx-select=[select]
                    hx-swap=[swap] hx-disinherit=[disinherit] {
                    (root)
                }
                script src=(HLJS_JS) {}
                script { "hljs.highlightAll();" }
                script type="module" { (PreEscaped(mermaid)) }
                script { (PreEscaped(LINE_JS)) }
                script { (PreEscaped(COPY_JS)) }
                @if tab.is_some() {
                    script { (PreEscaped(STATE_JS)) }
                }
            }
        }
    }
    .into_string()
}

impl<P: Host> Wiki<P> {
    /// A page of the host's own, for a host route outside the wiki: drawn
    /// through the template like any page, with `PageKind::Custom`, and
    /// empty unless the content adds something.
    pub fn custom_page<'a>(
        &'a self,
        request: &'a Parts,
        title: impl Into<String>,
    ) -> CustomPage<'a, P> {
        CustomPage {
            wiki: self,
            request,
            title: title.into(),
            live: false,
        }
    }
}

/// A host's page being set up; see [`Wiki::custom_page`].
pub struct CustomPage<'a, P: Host> {
    wiki: &'a Wiki<P>,
    request: &'a Parts,
    title: String,
    live: bool,
}

impl<P: Host> CustomPage<'_, P> {
    /// Makes the page refetch itself whenever it is signalled
    /// ([`Wiki::signal`]) at its request's URL path.
    pub fn live(mut self) -> Self {
        self.live = true;
        self
    }

    /// The whole page, with `content` where the template embeds it.
    pub async fn draw(self, content: impl Fn(&BodyCtx<'_, P>) -> Markup + Sync) -> Markup {
        let page = Page {
            kind: PageKind::Custom,
            title: self.title,
            crumbs: None,
            live: self.live.then(|| self.request.uri.path().to_string()),
        };
        PreEscaped(draw_page(self.wiki, self.request, page, &content).await)
    }
}

async fn root_index<P: Host>(State(wiki): State<Wiki<P>>, request: Parts) -> Result {
    folder_page(&wiki, &request, "").await
}

async fn path_page<P: Host>(State(wiki): State<Wiki<P>>, request: Parts) -> Result {
    let base = &wiki.inner.base;
    let Some(rest) = request.uri.path().strip_prefix(&format!("{base}/")) else {
        return Ok(not_found(&wiki, &request).await);
    };
    if rest == "_wiki" || rest.starts_with("_wiki/") {
        return Ok(not_found(&wiki, &request).await);
    }
    if let Some(folder) = rest.strip_suffix('/') {
        let Some(folder) = url_to_path(folder) else {
            return Ok(not_found(&wiki, &request).await);
        };
        return folder_page(&wiki, &request, &folder).await;
    }
    let Some(rel) = url_to_rel(rest) else {
        return Ok(not_found(&wiki, &request).await);
    };
    let path = rel.strip_suffix(".md").unwrap_or(&rel);
    // A last segment with an image extension is only ever an image request.
    if let Some(content_type) = image_type_of(path) {
        return Ok(image(&wiki, &request, path, content_type).await);
    }
    // Existence comes from the disk, not the index, so pages are served while
    // the index is still filling.
    let root = &wiki.inner.root;
    if is_kind(root, &rel, FsKind::File).await {
        return page_view(&wiki, &request, &rel).await;
    }
    // A page asked for by its file name goes to its URL, as a `.md` link to
    // it would; the browser keeps the `#fragment`.
    if path.ends_with(".md") && is_kind(root, path, FsKind::File).await {
        let to = local_href(base, path, "", false);
        return Ok(Redirect::temporary(&to).into_response());
    }
    if is_kind(root, path, FsKind::Dir).await {
        return Ok(Redirect::permanent(&format!("{base}/{rest}/")).into_response());
    }
    Ok(not_found(&wiki, &request).await)
}

/// The image file at `rel`, sent whole, if it is a file reached without
/// symlinks; otherwise 404.
async fn image<P: Host>(
    wiki: &Wiki<P>,
    request: &Parts,
    rel: &str,
    content_type: &'static str,
) -> Response {
    let root = &wiki.inner.root;
    if !is_kind(root, rel, FsKind::File).await {
        return not_found(wiki, request).await;
    }
    match tokio::fs::read(root.join(rel)).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type)], bytes).into_response(),
        Err(_) => not_found(wiki, request).await,
    }
}

/// True when `root/rel` is of `kind`, checked without following symlinks
/// (any error counts as no).
async fn is_kind(root: &std::path::Path, rel: &str, kind: FsKind) -> bool {
    matches!(fs_kind(root, rel).await, Ok(k) if k == kind)
}

/// Draws the document at `rel`, which the caller has checked is a file
/// reached without symlinks. Its wikilinks are resolved first, so the
/// content is a synchronous draw of the one parse.
async fn page_view<P: Host>(wiki: &Wiki<P>, request: &Parts, rel: &str) -> Result {
    let Some(doc) = Document::load(wiki, rel).await? else {
        return Ok(not_found(wiki, request).await);
    };
    let body = doc.body().await?;
    let meta = DocumentMeta::new(
        doc.title(),
        rel.to_string(),
        doc.url(),
        doc.frontmatter().cloned(),
        doc.tags().to_vec(),
    );
    let page = Page {
        kind: PageKind::Document(meta),
        title: doc.title(),
        crumbs: Some(doc.folder()),
        live: Some(format!("{}{}", wiki.inner.base, doc.url())),
    };
    let content = |ctx: &BodyCtx<'_, P>| ctx.draw(&body);
    Ok(Html(draw_page(wiki, request, page, &content).await).into_response())
}

/// `GET {base}/_wiki/events?url={page url path}&tab={id}`: an SSE stream
/// sending `event: changed` with `data: {url path}` each time the page is
/// signalled: by the watcher when it reindexes or removes the document at
/// that URL, or by the host (`Wiki::signal`). Signals that arrive before
/// one is sent collapse into one. The page need not exist. With `tab`, a
/// document page counts as open while the stream is connected. The stream
/// ends when the client disconnects, or once every clone of the `Wiki`
/// (including every router built from it) has been dropped. A served
/// connection holds a router clone for as long as it is open, so an open
/// stream is never ended from the server side: a host's graceful shutdown
/// does not complete on its own while a client stays connected.
async fn page_events<P: Host>(
    State(wiki): State<Wiki<P>>,
    Query(query): Query<EventsQuery>,
    request: Parts,
) -> Response {
    let Some(url) = query.url.filter(|url| url.starts_with('/')) else {
        return not_found(&wiki, &request).await;
    };
    // A document page is open for as long as the stream holds the guard.
    let rel = url
        .strip_prefix(&wiki.inner.base)
        .filter(|rest| rest.starts_with('/'))
        .and_then(url_to_rel);
    let guard = query
        .tab
        .filter(|tab| valid_tab(tab))
        .zip(rel)
        .map(|(tab, rel)| wiki.inner.tabs.connect(tab, rel.into()));
    let signals = wiki.inner.signals.open_page(&url);
    let stream = signals.map(move |()| {
        let _open = &guard;
        Ok::<_, Infallible>(Event::default().event("changed").data(&url))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[derive(serde::Deserialize)]
struct EventsQuery {
    /// The page's URL path, base included.
    url: Option<String>,
    /// A document page's tab id, from `draw_page`; the page counts as open
    /// while the stream is connected.
    tab: Option<String>,
}

/// A tab id as `Tabs::new_id` makes them, loosely: short, digits and `-`.
fn valid_tab(tab: &str) -> bool {
    !tab.is_empty() && tab.len() <= 64 && tab.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

/// `POST {base}/_wiki/state`: a document page's pushed state; see
/// `STATE_JS`. Ignored for a tab whose stream isn't connected.
async fn push_state<P: Host>(
    State(wiki): State<Wiki<P>>,
    Json(state): Json<PushedState>,
) -> StatusCode {
    if valid_tab(&state.tab) {
        wiki.inner.tabs.push(state);
    }
    StatusCode::NO_CONTENT
}

async fn folder_page<P: Host>(wiki: &Wiki<P>, request: &Parts, folder: &str) -> Result {
    // The folder exists if the disk says so; its contents come from the index.
    if !is_kind(&wiki.inner.root, folder, FsKind::Dir).await {
        return Ok(not_found(wiki, request).await);
    }
    let body = FolderIndexBody::load(&Folder::new(wiki.clone(), folder.to_string())).await?;
    let parent = folder.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    let page = Page {
        kind: PageKind::FolderIndex {
            path: folder.to_string(),
        },
        title: body.title().to_string(),
        crumbs: (!folder.is_empty()).then_some(parent),
        live: None,
    };
    let content = |ctx: &BodyCtx<'_, P>| ctx.draw(&body);
    Ok(Html(draw_page(wiki, request, page, &content).await).into_response())
}

async fn link_page<P: Host>(
    State(wiki): State<Wiki<P>>,
    Query(query): Query<HashMap<String, String>>,
    request: Parts,
) -> Result {
    let raw = query.get("to").map(String::as_str).unwrap_or("");
    let Some(body) = DisambiguationBody::load(&wiki, raw).await? else {
        return Ok(not_found(&wiki, &request).await);
    };
    let page = Page {
        kind: PageKind::Disambiguation {
            target: body.title().to_string(),
        },
        title: body.title().to_string(),
        crumbs: Some(""),
        live: None,
    };
    let content = |ctx: &BodyCtx<'_, P>| ctx.draw(&body);
    Ok(Html(draw_page(&wiki, &request, page, &content).await).into_response())
}

async fn tag_page<P: Host>(
    State(wiki): State<Wiki<P>>,
    Path(tag): Path<String>,
    request: Parts,
) -> Result {
    let body = TagBody::load(&wiki, &tag).await?;
    let page = Page {
        kind: PageKind::Tag {
            tag: tag.to_lowercase(),
        },
        title: body.title(),
        crumbs: Some(""),
        live: None,
    };
    let content = |ctx: &BodyCtx<'_, P>| ctx.draw(&body);
    Ok(Html(draw_page(&wiki, &request, page, &content).await).into_response())
}

async fn search_page<P: Host>(
    State(wiki): State<Wiki<P>>,
    Query(query): Query<HashMap<String, String>>,
    request: Parts,
) -> Result {
    let q = query.get("q").map(String::as_str).unwrap_or("");
    let body = SearchBody::load(&wiki, q).await?;
    let page = Page {
        kind: PageKind::Search {
            query: q.to_string(),
        },
        title: body.title(),
        crumbs: Some(""),
        live: None,
    };
    let content = |ctx: &BodyCtx<'_, P>| ctx.draw(&body);
    Ok(Html(draw_page(&wiki, &request, page, &content).await).into_response())
}

async fn not_found<P: Host>(wiki: &Wiki<P>, request: &Parts) -> Response {
    let page = Page {
        kind: PageKind::NotFound,
        title: "Not found".to_string(),
        crumbs: Some(""),
        live: None,
    };
    let content = |ctx: &BodyCtx<'_, P>| ctx.draw(&NotFoundBody);
    let html = draw_page(wiki, request, page, &content).await;
    (StatusCode::NOT_FOUND, Html(html)).into_response()
}
