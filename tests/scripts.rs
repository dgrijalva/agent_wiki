//! The script filter through the web view: the permissive default, the
//! blocking configuration, an allow-list, and a template's own scripts.

use std::sync::{Arc, Mutex};

use agent_wiki::maud::{Markup, PreEscaped, html};
use agent_wiki::{BodyCtx, HeadCtx, Host, Never, PageKind, Script, ScriptKind, Template, Wiki};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

/// A note with one script of each kind: inline, external, an event handler,
/// and `javascript:` links, written as raw HTML and as markdown.
const PROBE: &str = "# Probe

<script>document.title = 'inline ran'</script>

<script src=\"https://evil.example/steal.js\"></script>

<img src=\"missing.png\" onerror=\"document.title = 'onerror ran'\">

[markdown link](javascript:document.title='link-ran')

<a href=\"javascript:document.title='raw link ran'\">raw link</a>

Split across inline HTML: <script>document.title = 'split ran'</script>
";

const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";

struct Reply {
    status: StatusCode,
    csp_header: Option<String>,
    body: String,
}

async fn get(app: &Router, uri: &str) -> Reply {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let csp_header = response
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        csp_header,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn ok(app: &Router, uri: &str) -> String {
    let reply = get(app, uri).await;
    assert_eq!(reply.status, StatusCode::OK, "{uri}");
    reply.body
}

fn vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("Probe.md", PROBE);
    write("Dashboards/Board.md", PROBE);
    write("drawing.svg", SVG);
    dir
}

/// The page's policy, from its `<meta http-equiv>`.
fn csp(body: &str) -> Option<&str> {
    let start = "http-equiv=\"Content-Security-Policy\" content=\"";
    let at = body.find(start)? + start.len();
    Some(&body[at..at + body[at..].find('"').unwrap()])
}

/// The page's policy's `script-src` sources.
fn policy_sources(body: &str) -> Vec<&str> {
    let csp = csp(body).expect("a policy");
    let script_src = csp.split(';').next().unwrap();
    script_src.split_whitespace().skip(1).collect()
}

fn hash(code: &str) -> String {
    let digest = Sha256::digest(code.as_bytes());
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// Each `<script>` in the page: its `src`, or its code.
fn scripts(body: &str) -> Vec<Result<&str, &str>> {
    body.match_indices("<script")
        .map(|(at, _)| {
            let tag_end = at + body[at..].find('>').unwrap();
            let tag = &body[at..tag_end];
            match tag.split_once(" src=\"") {
                Some((_, rest)) => Ok(&rest[..rest.find('"').unwrap()]),
                None => {
                    let code = &body[tag_end + 1..];
                    Err(&code[..code.find("</script>").unwrap()])
                }
            }
        })
        .collect()
}

/// True when the policy lets `script` run.
fn allows(sources: &[&str], script: Result<&str, &str>) -> bool {
    match script {
        Ok(src) => sources.iter().any(|s| {
            src == *s
                || (s.ends_with('/') && src.starts_with(s))
                || (*s == "'self'" && src.starts_with('/'))
        }),
        Err(code) => sources.contains(&hash(code).as_str()),
    }
}

#[tokio::test]
async fn the_default_runs_every_script_as_written() {
    let dir = vault();
    let wiki = Wiki::builder(dir.path()).build().await.unwrap();
    wiki.indexed().await;
    let app: Router = wiki.router();
    let body = ok(&app, "/Probe").await;
    assert!(csp(&body).is_none(), "{body}");
    for needle in [
        "<script data-line=\"3\">document.title = 'inline ran'</script>",
        "<script data-line=\"5\" src=\"https://evil.example/steal.js\"></script>",
        "onerror=\"document.title = 'onerror ran'\"",
        "href=\"javascript:document.title=&#x27;link-ran&#x27;\"",
        "<a href=\"javascript:document.title='raw link ran'\">",
        "<script>document.title = 'split ran'</script>",
    ] {
        assert!(body.contains(needle), "{needle}");
    }
    assert!(!body.contains("script blocked"));
    let svg = get(&app, "/drawing.svg").await;
    assert_eq!(svg.status, StatusCode::OK);
    assert_eq!(svg.csp_header, None);
}

#[tokio::test]
async fn the_blocking_filter_runs_no_content_script() {
    let dir = vault();
    let wiki = Wiki::builder(dir.path())
        .script_filter(|_| false)
        .build()
        .await
        .unwrap();
    wiki.indexed().await;
    let app: Router = wiki.router();
    for uri in [
        "/Probe",
        "/Dashboards/Board",
        "/",
        "/_wiki/search?q=probe",
        "/missing",
    ] {
        let reply = get(&app, uri).await;
        let body = reply.body;
        let sources = policy_sources(&body);
        // Only hashes and exact library URLs: nothing that lets a written
        // script, a handler or `eval` run.
        for source in &sources {
            assert!(
                source.starts_with("'sha256-")
                    || source.starts_with("https://cdn.jsdelivr.net/npm/"),
                "{uri}: {source}"
            );
        }
        assert!(
            csp(&body).unwrap().ends_with("; object-src 'none'"),
            "{uri}"
        );
        // Every script left on the page is the library's, and allowed.
        for script in scripts(&body) {
            assert!(allows(&sources, script), "{uri}: {script:?}");
        }
        for needle in [" ran'", "-ran'", "evil.example", "onerror", "javascript:"] {
            assert!(!body.contains(needle), "{uri}: {needle}");
        }
    }
    let body = ok(&app, "/Probe").await;
    assert_eq!(body.matches("<!-- script blocked -->").count(), 3, "{body}");
    assert!(
        body.contains("<img data-line=\"7\" src=\"missing.png\">"),
        "{body}"
    );
    assert!(body.contains("<a>markdown link</a>"), "{body}");
    assert!(body.contains("<a>raw link</a>"), "{body}");
    // The policy comes before any script.
    assert!(body.find("Content-Security-Policy").unwrap() < body.find("<script").unwrap());

    let svg = get(&app, "/drawing.svg").await;
    assert_eq!(svg.status, StatusCode::OK);
    assert_eq!(
        svg.csp_header.as_deref(),
        Some("script-src 'none'; object-src 'none'")
    );
    assert_eq!(svg.body, SVG);

    // The reference, served from the wiki, has the same policy.
    let body = ok(&app, "/_wiki/reference/onboarding").await;
    let reference = policy_sources(&body);
    for script in scripts(&body) {
        assert!(allows(&reference, script), "{script:?}");
    }
}

#[tokio::test]
async fn an_allow_list_filter_runs_only_what_it_allows() {
    let dir = vault();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let wiki = Wiki::builder(dir.path())
        .script_filter({
            let seen = seen.clone();
            move |script: &Script| {
                let page = match script.page() {
                    PageKind::Document(doc) => doc.path().to_string(),
                    _ => String::new(),
                };
                seen.lock().unwrap().push((
                    page,
                    script.url().to_string(),
                    format!("{:?}", script.kind()),
                ));
                match script.kind() {
                    ScriptKind::External(src) => src.starts_with("https://evil.example/"),
                    ScriptKind::Inline(_) => matches!(
                        script.page(),
                        PageKind::Document(doc) if doc.path().starts_with("Dashboards/")
                    ),
                    _ => false,
                }
            }
        })
        .build()
        .await
        .unwrap();
    wiki.indexed().await;
    let app: Router = wiki.router();

    let body = ok(&app, "/Probe").await;
    let sources = policy_sources(&body);
    assert!(
        sources.contains(&"https://evil.example/steal.js"),
        "{sources:?}"
    );
    assert!(!sources.contains(&hash("document.title = 'inline ran'").as_str()));
    assert!(!sources.contains(&"'unsafe-hashes'"));
    assert!(
        body.contains("<script data-line=\"5\" src=\"https://evil.example/steal.js\"></script>")
    );
    assert!(
        !body.contains("inline ran") && !body.contains("split ran"),
        "{body}"
    );
    assert!(
        !body.contains("onerror") && !body.contains("javascript:"),
        "{body}"
    );
    assert_eq!(
        seen.lock().unwrap().drain(..).collect::<Vec<_>>(),
        [
            "Inline(\"document.title = 'inline ran'\")",
            "External(\"https://evil.example/steal.js\")",
            "Handler { attribute: \"onerror\", code: \"document.title = 'onerror ran'\" }",
            "Url(\"javascript:document.title='link-ran'\")",
            "Url(\"javascript:document.title='raw link ran'\")",
            "Inline(\"document.title = 'split ran'\")",
        ]
        .map(|kind| (
            "Probe.md".to_string(),
            "/Probe".to_string(),
            kind.to_string()
        ))
    );

    let body = ok(&app, "/Dashboards/Board").await;
    let board = policy_sources(&body);
    for code in [
        "document.title = 'inline ran'",
        "document.title = 'split ran'",
    ] {
        assert!(board.contains(&hash(code).as_str()), "{code}");
        assert!(body.contains(code), "{code}");
    }
    assert!(!body.contains("onerror"), "{body}");
}

struct ScriptedHost;

impl Host for ScriptedHost {
    type Block = Never;
    type Inline = Never;
    type Template = ScriptedTemplate;
}

/// A template with scripts of its own, in its head and in its body.
struct ScriptedTemplate;

impl Template<ScriptedHost> for ScriptedTemplate {
    async fn head(&self, _: &mut HeadCtx<'_, ScriptedHost>) -> Markup {
        html! {
            script src="/static/host.js" {}
            script src="https://host.example/widget.js" {}
            script { (PreEscaped("window.host = 1;")) }
        }
    }

    async fn body(
        &self,
        ctx: &BodyCtx<'_, ScriptedHost>,
        content: &(dyn Fn(&BodyCtx<'_, ScriptedHost>) -> Markup + Sync),
    ) -> Markup {
        html! {
            script { (PreEscaped("window.chrome = 1;")) }
            (content(ctx))
        }
    }
}

#[tokio::test]
async fn a_templates_head_is_trusted_and_its_body_filtered() {
    let dir = vault();
    let wiki = Wiki::builder(dir.path())
        .host(ScriptedHost, ScriptedTemplate)
        .script_filter(|_| false)
        .build()
        .await
        .unwrap();
    wiki.indexed().await;
    let app = Router::new()
        .route(
            "/custom",
            axum::routing::get({
                let wiki = wiki.clone();
                move |request: Request<Body>| async move {
                    let (parts, _) = request.into_parts();
                    let page = wiki.custom_page(&parts, "Custom");
                    axum::response::Html(
                        page.draw(|_| html! { p { "custom" } }).await.into_string(),
                    )
                }
            }),
        )
        .merge(wiki.router());
    for uri in ["/Probe", "/custom"] {
        let body = ok(&app, uri).await;
        let sources = policy_sources(&body);
        for source in [
            "'self'",
            "https://host.example/widget.js",
            hash("window.host = 1;").as_str(),
        ] {
            assert!(sources.contains(&source), "{uri}: {source}");
        }
        assert!(
            body.contains("<script src=\"/static/host.js\"></script>"),
            "{uri}"
        );
        assert!(!body.contains("window.chrome"), "{uri}");
        for script in scripts(&body) {
            assert!(allows(&sources, script), "{uri}: {script:?}");
        }
    }
}
