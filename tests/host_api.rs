//! A host's hooks through the web view: a host inline element, and a
//! template that sets the title, drops the built-in style and places the
//! built-in chrome itself.

use agent_wiki::maud::{Markup, html};
use agent_wiki::nom::bytes::complete::{tag, take_while1};
use agent_wiki::nom::sequence::delimited;
use agent_wiki::{
    BodyCtx, DrawChildren, DrawCtx, Element, HeadCtx, Host, InlineKind, InlineStream, Never,
    PageKind, Parsed, Template, Wiki,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

struct TestHost;

/// `[[ABC-1]]` and the like.
struct Ticket(String);

impl Element<TestHost> for Ticket {
    fn draw(&self, _: &DrawCtx<'_>, _: DrawChildren<'_, TestHost>) -> Markup {
        html! { span.ticket { (self.0) } }
    }

    fn text(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

impl Host for TestHost {
    type Block = Never;
    type Inline = Ticket;
    type Template = TestTemplate;

    fn parse_inline(&self, s: &mut InlineStream<'_, Self>) -> Option<Parsed<InlineKind<Self>>> {
        let id = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-';
        let id = s.try_parse(delimited(tag("[[ABC-"), take_while1(id), tag("]]")))?;
        Some(s.leaf(InlineKind::Host(Ticket(format!("ABC-{id}")))))
    }
}

struct TestTemplate;

impl Template<TestHost> for TestTemplate {
    async fn head(&self, ctx: &mut HeadCtx<'_, TestHost>) -> Markup {
        let title = format!("{} — Test", ctx.title());
        ctx.set_title(title);
        ctx.suppress_default_styles();
        html! {
            meta name="test-head";
            meta name="test-base" content=(ctx.base_path());
        }
    }

    async fn body(
        &self,
        ctx: &BodyCtx<'_, TestHost>,
        content: &(dyn Fn(&BodyCtx<'_, TestHost>) -> Markup + Sync),
    ) -> Markup {
        let kind = match ctx.kind() {
            PageKind::Document(meta) => format!("document {}", meta.path()),
            PageKind::FolderIndex => "folder index".to_string(),
            PageKind::Search { query } => format!("search {query}"),
            PageKind::Tag => "tag".to_string(),
            PageKind::Disambiguation => "disambiguation".to_string(),
            PageKind::NotFound => "not found".to_string(),
        };
        let probe = ctx
            .request()
            .headers
            .get("x-probe")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        html! {
            div.host-nav { "host nav" }
            div.host-search { (ctx.search_box()) }
            div.host-crumbs { (ctx.breadcrumbs()) }
            p #kind { (kind) }
            p #probe { (probe) }
            p #base { (ctx.base_path()) }
            (content(ctx))
        }
    }
}

async fn app() -> (tempfile::TempDir, Router) {
    app_at("/").await
}

async fn app_at(base: &str) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("Page.md", "# Page\n\nSee [[ABC-1]] and [[Other]]. #topic\n");
    write("Other.md", "# Other\n");
    write("sub/Deep.md", "# Deep\n");
    let wiki = Wiki::builder(dir.path())
        .host(TestHost, TestTemplate)
        .base_path(base)
        .stylesheet("body { --probe-host: 1 }")
        .build()
        .await
        .unwrap();
    wiki.indexed().await;
    (dir, wiki.router())
}

async fn get(app: &Router, uri: &str, probe: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header("x-probe", probe)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn ok(app: &Router, uri: &str) -> String {
    let (status, body) = get(app, uri, "").await;
    assert_eq!(status, StatusCode::OK, "{uri}");
    body
}

#[tokio::test]
async fn a_claimed_wikilink_is_drawn_by_the_host() {
    let (_dir, app) = app().await;
    let body = ok(&app, "/Page").await;
    assert!(
        body.contains("<span class=\"ticket\">ABC-1</span>"),
        "{body}"
    );
    assert!(!body.contains("to=ABC-1"), "{body}");
    // `[[Other]]` is the page's first wikilink.
    assert!(body.contains("href=\"/Other\" id=\"wl-0\""), "{body}");

    let refs = ok(&app, "/_wiki/link?to=ABC-1").await;
    assert!(!refs.contains("Pages that link here"), "{refs}");
    let refs = ok(&app, "/_wiki/link?to=Other").await;
    assert!(refs.contains("<a href=\"/Page#wl-0\">Page</a>"), "{refs}");
}

#[tokio::test]
async fn a_host_elements_text_is_searchable() {
    let (_dir, app) = app().await;
    let body = ok(&app, "/_wiki/search?q=ABC-1").await;
    assert!(body.contains("<a href=\"/Page\">Page</a>"), "{body}");
}

#[tokio::test]
async fn the_head_hook_sets_the_title_and_drops_the_builtin_style() {
    let (_dir, app) = app().await;
    for (uri, title) in [
        ("/Page", "Page — Test"),
        ("/", "Index — Test"),
        ("/_wiki/search?q=x", "Search: x — Test"),
    ] {
        let body = ok(&app, uri).await;
        assert!(body.contains(&format!("<title>{title}</title>")), "{body}");
        // The built-in stylesheet is gone; highlighting and the builder's
        // stylesheet stay.
        assert!(!body.contains("--unresolved"), "{uri}");
        assert!(body.contains("rose-pine-dawn.min.css"), "{uri}");
        assert!(body.contains("--probe-host"), "{uri}");
        // Host markup comes after the library's head contents.
        let host = body.find("<meta name=\"test-head\">").unwrap();
        assert!(body.find("sse.min.js").unwrap() < host, "{uri}");
        assert!(host < body.find("</head>").unwrap(), "{uri}");
    }
}

#[tokio::test]
async fn a_custom_body_places_the_chrome_inside_the_htmx_root() {
    let (_dir, app) = app().await;
    let body = ok(&app, "/sub/Deep").await;
    let root = body.find("<div id=\"htmx-root\"").unwrap();
    let end = body.find("</div><script").unwrap();
    let inside = &body[root..end];
    let at = |needle: &str| {
        inside
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} not inside #htmx-root: {body}"))
    };
    assert!(at("host nav") < at("role=\"search\""));
    assert!(
        at("role=\"search\"")
            < at("<nav><a href=\"/\">Index</a> / <a href=\"/sub/\">sub</a></nav>")
    );
    assert!(
        at("<nav>")
            < at(
                "<h1 data-line=\"1\" id=\"deep\">Deep<a class=\"anchor\" href=\"#deep\">#</a></h1>"
            )
    );
    // The default layout's wrappers are the template's to draw, not the
    // library's.
    assert!(!body.contains("<header>"), "{body}");
    assert!(!body.contains("<main>"), "{body}");
    // Live-update wiring is still the library's.
    // The stream carries the page's tab id, which differs per page load.
    assert!(
        body.contains("<body hx-ext=\"sse\" sse-connect=\"/_wiki/events/sub/Deep?tab="),
        "{body}"
    );
    assert!(body[root..].starts_with(
        "<div id=\"htmx-root\" hx-get=\"/sub/Deep\" hx-trigger=\"sse:changed\" \
         hx-select=\"#htmx-root\" hx-swap=\"outerHTML\" hx-disinherit=\"*\">"
    ));
}

#[tokio::test]
async fn kind_matches_each_page_type() {
    let (_dir, app) = app().await;
    for (uri, status, kind) in [
        ("/Page", StatusCode::OK, "document Page.md"),
        ("/sub/Deep", StatusCode::OK, "document sub/Deep.md"),
        ("/", StatusCode::OK, "folder index"),
        ("/sub/", StatusCode::OK, "folder index"),
        ("/_wiki/search?q=x", StatusCode::OK, "search x"),
        ("/_wiki/tag/topic", StatusCode::OK, "tag"),
        ("/_wiki/link?to=Nowhere", StatusCode::OK, "disambiguation"),
        ("/Missing", StatusCode::NOT_FOUND, "not found"),
    ] {
        let (got, body) = get(&app, uri, "").await;
        assert_eq!(got, status, "{uri}");
        assert!(
            body.contains(&format!("<p id=\"kind\">{kind}</p>")),
            "{uri}: {body}"
        );
    }
}

#[tokio::test]
async fn the_request_is_visible_to_the_template() {
    let (_dir, app) = app().await;
    for uri in ["/Page", "/", "/Missing"] {
        let (_, body) = get(&app, uri, "token-123").await;
        assert!(
            body.contains("<p id=\"probe\">token-123</p>"),
            "{uri}: {body}"
        );
    }
}

#[tokio::test]
async fn the_base_path_is_visible_to_the_template() {
    for (base, uri, want) in [
        ("/", "/Page", ""),
        ("/docs", "/docs/Page", "/docs"),
        ("/docs/", "/docs/_wiki/search?q=x", "/docs"),
    ] {
        let (_dir, app) = app_at(base).await;
        let body = ok(&app, uri).await;
        assert!(
            body.contains(&format!("<meta name=\"test-base\" content=\"{want}\">")),
            "{base}: {body}"
        );
        assert!(
            body.contains(&format!("<p id=\"base\">{want}</p>")),
            "{base}: {body}"
        );
    }
}
