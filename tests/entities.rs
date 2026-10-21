//! Registering host entity types through the builder, with the derive.

use agent_wiki::filter::{between, eq, is_in, is_null, like, matches, ne};
use agent_wiki::{
    Direction, DocumentTree, EntityType, Field, FieldKind, Host, KindRef, Never, Records, Sort,
    Wiki,
};

#[derive(EntityType)]
struct Mention {
    #[entity(index)]
    who: String,
    #[entity(fulltext)]
    text: String,
    weight: Option<f64>,
}

#[derive(EntityType)]
#[entity(name = "bad", version = 2)]
struct NotText {
    #[entity(fulltext)]
    count: i64,
}

/// A ticket ID like `ABC-12` written in a paragraph.
#[derive(EntityType, Debug, PartialEq)]
struct Ticket {
    #[entity(index)]
    key: String,
    number: i64,
}

/// Pushes a `Mention` per `@someone` and a `Ticket` per `ABC-N` in each
/// paragraph.
struct MentionHost;

impl Host for MentionHost {
    type Block = Never;
    type Inline = Never;
    type Template = agent_wiki::DefaultTemplate;

    fn extract(&self, _path: &str, doc: &DocumentTree<Self>, out: &mut Records) {
        for block in doc.blocks() {
            if !matches!(
                block.kind(),
                KindRef::Block(agent_wiki::BlockKind::Paragraph)
            ) {
                continue;
            }
            let text = block.text();
            for word in text.split_whitespace() {
                let word = word.trim_end_matches(['.', ',']);
                if let Some(n) = word.strip_prefix("ABC-").and_then(|n| n.parse().ok()) {
                    out.push(Ticket {
                        key: word.to_string(),
                        number: n,
                    });
                }
                if let Some(who) = word.strip_prefix('@') {
                    out.push(Mention {
                        who: who.to_string(),
                        text: text.clone(),
                        weight: None,
                    });
                }
            }
        }
    }
}

#[test]
fn the_derive_describes_the_fields() {
    assert_eq!(<Mention as EntityType>::NAME, "mention");
    assert_eq!(<NotText as EntityType>::NAME, "bad");
    assert_eq!(<NotText as EntityType>::VERSION, 2);
    assert_eq!(
        Mention::fields(),
        [
            Field {
                name: "who",
                kind: FieldKind::Text,
                optional: false,
                index: true,
                fulltext: false
            },
            Field {
                name: "text",
                kind: FieldKind::Text,
                optional: false,
                index: false,
                fulltext: true
            },
            Field {
                name: "weight",
                kind: FieldKind::Real,
                optional: true,
                index: false,
                fulltext: false
            },
        ]
    );
}

#[tokio::test]
async fn a_host_registers_and_extracts_entities() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "Ask @dana about it.\n").unwrap();
    let wiki = Wiki::builder(dir.path())
        .host(MentionHost, agent_wiki::DefaultTemplate)
        .entity::<Mention>()
        .build()
        .await
        .unwrap();
    wiki.indexed().await;
}

#[tokio::test]
async fn invalid_registrations_fail_the_build() {
    let dir = tempfile::tempdir().unwrap();
    let twice = Wiki::builder(dir.path())
        .entity::<Mention>()
        .entity::<Mention>()
        .build()
        .await;
    assert!(format!("{:#}", twice.err().unwrap()).contains("registered twice"));
    #[derive(EntityType)]
    #[entity(name = "tag")]
    struct MyTag {
        label: String,
    }
    let builtin = Wiki::builder(dir.path()).entity::<MyTag>().build().await;
    assert!(format!("{:#}", builtin.err().unwrap()).contains("the library's own"));
    let not_text = Wiki::builder(dir.path()).entity::<NotText>().build().await;
    assert!(format!("{:#}", not_text.err().unwrap()).contains("isn't text"));
}

async fn mention_wiki(dir: &tempfile::TempDir) -> Wiki<MentionHost> {
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("a.md", "Ask @dana about ABC-1.\n\nThen @lee, re ABC-2.\n");
    write("p/b.md", "Ping @dana on ABC-3 today. #urgent\n");
    write("p/c.md", "Nobody here but ABC-4.\n");
    let wiki = Wiki::builder(dir.path())
        .host(MentionHost, agent_wiki::DefaultTemplate)
        .entity::<Mention>()
        .entity::<Ticket>()
        .build()
        .await
        .unwrap();
    wiki.indexed().await;
    wiki
}

fn keys(tickets: &[agent_wiki::Entity<Ticket>]) -> Vec<&str> {
    tickets.iter().map(|t| t.record().key.as_str()).collect()
}

#[tokio::test]
async fn records_come_back_with_their_pages() {
    let dir = tempfile::tempdir().unwrap();
    let wiki = mention_wiki(&dir).await;

    // By default: by page path, then document order.
    let all = wiki.query::<Ticket>().fetch().await.unwrap();
    assert_eq!(keys(&all), ["ABC-1", "ABC-2", "ABC-3", "ABC-4"]);
    let first = &all[0];
    assert_eq!(
        first.record(),
        &Ticket {
            key: "ABC-1".into(),
            number: 1
        }
    );
    assert_eq!(
        (first.path(), first.url(), first.folder()),
        ("a.md", "/a", "")
    );
    assert_eq!((first.ordinal(), all[1].ordinal()), (0, 1));
    assert!(first.edited() > 0);

    let under = wiki.query::<Ticket>().under("p").fetch().await.unwrap();
    assert_eq!(keys(&under), ["ABC-3", "ABC-4"]);
    let doc = wiki.query::<Ticket>().document("a").fetch().await.unwrap();
    assert_eq!(keys(&doc), ["ABC-1", "ABC-2"]);

    let ordered = wiki
        .query::<Ticket>()
        .order_by(Sort::Field("number".into()), Direction::Desc)
        .offset(1)
        .limit(2)
        .fetch()
        .await
        .unwrap();
    assert_eq!(keys(&ordered), ["ABC-3", "ABC-2"]);
}

#[tokio::test]
async fn filters() {
    let dir = tempfile::tempdir().unwrap();
    let wiki = mention_wiki(&dir).await;
    let tickets = |filter| {
        let wiki = wiki.clone();
        async move { wiki.query::<Ticket>().filter(filter).fetch().await.unwrap() }
    };
    assert_eq!(keys(&tickets(eq("key", "ABC-2")).await), ["ABC-2"]);
    assert_eq!(keys(&tickets(ne("number", 2)).await).len(), 3);
    assert_eq!(
        keys(&tickets(between("number", 2, 3)).await),
        ["ABC-2", "ABC-3"]
    );
    assert_eq!(
        keys(&tickets(is_in("key", ["ABC-1", "ABC-4"])).await),
        ["ABC-1", "ABC-4"]
    );
    assert_eq!(
        keys(&tickets(eq("number", 1).or(eq("number", 4)).not()).await),
        ["ABC-2", "ABC-3"]
    );
    assert_eq!(keys(&tickets(like("key", "abc-1")).await), ["ABC-1"]);
    // A string is a value, never a field.
    assert!(tickets(eq("key", "number")).await.is_empty());

    let dana = wiki
        .query::<Mention>()
        .filter(eq("who", "dana"))
        .fetch()
        .await
        .unwrap();
    assert_eq!(dana.len(), 2);
    assert!(dana.iter().all(|m| m.record().weight.is_none()));
    let null = wiki
        .query::<Mention>()
        .filter(is_null("weight"))
        .fetch()
        .await
        .unwrap();
    assert_eq!(null.len(), 3);
    let today = wiki
        .query::<Mention>()
        .filter(matches("text", "today"))
        .fetch()
        .await
        .unwrap();
    assert_eq!(today.len(), 1);
    assert_eq!(today[0].path(), "p/b.md");
}

#[tokio::test]
async fn tickets_on_pages_where_dana_is_mentioned() {
    let dir = tempfile::tempdir().unwrap();
    let wiki = mention_wiki(&dir).await;
    let tickets = wiki
        .query::<Ticket>()
        .on_pages_with::<Mention>(eq("who", "dana"))
        .fetch()
        .await
        .unwrap();
    assert_eq!(keys(&tickets), ["ABC-1", "ABC-2", "ABC-3"]);

    // The built-in tags are entity types too.
    let urgent = wiki
        .query::<Ticket>()
        .on_pages_with::<agent_wiki::Tag>(eq("tag", "urgent"))
        .fetch()
        .await
        .unwrap();
    assert_eq!(keys(&urgent), ["ABC-3"]);
    let tags = wiki.query::<agent_wiki::Tag>().fetch().await.unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].record().first, Some(0));
}

#[tokio::test]
async fn query_errors() {
    let dir = tempfile::tempdir().unwrap();
    let wiki = mention_wiki(&dir).await;
    let err = |r: anyhow::Result<Vec<agent_wiki::Entity<Ticket>>>| format!("{:#}", r.unwrap_err());
    let unknown = wiki.query::<Ticket>().filter(eq("nope", 1)).fetch().await;
    assert!(err(unknown).contains("no field \"nope\""));
    let not_text = wiki
        .query::<Ticket>()
        .filter(matches("key", "x"))
        .fetch()
        .await;
    assert!(err(not_text).contains("isn't full-text"));
    let sort = wiki
        .query::<Ticket>()
        .order_by(Sort::Field("nope".into()), Direction::Asc)
        .fetch()
        .await;
    assert!(err(sort).contains("no field"));
    let unregistered = wiki.query::<NotText>().fetch().await;
    let Err(unregistered) = unregistered else {
        panic!("an unregistered type was queried");
    };
    assert!(format!("{unregistered:#}").contains("isn't registered"));
}
