//! Registering host entity types through the builder, with the derive.

use agent_wiki::{DocumentTree, Entity, Field, FieldKind, Host, KindRef, Never, Records, Wiki};

#[derive(Entity)]
struct Mention {
    #[entity(index)]
    who: String,
    #[entity(fulltext)]
    text: String,
    weight: Option<f64>,
}

#[derive(Entity)]
#[entity(name = "bad", version = 2)]
struct NotText {
    #[entity(fulltext)]
    count: i64,
}

/// Pushes a `Mention` per paragraph naming `@someone`.
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
    assert_eq!(<Mention as Entity>::NAME, "mention");
    assert_eq!(<NotText as Entity>::NAME, "bad");
    assert_eq!(<NotText as Entity>::VERSION, 2);
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
    let not_text = Wiki::builder(dir.path()).entity::<NotText>().build().await;
    assert!(format!("{:#}", not_text.err().unwrap()).contains("isn't text"));
}
