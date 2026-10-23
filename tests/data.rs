//! The data layer: folders and documents, read the way a host would.

use agent_wiki::{BlockKind, Entry, KindRef, SearchOpts, Wiki};

fn site() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("home.md", "# Home\n\nSee [[Alpha]] and [[Nowhere]]. #top\n");
    write(
        "Projects/Alpha.md",
        "---\ntitle: Alpha Project\ntags: [project]\n---\nAlpha is about rockets.\n\n## Status\n\nGreen. #project\n",
    );
    write(
        "Projects/Beta.md",
        "# Beta\n\nRockets again, and [[Alpha]].\n",
    );
    write(
        "Projects/Deep/Gamma.md",
        "# Gamma\n\nNo rockets here. #project\n",
    );
    write(".hidden/secret.md", "# Secret\n");
    dir
}

async fn wiki(dir: &tempfile::TempDir) -> Wiki {
    let wiki = Wiki::builder(dir.path()).build().await.unwrap();
    wiki.indexed().await;
    wiki
}

fn paths<P: agent_wiki::Host>(docs: &[agent_wiki::Document<P>]) -> Vec<&str> {
    docs.iter().map(|d| d.path()).collect()
}

#[tokio::test]
async fn lookup_finds_folders_and_documents() {
    let dir = site();
    let wiki = wiki(&dir).await;
    let Some(Entry::Folder(root)) = wiki.lookup("").await.unwrap() else {
        panic!("root is a folder");
    };
    assert_eq!(root.path(), "");
    let Some(Entry::Folder(projects)) = wiki.lookup("/Projects/").await.unwrap() else {
        panic!("Projects is a folder");
    };
    assert_eq!(
        (projects.name(), projects.url()),
        ("Projects", "/Projects/".to_string())
    );
    for path in ["Projects/Alpha", "Projects/Alpha.md"] {
        let Some(Entry::Document(doc)) = wiki.lookup(path).await.unwrap() else {
            panic!("{path} is a document");
        };
        assert_eq!(doc.path(), "Projects/Alpha.md");
    }
    for path in [
        "Projects/Nope",
        ".hidden/secret",
        "Projects/../home",
        "a//b",
    ] {
        assert!(wiki.lookup(path).await.unwrap().is_none(), "{path}");
    }
}

#[tokio::test]
async fn document_accessors() {
    let dir = site();
    let wiki = wiki(&dir).await;
    let Some(Entry::Document(doc)) = wiki.lookup("Projects/Alpha").await.unwrap() else {
        panic!();
    };
    assert_eq!(doc.title(), "Alpha Project");
    assert_eq!(doc.url(), "/Projects/Alpha");
    assert_eq!(doc.folder(), "Projects");
    assert_eq!(doc.tags(), ["project"]);
    assert_eq!(doc.summary(), "Alpha is about rockets.");
    assert!(doc.edited().is_some());
    assert_eq!(doc.frontmatter().unwrap()["title"], "Alpha Project");
    let outline: Vec<_> = doc
        .outline()
        .iter()
        .map(|h| (h.level, h.text.as_str()))
        .collect();
    assert_eq!(outline, [(2, "Status")]);

    // The tree reads back: the heading, with its line and text.
    let headings: Vec<_> = doc
        .tree()
        .nodes()
        .filter(|n| matches!(n.kind(), KindRef::Block(BlockKind::Heading { .. })))
        .map(|n| (n.line(), n.text()))
        .collect();
    assert_eq!(headings, [(Some(7), "Status".to_string())]);

    // The body draws with its wikilinks resolved.
    let Some(Entry::Document(home)) = wiki.lookup("home").await.unwrap() else {
        panic!();
    };
    let body = home.body().await.unwrap();
    let html = wiki.draw(&body).into_string();
    assert!(html.contains("href=\"/Projects/Alpha\""), "{html}");
    assert!(html.contains("wikilink unresolved"), "{html}");
}

#[tokio::test]
async fn folder_contents() {
    let dir = site();
    let wiki = wiki(&dir).await;
    let root = wiki.root();
    let folders: Vec<String> = root
        .folders()
        .await
        .unwrap()
        .iter()
        .map(|f| f.path().to_string())
        .collect();
    assert_eq!(folders, ["Projects"]);
    assert_eq!(paths(&root.documents().await.unwrap()), ["home.md"]);

    let Some(Entry::Folder(projects)) = wiki.lookup("Projects").await.unwrap() else {
        panic!();
    };
    // Sorted by title: "Alpha Project", "Beta".
    assert_eq!(
        paths(&projects.documents().await.unwrap()),
        ["Projects/Alpha.md", "Projects/Beta.md"]
    );
    assert_eq!(projects.recent(10).await.unwrap().len(), 3);
    assert_eq!(
        projects.tags(10).await.unwrap(),
        [("project".to_string(), 2)]
    );

    let mut walk = projects.walk().await.unwrap();
    let mut walked = Vec::new();
    while let Some(doc) = walk.next().await {
        walked.push(doc.unwrap().path().to_string());
    }
    assert_eq!(
        walked,
        [
            "Projects/Alpha.md",
            "Projects/Beta.md",
            "Projects/Deep/Gamma.md"
        ]
    );
}

#[tokio::test]
async fn search_scoped_and_limited() {
    let dir = site();
    let wiki = wiki(&dir).await;
    let all = wiki.search("rockets", SearchOpts::default()).await.unwrap();
    assert_eq!(all.len(), 3);
    let one = wiki
        .search("rockets", SearchOpts { limit: Some(1) })
        .await
        .unwrap();
    assert_eq!(one.len(), 1);
    let Some(Entry::Folder(deep)) = wiki.lookup("Projects/Deep").await.unwrap() else {
        panic!();
    };
    let scoped = deep.search("rockets", SearchOpts::default()).await.unwrap();
    assert_eq!(paths(&scoped), ["Projects/Deep/Gamma.md"]);
}

#[tokio::test]
async fn tags_and_links() {
    let dir = site();
    let wiki = wiki(&dir).await;
    let tagged = wiki.tagged("PROJECT").await.unwrap();
    let tagged: Vec<_> = tagged.iter().map(|(d, n)| (d.path(), *n)).collect();
    // Alpha's only inline tag is its first; Gamma's too.
    assert_eq!(
        tagged,
        [
            ("Projects/Alpha.md", Some(0)),
            ("Projects/Deep/Gamma.md", Some(0))
        ]
    );

    let alpha = wiki.link_target("Alpha").await.unwrap().unwrap();
    assert_eq!(paths(alpha.matches()), ["Projects/Alpha.md"]);
    let refs: Vec<_> = alpha
        .references()
        .iter()
        .map(|(d, n)| (d.path(), *n))
        .collect();
    assert_eq!(refs, [("Projects/Beta.md", 0), ("home.md", 0)]);

    let nowhere = wiki.link_target("Nowhere").await.unwrap().unwrap();
    assert!(nowhere.matches().is_empty());
    assert_eq!(nowhere.references().len(), 1);
}

/// A daily note with sections to cut out, served under `/docs`.
async fn daily() -> (tempfile::TempDir, Wiki) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Daily")).unwrap();
    std::fs::write(dir.path().join("Projects.md"), "# Projects\n").unwrap();
    std::fs::write(
        dir.path().join("Daily/today.md"),
        "# Today\n\n\
         ## Today's focus\n\n\
         - ship [[Projects]] and [[Elsewhere]]\n\
         - see [below](#later), [line](#:3), [[#Later]], [notes](notes.md), ![](pic.png)\n\n\
         ### Detail\n\n\
         More.\n\n\
         > ## Quoted\n\n\
         ## Later\n\n\
         Not in focus.\n",
    )
    .unwrap();
    let wiki = Wiki::builder(dir.path())
        .base_path("/docs")
        .build()
        .await
        .unwrap();
    wiki.indexed().await;
    (dir, wiki)
}

#[tokio::test]
async fn section_runs_to_the_next_heading_at_its_level() {
    let (_dir, wiki) = daily().await;
    let Some(Entry::Document(doc)) = wiki.lookup("Daily/today").await.unwrap() else {
        panic!();
    };
    let anchor = &doc
        .outline()
        .iter()
        .find(|h| h.text == "Today's focus")
        .unwrap()
        .anchor;
    // The quoted heading isn't in the outline.
    let outline: Vec<_> = doc.outline().iter().map(|h| h.text.as_str()).collect();
    assert_eq!(outline, ["Today", "Today's focus", "Detail", "Later"]);
    let section = doc.section(anchor).unwrap();
    assert_eq!(section.heading().text(), "Today's focus");
    let lines: Vec<_> = section.blocks().map(|n| n.line()).collect();
    // The heading, the list, the subsection's heading and paragraph, and the
    // quote; not `## Later`.
    assert_eq!(lines, [Some(3), Some(5), Some(8), Some(10), Some(12)]);

    // The whole of the last section; none for a heading in a quote.
    assert_eq!(doc.section("later").unwrap().blocks().count(), 2);
    assert!(doc.section("quoted").is_none());
    assert!(doc.section("nowhere").is_none());
}

#[tokio::test]
async fn chunk_draws_off_its_page() {
    let (_dir, wiki) = daily().await;
    let Some(Entry::Document(doc)) = wiki.lookup("Daily/today").await.unwrap() else {
        panic!();
    };
    let chunk = doc
        .chunk(doc.section("todays-focus").unwrap())
        .await
        .unwrap();
    drop(doc);
    let html = wiki.draw(&chunk).into_string();
    let page = "/docs/Daily/today";
    for want in [
        "href=\"/docs/Projects\"",
        "wikilink unresolved",
        &format!("href=\"{page}#todays-focus\""),
        &format!("href=\"{page}#later\""),
        &format!("href=\"{page}#:3\""),
        "href=\"/docs/Daily/notes\"",
        "src=\"/docs/Daily/pic.png\"",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // `[[#Later]]` too: two links to it.
    assert_eq!(html.matches(&format!("{page}#later")).count(), 2, "{html}");
    assert!(!html.contains("data-line"), "{html}");
    assert!(!html.contains("Not in focus"), "{html}");

    // Any nodes, e.g. just the list.
    let Some(Entry::Document(doc)) = wiki.lookup("Daily/today").await.unwrap() else {
        panic!();
    };
    let list = doc
        .tree()
        .blocks()
        .find(|n| matches!(n.kind(), KindRef::Block(BlockKind::List { .. })))
        .unwrap();
    let html = wiki.draw(&doc.chunk([list]).await.unwrap()).into_string();
    assert!(html.starts_with("<ul>"), "{html}");
    assert!(!html.contains("<h2"), "{html}");

    // Not another document's.
    let Some(Entry::Document(other)) = wiki.lookup("Projects").await.unwrap() else {
        panic!();
    };
    let heading = other.tree().blocks().next().unwrap();
    assert!(doc.chunk([heading]).await.is_err());
}
