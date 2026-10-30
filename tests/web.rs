use agent_wiki::Wiki;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use tower::ServiceExt;

const FIXTURE: &str = "tests/fixtures/site";

struct Reply {
    status: StatusCode,
    location: Option<String>,
    body: String,
}

async fn get_path(app: &Router, uri: &str) -> Reply {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        location,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

/// Status, content type and raw body bytes.
async fn get_raw(app: &Router, uri: &str) -> (StatusCode, Option<String>, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, content_type, bytes.to_vec())
}

async fn root_app() -> Router {
    let wiki = Wiki::builder(FIXTURE).build().await.unwrap();
    wiki.indexed().await;
    wiki.router()
}

async fn ok_body(app: &Router, uri: &str) -> String {
    let reply = get_path(app, uri).await;
    assert_eq!(reply.status, StatusCode::OK, "{uri}");
    reply.body
}

/// The `<a ...>` tag whose attributes contain `needle`.
fn anchor_containing<'a>(body: &'a str, needle: &str) -> &'a str {
    let at = body
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in body"));
    let start = body[..at].rfind("<a ").unwrap();
    let end = at + body[at..].find('>').unwrap();
    &body[start..=end]
}

/// The `<section>`-like slice of the body after `heading` up to the next `<h2>` or end.
fn section<'a>(body: &'a str, heading: &str) -> &'a str {
    let start = body
        .find(heading)
        .unwrap_or_else(|| panic!("{heading} missing"));
    let rest = &body[start + heading.len()..];
    let end = rest.find("<h2>").unwrap_or(rest.len());
    &rest[..end]
}

#[tokio::test]
async fn home_page_renders() {
    let app = root_app().await;
    let body = ok_body(&app, "/Home").await;

    assert!(body.contains("<title>Welcome Home</title>"));
    let details = section(&body, "<details>");
    assert!(details.contains("<table>"));
    assert!(details.contains("<th>status</th>"));

    assert_eq!(
        body.matches("href=\"/2021-01-01-foo-example\"").count(),
        2,
        "[[Foo Example]] and [[FOO_example]]"
    );
    assert!(body.contains("href=\"/_wiki/link?to=Shared%20Name\""));
    assert!(body.contains("href=\"/Projects/Alpha/Shared%20Name\""));
    assert!(body.contains("href=\"/Projects/Beta/shared_name\""));
    let missing = anchor_containing(&body, "href=\"/_wiki/link?to=Missing%20Page\"");
    assert!(
        missing.contains("class=\"wikilink unresolved\""),
        "{missing}"
    );
    assert!(body.contains("class=\"wikilink unresolved\">the label</a>"));
    assert!(body.contains("id=\"wl-0\""));

    assert!(body.contains("<pre data-line=\"21\" class=\"mermaid\">"));
    assert!(body.contains("A --&gt; B"));
    assert!(body.contains("<code class=\"language-rust\">"));
    assert!(body.contains("<div data-line=\"26\" class=\"raw-html-check\">raw <b>html</b></div>"));
    assert!(body.contains("First line<br"));
    assert!(body.contains("<ul data-line=\"28\" class=\"contains-task-list\">"));
    for status in [" ", "x", "/", "-"] {
        let item =
            format!("class=\"task-list-item\" data-task=\"{status}\"><input type=\"checkbox\"");
        assert!(body.contains(&item), "{item}");
    }
    assert!(body.contains("box.indeterminate = true"));
    assert!(body.contains(".task-list-item[data-task=\"-\"] .task-list-item-checkbox {"));
    assert!(body.contains(".task-list-item[data-task=\"-\"] .task-list-item-checkbox:checked {"));
    assert!(
        body.contains(".task-list-item[data-task=\"-\"] .task-list-item-checkbox:indeterminate {")
    );
    assert!(body.contains("<code>#notatag</code>"));
    assert!(!body.contains("/_wiki/tag/notatag"));
    assert!(body.contains("href=\"/_wiki/tag/project/alpha\""));
    assert!(body.contains("id=\"tag-0\""));

    let tag_line = section(&body, "<p class=\"tags\">");
    assert!(tag_line.contains("href=\"/_wiki/tag/meta\""));

    assert!(body.contains("@highlightjs/cdn-assets@11.12.0/highlight.min.js"));
    assert!(body.contains("pele@1/dist/pele.js"));
}

#[tokio::test]
async fn missing_link_disambiguation() {
    let app = root_app().await;
    let body = ok_body(&app, "/_wiki/link?to=Missing%20Page").await;
    assert!(!body.contains("Matching pages"));
    let refs = section(&body, "<h2>Pages that link here</h2>");
    for (title, href) in [
        ("Welcome Home", "/Home#wl-8"),
        ("Shared Name", "/Projects/Alpha/Shared%20Name#wl-0"),
        ("Nested Note", "/Projects/Beta/Deep/Nested%20Note#wl-0"),
    ] {
        assert!(
            refs.contains(&format!("<a href=\"{href}\">{title}</a>")),
            "{title}: {refs}"
        );
    }
}

#[tokio::test]
async fn ambiguous_link_disambiguation() {
    let app = root_app().await;
    let body = ok_body(&app, "/_wiki/link?to=Shared%20Name").await;
    let matching = section(&body, "<h2>Matching pages</h2>");
    assert!(matching.contains(">Shared Name</a>"));
    assert!(matching.contains(">Beta Shared</a>"));
    let refs = section(&body, "<h2>Pages that link here</h2>");
    assert!(refs.contains(">Welcome Home</a>"));
    assert_eq!(refs.matches("<li>").count(), 1);
}

#[tokio::test]
async fn tag_page_lists_tagged_pages() {
    let app = root_app().await;
    let body = ok_body(&app, "/_wiki/tag/status").await;
    assert!(body.contains("<a href=\"/Home#tag-1\">Welcome Home</a>"));
    assert!(body.contains("<a href=\"/Projects/Alpha/Shared%20Name#tag-0\">Shared Name</a>"));

    let body = ok_body(&app, "/_wiki/tag/meta").await;
    assert!(body.contains("<a href=\"/Home\">Welcome Home</a>"));
}

#[tokio::test]
async fn folder_indexes() {
    let app = root_app().await;
    let body = ok_body(&app, "/").await;
    for text in ["Notes/", "Projects/", "Foo, Dated", "Welcome Home"] {
        assert!(body.contains(text), "{text}");
    }
    assert!(!body.contains(".hidden"));

    let body = ok_body(&app, "/Projects/Beta/").await;
    assert!(body.contains("<a href=\"/Projects/Beta/Deep/\">Deep/</a>"));
    assert!(body.contains("Beta Shared"));
}

#[tokio::test]
async fn folder_indexes_show_recent_and_tags() {
    let app = root_app().await;
    let body = ok_body(&app, "/").await;
    let recent_at = body.find("<h2>Recently edited</h2>").expect("recent");
    let contents_at = body.find("<h2>Contents</h2>").expect("contents");
    let tags_at = body.find("<h2>Tags</h2>").expect("tags");
    assert!(recent_at < contents_at && contents_at < tags_at, "{body}");
    let recent = section(&body, "<h2>Recently edited</h2>");
    assert_eq!(recent.matches("<li>").count(), 10);
    // Each row has its path from the folder and its edit date.
    assert!(
        recent.contains("<span class=\"file\">Notes/Weather Tagged.md</span><time>"),
        "{recent}"
    );

    let tags = section(&body, "<h2>Tags</h2>");
    assert!(
        tags.contains(">#status <span class=\"count\">(2)</span></a>"),
        "{tags}"
    );
    assert!(tags.contains(">#meta</a>"), "{tags}");
    assert!(!tags.contains("font-size"), "{tags}");

    let body = ok_body(&app, "/Projects/").await;
    let cloud = section(&body, "<h2>Tags</h2>");
    assert!(cloud.contains(">#status</a>"), "{cloud}");
    assert!(!cloud.contains("#meta"), "{cloud}");
    let recent = section(&body, "<h2>Recently edited</h2>");
    let hrefs: Vec<&str> = recent.split("href=\"").skip(1).collect();
    assert!(!hrefs.is_empty(), "{recent}");
    for href in hrefs {
        assert!(href.starts_with("/Projects/"), "{recent}");
    }
    // Paths are relative to the folder shown.
    assert!(
        recent.contains("<span class=\"file\">Beta/Deep/Nested Note.md</span>"),
        "{recent}"
    );

    // `Projects/Beta/Deep` holds one untagged page, so its tag cloud is
    // empty and omitted (#84).
    let body = ok_body(&app, "/Projects/Beta/Deep/").await;
    assert!(
        section(&body, "<h2>Recently edited</h2>").contains(">Nested Note</a>"),
        "{body}"
    );
}

#[tokio::test]
async fn folder_listing_shows_page_details() {
    let app = root_app().await;
    let body = ok_body(&app, "/Notes/").await;
    let contents = section(&body, "<h2>Contents</h2>");
    let at = contents.find(">Weather Tagged</a>").expect("entry");
    let entry = &contents[at..];
    let entry = &entry[..entry.find("</li>").unwrap()];
    assert!(
        entry.contains("<span class=\"file\">Weather Tagged.md</span>"),
        "{entry}"
    );
    assert!(entry.contains("<time>"), "{entry}");
    assert!(
        entry.contains("<p class=\"summary\">Filed under #zephyr only.</p>"),
        "{entry}"
    );
    assert!(
        entry.contains("<a class=\"tag\" href=\"/_wiki/tag/zephyr\">#zephyr</a>"),
        "{entry}"
    );

    let body = ok_body(&app, "/").await;
    let contents = section(&body, "<h2>Contents</h2>");
    assert!(
        contents.contains("<li class=\"folder\"><a href=\"/Notes/\">Notes/</a></li>"),
        "{contents}"
    );
}

#[tokio::test]
async fn breadcrumbs_reach_folder_indexes() {
    let app = root_app().await;
    let body = ok_body(&app, "/Projects/Beta/Deep/Nested%20Note").await;
    let nav = section(&body, "<nav>");
    for href in ["/", "/Projects/", "/Projects/Beta/", "/Projects/Beta/Deep/"] {
        assert!(nav.contains(&format!("href=\"{href}\"")), "{href}: {nav}");
    }
}

#[tokio::test]
async fn redirects_and_not_found() {
    let app = root_app().await;
    let reply = get_path(&app, "/Projects").await;
    assert_eq!(reply.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(reply.location.as_deref(), Some("/Projects/"));

    for uri in [
        "/.hidden/Secret",
        "/Nope",
        "/..%2FCargo",
        "/Nope/",
        "/_wiki/other",
    ] {
        assert_eq!(
            get_path(&app, uri).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn hosted_under_docs() {
    let wiki = Wiki::builder(FIXTURE)
        .base_path("/docs")
        .build()
        .await
        .unwrap();
    wiki.indexed().await;
    let app: Router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(wiki.router());

    let body = ok_body(&app, "/docs/Home").await;
    assert!(body.contains("href=\"/docs/2021-01-01-foo-example\""));
    assert!(body.contains("href=\"/docs/_wiki/link?to=Shared%20Name\""));
    assert!(body.contains("href=\"/docs/_wiki/tag/project/alpha\""));
    assert!(body.contains("href=\"/docs/\""));
    assert!(!body.contains("href=\"/Home"));

    let reply = get_path(&app, "/docs").await;
    assert_eq!(reply.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(reply.location.as_deref(), Some("/docs/"));

    let body = ok_body(&app, "/docs/").await;
    assert!(body.contains("href=\"/docs/Projects/\""));
    assert!(body.contains("href=\"/docs/_wiki/tag/status\""));
    let recent = section(&body, "<h2>Recently edited</h2>");
    assert_eq!(recent.matches("href=\"/docs/").count(), 10, "{recent}");
    let body = ok_body(&app, "/docs/_wiki/link?to=Missing%20Page").await;
    assert!(body.contains("href=\"/docs/Home#wl-8\""));
    let reply = get_path(&app, "/docs/Projects").await;
    assert_eq!(reply.location.as_deref(), Some("/docs/Projects/"));

    assert_eq!(ok_body(&app, "/health").await, "ok");
    assert_eq!(get_path(&app, "/Home").await.status, StatusCode::NOT_FOUND);

    let body = ok_body(&app, "/docs/Projects/Alpha/Links").await;
    assert!(body.contains("href=\"/docs/Home\""));
    assert!(body.contains("src=\"/docs/Projects/Alpha/diagram.png\""));
    // Undecodable relative URLs would resolve above `/docs` in the browser.
    assert!(body.contains("<a class=\"unresolved\">bad bytes</a>"));
    assert!(
        !body.contains("%FF\""),
        "an undecodable URL reached an attribute"
    );
    assert!(!body.contains("logout\""));
    assert!(body.contains("<a href=\"?x=1\">query</a>"));
    let (status, content_type, _) = get_raw(&app, "/docs/Projects/Alpha/diagram.png").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/png"));

    let body = ok_body(&app, "/docs/Home").await;
    assert!(body.contains("action=\"/docs/_wiki/search\""));
    let body = ok_body(&app, "/docs/_wiki/search?q=zephyr").await;
    assert!(body.contains("action=\"/docs/_wiki/search\""));
    assert!(body.contains("<a href=\"/docs/Notes/Zephyr%20Notes\">Zephyr Notes</a>"));
    assert!(body.contains("<a href=\"/docs/Notes/Body%20Only\">Body Only</a>"));

    let body = ok_body(&app, "/docs/Home").await;
    assert!(body.contains("sse-connect=\"/docs/_wiki/events?url=%2Fdocs%2FHome&amp;tab="));
    assert!(body.contains("data-base=\"/docs\""));
    assert!(body.contains("hx-get=\"/docs/Home\""));
}

#[tokio::test]
async fn search_results_rank_title_tag_body() {
    let app = root_app().await;
    let body = ok_body(&app, "/_wiki/search?q=zephyr").await;
    assert!(body.contains("<title>Search: zephyr</title>"), "{body}");
    assert!(body.contains("<h1>Search: zephyr</h1>"), "{body}");
    assert!(body.contains("name=\"q\" placeholder=\"Search\" value=\"zephyr\""));
    let main = section(&body, "<main>");
    let at = |title: &str| {
        main.find(&format!(">{title}</a>"))
            .unwrap_or_else(|| panic!("{title} missing: {main}"))
    };
    assert!(at("Zephyr Notes") < at("Weather Tagged"));
    assert!(at("Weather Tagged") < at("Body Only"));
    assert_eq!(main.matches("<li>").count(), 3, "{main}");
    assert!(main.contains(
        "<li><a href=\"/Notes/Zephyr%20Notes\">Zephyr Notes</a> <span class=\"muted\">Notes</span></li>"
    ));

    let body = ok_body(&app, "/_wiki/search?q=").await;
    assert!(!section(&body, "<main>").contains("<ul>"));
    assert!(!body.contains("No results"));

    let body = ok_body(&app, "/_wiki/search?q=nomatchword").await;
    assert!(body.contains("No results"));

    let body = ok_body(&app, "/_wiki/search?q=%3Cscript%3E").await;
    assert!(!body.contains("Search: <script>"), "{body}");
    assert!(!body.contains("value=\"<script>"), "{body}");
    assert!(body.contains("<title>Search: &lt;script&gt;</title>"));
    assert!(body.contains("<h1>Search: &lt;script&gt;</h1>"));
    assert!(body.contains("value=\"&lt;script&gt;\""));

    let body = ok_body(&app, "/_wiki/search?q=%22%3E%3Cb%3E").await;
    assert!(body.contains("value=\"&quot;&gt;&lt;b&gt;\""), "{body}");

    for uri in ["/Home", "/", "/_wiki/tag/status"] {
        let body = ok_body(&app, uri).await;
        assert!(body.contains("action=\"/_wiki/search\""), "{uri}");
    }
    let reply = get_path(&app, "/Nope").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert!(reply.body.contains("action=\"/_wiki/search\""));
}

#[tokio::test]
async fn ordinary_links_and_images_resolve() {
    let app = root_app().await;
    let body = ok_body(&app, "/Projects/Alpha/Links").await;
    for needle in [
        "href=\"/Projects/Beta/shared_name\"",
        "href=\"/Home\"",
        "href=\"/Home#home-heading\"",
        "href=\"/Projects/Beta/\"",
        "href=\"https://example.com/x.md\"",
        "href=\"mailto:someone@example.com\"",
        "href=\"#links\"",
        "src=\"/Projects/Alpha/diagram.png\" alt=\"diagram\"",
        "src=\"/assets/logo.svg\" alt=\"logo\"",
        "<img src=\"/Projects/Alpha/diagram.png\" alt=\"diagram.png\"",
        "<img src=\"/assets/logo.svg\" alt=\"Site logo\"",
        "<a class=\"unresolved\">escape</a>",
        "<a class=\"unresolved\">abs escape</a>",
        "<a class=\"unresolved\">bad bytes</a>",
        "<img alt=\"bad image\"",
        "<a href=\"?x=1\">query</a>",
        "<a href=\"/Home\">ok</a>",
        "![[Home]]",
    ] {
        assert!(body.contains(needle), "{needle}");
    }
    assert_eq!(
        body.matches("href=\"/Projects/Alpha/Shared%20Name\"")
            .count(),
        2,
        "encoded and angle-bracket links"
    );
    // Nothing that escapes the root reaches an href or src.
    for attr in body.split(['<', '>']).flat_map(|tag| {
        tag.split(" href=\"")
            .skip(1)
            .chain(tag.split(" src=\"").skip(1))
            .map(|rest| rest.split('"').next().unwrap())
            .collect::<Vec<_>>()
    }) {
        for bad in ["outside.md", "passwd", "x.png", "%FF", "admin", "logout"] {
            assert!(!attr.contains(bad), "{attr}");
        }
    }

    let main = section(&body, "<main>");
    let tag = anchor_containing(main, "href=\"/_wiki/tag/linktest\"");
    assert!(tag.contains("id=\"tag-0\""), "{tag}");
    let tag_page = ok_body(&app, "/_wiki/tag/linktest").await;
    assert!(tag_page.contains("href=\"/Projects/Alpha/Links#tag-0\""));
}

#[tokio::test]
async fn only_images_are_served() {
    let app = root_app().await;
    let (status, content_type, bytes) = get_raw(&app, "/Projects/Alpha/diagram.png").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/png"));
    assert_eq!(
        bytes,
        std::fs::read(format!("{FIXTURE}/Projects/Alpha/diagram.png")).unwrap()
    );

    let (status, content_type, _) = get_raw(&app, "/assets/logo.svg").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/svg+xml"));

    for uri in [
        "/Notes/ignored.txt",
        "/..%2FCargo.toml",
        "/.hidden/x.png",
        "/assets/missing.png",
    ] {
        assert_eq!(get_raw(&app, uri).await.0, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn host_stylesheet_layers_after_builtin() {
    let wiki = Wiki::builder(FIXTURE)
        .stylesheet("body { --probe-host: 1 }")
        .build()
        .await
        .unwrap();
    wiki.indexed().await;
    let app: Router = wiki.router();
    for uri in ["/Home", "/"] {
        let body = ok_body(&app, uri).await;
        let builtin = body.find("<style>").unwrap();
        let host = body
            .find("--probe-host")
            .unwrap_or_else(|| panic!("{uri}: no host stylesheet"));
        assert!(body[builtin..host].contains("</style>"), "{uri}");
        assert!(body[builtin + 1..host].contains("<style>"), "{uri}");
    }

    let app = root_app().await;
    for uri in ["/Home", "/"] {
        let body = ok_body(&app, uri).await;
        assert_eq!(body.matches("<style>").count(), 1, "{uri}");
        assert!(!body.contains("--probe-host"), "{uri}");
    }
}

#[tokio::test]
async fn builtin_stylesheet_has_dark_mode() {
    let body = ok_body(&root_app().await, "/Home").await;
    assert!(body.contains("prefers-color-scheme: dark"));
    assert!(body.contains("styles/rose-pine.min.css"));
}

#[tokio::test]
async fn page_views_are_wired_for_live_updates() {
    let app = root_app().await;
    let body = ok_body(&app, "/Home").await;
    for needle in [
        "htmx.org@2.0.11/dist/htmx.min.js",
        "htmx-ext-sse@2.2.4/dist/sse.min.js",
        "<body hx-ext=\"sse\" sse-connect=\"/_wiki/events?url=%2FHome&amp;tab=",
        "hx-get=\"/Home\"",
        "hx-trigger=\"sse:changed\"",
        "hx-select=\"#htmx-root\"",
        "hx-swap=\"outerHTML\"",
        "id=\"htmx-root\"",
        // Descendants (e.g. a host widget's own hx-get) don't inherit the
        // root's hx-select and the rest.
        "hx-swap=\"outerHTML\" hx-disinherit=\"*\">",
        "htmx:afterSwap",
        // Highlighting re-runs on a swap of the root, not of a host widget.
        "e.detail.target?.id === 'htmx-root'",
    ] {
        assert!(body.contains(needle), "{needle}");
    }
    // The SSE connection is on <body>, outside the swapped root; the search
    // form, breadcrumbs, tags and the page body are inside it.
    let root = body.find("id=\"htmx-root\"").unwrap();
    assert!(body.find("sse-connect").unwrap() < root);
    let inside = &body[root..body.find("</div><script").unwrap()];
    for needle in [
        "role=\"search\"",
        "<nav>",
        "class=\"tags\"",
        "<main>",
        "</main>",
    ] {
        assert!(inside.contains(needle), "{needle} outside #htmx-root");
    }

    let body = ok_body(&app, "/Projects/Beta/Deep/Nested%20Note").await;
    assert!(body.contains(
        "sse-connect=\"/_wiki/events?url=%2FProjects%2FBeta%2FDeep%2FNested%2520Note&amp;tab="
    ));

    // Folder index pages are live, but not counted as open documents.
    for (uri, url) in [("/", "%2F"), ("/Projects/Beta/", "%2FProjects%2FBeta%2F")] {
        let body = ok_body(&app, uri).await;
        assert!(
            body.contains(&format!("sse-connect=\"/_wiki/events?url={url}\"")),
            "{uri}"
        );
        assert!(body.contains(&format!("hx-get=\"{uri}\"")), "{uri}");
    }

    let body = ok_body(&app, "/_wiki/search?q=x").await;
    assert!(body.contains("htmx.org@2.0.11/dist/htmx.min.js"));
    assert!(body.contains("id=\"htmx-root\""));
    assert!(!body.contains("sse-connect"));
    assert!(!body.contains("hx-trigger"));
}

#[tokio::test]
async fn invalid_event_paths_are_not_found() {
    let app = root_app().await;
    for uri in ["/_wiki/events?url=x", "/_wiki/events?url=", "/_wiki/events"] {
        assert_eq!(
            get_path(&app, uri).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn document_pages_have_a_table_of_contents() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("long.md"),
        "---\nstatus: draft\n---\n# Long\n\n## Part\n\n> ## Quoted\n\nText.\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("flat.md"), "No headings.\n").unwrap();
    let wiki = Wiki::builder(dir.path()).build().await.unwrap();
    wiki.indexed().await;
    let app = wiki.router();

    let body = ok_body(&app, "/long").await;
    let properties = body.find("<summary>Properties</summary>").unwrap();
    let toc = body.find("<details class=\"toc\">").unwrap();
    let content = body.find("<h1").unwrap();
    assert!(properties < toc && toc < content, "{body}");
    let toc = &body[toc..content];
    assert!(
        toc.contains("<a href=\"/long#long\">Long</a><ul><li><a href=\"/long#part\">Part</a>"),
        "{toc}"
    );
    assert!(!toc.contains("Quoted"), "{toc}");

    let body = ok_body(&app, "/flat").await;
    assert!(!body.contains("class=\"toc\""), "{body}");
}

#[tokio::test]
async fn the_reference_is_served_under_wiki() {
    let app = root_app().await;
    let index = ok_body(&app, "/_wiki/reference/").await;
    // The prompt, then the index.
    let prompt = index.find("agent_wiki reference").unwrap();
    let listing = index.find("/_wiki/reference/onboarding").unwrap();
    assert!(prompt < listing, "{index}");
    let page = ok_body(&app, "/_wiki/reference/onboarding").await;
    assert!(page.contains("Onboarding"), "{page}");
    let reply = get_path(&app, "/_wiki/reference").await;
    assert_eq!(reply.location.as_deref(), Some("/_wiki/reference/"));
    // Not in the wiki's own index or search.
    let root = ok_body(&app, "/").await;
    assert!(!root.contains("_wiki/reference"), "{root}");
    let search = ok_body(&app, "/_wiki/search?q=onboarding").await;
    assert!(!search.contains("/_wiki/reference/"), "{search}");
}
