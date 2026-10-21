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
