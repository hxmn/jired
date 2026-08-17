//! Property-based conversion round-trips [M2.6].
//!
//! Conversion fidelity is the highest-risk part of jired: a silent loss here
//! corrupts a user's issue on push. Example-based tests only cover the shapes
//! someone thought of, so the contract is pinned by generating random documents
//! instead.
//!
//! Three properties, one per direction:
//!
//! | property | what a failure would mean |
//! |---|---|
//! | AST → Markdown → AST | an edit in the working copy is misread |
//! | AST → ADF → AST | a Cloud issue changes shape on push |
//! | AST → wiki → AST | a Data Center issue changes shape on push |

use jired::convert::ast::{
    normalize_inlines, Block, Document, Inline, ListItem, RawStore, Table, TaskItem,
};
use jired::convert::{adf, markdown, wiki};
use proptest::prelude::*;

/// Text fragments that stress escaping without generating shapes the format
/// itself cannot hold (leading/trailing space, empty strings, newlines).
fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            30 => "[a-zA-Z0-9]{1,8}",
            2 => Just("*".to_string()),
            2 => Just("_".to_string()),
            2 => Just("`".to_string()),
            2 => Just("[".to_string()),
            2 => Just("]".to_string()),
            2 => Just("\\".to_string()),
            2 => Just("#".to_string()),
            2 => Just("|".to_string()),
            2 => Just("~".to_string()),
            2 => Just("<".to_string()),
            2 => Just(">".to_string()),
            1 => Just("PROJ-1".to_string()),
        ],
        1..6,
    )
    .prop_map(|parts| parts.join(" "))
}

fn plain_inline() -> impl Strategy<Value = Inline> {
    prop_oneof![
        6 => text().prop_map(Inline::Text),
        1 => "[a-z]{1,6}".prop_map(Inline::Code),
        1 => (text(), "https://[a-z]{2,6}\\.example/[a-z]{0,5}")
            .prop_map(|(t, href)| Inline::Link {
                text: vec![Inline::Text(t)],
                href,
            }),
        1 => ("[A-Z][a-z]{2,6}", "[a-z0-9]{4,10}")
            .prop_map(|(display, id)| Inline::Mention { id, display }),
    ]
}

/// One level of nesting is enough: marks compose associatively, and deeper
/// trees mostly re-test the same seams while slowing the run down.
fn inline() -> impl Strategy<Value = Inline> {
    prop_oneof![
        8 => plain_inline(),
        1 => prop::collection::vec(plain_inline(), 1..3).prop_map(Inline::Strong),
        1 => prop::collection::vec(plain_inline(), 1..3).prop_map(Inline::Emphasis),
        1 => prop::collection::vec(plain_inline(), 1..3).prop_map(Inline::Strike),
    ]
}

fn inlines() -> impl Strategy<Value = Vec<Inline>> {
    prop::collection::vec(inline(), 1..5).prop_map(normalize_inlines)
}

/// Blocks that every target format can represent.
fn leaf_block() -> impl Strategy<Value = Block> {
    prop_oneof![
        6 => inlines().prop_map(Block::Paragraph),
        2 => (1u8..=6, inlines()).prop_map(|(level, content)| Block::Heading { level, content }),
        2 => (
            prop::option::of("[a-z]{2,6}"),
            "(?s)[a-zA-Z0-9 \n]{1,30}",
        )
            .prop_map(|(language, code)| Block::CodeBlock {
                language,
                code: code.trim_end().to_string(),
            }),
        1 => Just(Block::Rule),
    ]
}

fn list_item() -> impl Strategy<Value = ListItem> {
    inlines().prop_map(|content| ListItem {
        content: vec![Block::Paragraph(content)],
    })
}

fn block() -> impl Strategy<Value = Block> {
    prop_oneof![
        8 => leaf_block(),
        2 => prop::collection::vec(list_item(), 1..4).prop_map(Block::BulletList),
        2 => (1u32..5, prop::collection::vec(list_item(), 1..4))
            .prop_map(|(start, items)| Block::OrderedList { start, items }),
        1 => prop::collection::vec(leaf_block(), 1..3).prop_map(Block::BlockQuote),
    ]
}

fn document() -> impl Strategy<Value = Document> {
    prop::collection::vec(block(), 1..6).prop_map(Document::new)
}

/// Markdown-only constructs: GFM task lists and tables have no wiki equivalent
/// and no plain-ADF-free representation, so they are exercised separately.
fn task_item() -> impl Strategy<Value = TaskItem> {
    (any::<bool>(), inlines()).prop_map(|(checked, content)| TaskItem {
        checked,
        content,
        id: None,
    })
}

fn cell() -> impl Strategy<Value = Vec<Block>> {
    inlines().prop_map(|c| vec![Block::Paragraph(c)])
}

fn table() -> impl Strategy<Value = Table> {
    (1usize..4, 1usize..4)
        .prop_flat_map(|(cols, rows)| {
            (
                prop::collection::vec(cell(), cols..=cols),
                prop::collection::vec(prop::collection::vec(cell(), cols..=cols), rows..=rows),
            )
        })
        .prop_map(|(header, rows)| Table {
            header: Some(header),
            rows,
        })
}

/// A conversion is not required to be the identity on an arbitrary AST — each
/// format normalizes (ADF flattens mark sets, wiki mentions carry no display
/// name, Markdown recognises bare issue keys). What it *is* required to be is
/// **idempotent**: once a document has been through a format, going through it
/// again must change nothing.
///
/// That is the property the tool actually depends on. A non-idempotent
/// conversion would make every pull rewrite files and every diff show phantom
/// changes, no matter how faithful the first pass was.
fn assert_idempotent<T: PartialEq + std::fmt::Debug>(
    once: T,
    twice: T,
    rendered: &str,
) -> Result<(), TestCaseError> {
    prop_assert_eq!(&once, &twice, "\n--- rendered ---\n{}", rendered);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// The working copy is Markdown; churn here means noisy diffs on every pull.
    #[test]
    fn markdown_is_idempotent(doc in document()) {
        let store = RawStore::new();
        let once = markdown::parse(&markdown::render(&doc, &store), &store);
        let text = markdown::render(&once, &store);
        assert_idempotent(once.clone(), markdown::parse(&text, &store), &text)?;
        // The serialized form must be stable too, not just the AST.
        prop_assert_eq!(markdown::render(&once, &store), text);
    }

    #[test]
    fn markdown_is_idempotent_for_task_lists_and_tables(
        items in prop::collection::vec(task_item(), 1..4),
        t in table(),
    ) {
        let doc = Document::new(vec![Block::TaskList(items), Block::Table(t)]);
        let store = RawStore::new();
        let once = markdown::parse(&markdown::render(&doc, &store), &store);
        let text = markdown::render(&once, &store);
        assert_idempotent(once, markdown::parse(&text, &store), &text)?;
    }

    /// Cloud stores ADF; churn here means a push reshapes the issue.
    #[test]
    fn adf_is_idempotent(doc in document()) {
        let mut store = RawStore::new();
        let once = adf::to_ast(&adf::from_ast(&doc, &store), &mut store);
        let value = adf::from_ast(&once, &store);
        assert_idempotent(once, adf::to_ast(&value, &mut store), &value.to_string())?;
        // Byte-level stability is what makes an unchanged issue push as a no-op.
        prop_assert_eq!(
            serde_json::to_string(&adf::from_ast(&adf::to_ast(&value, &mut store), &store)).unwrap(),
            serde_json::to_string(&value).unwrap()
        );
    }

    /// Data Center stores wiki markup; same stakes as ADF.
    #[test]
    fn wiki_is_idempotent(doc in document()) {
        let mut store = RawStore::new();
        let once = wiki::to_ast(&wiki::from_ast(&doc, &store), &mut store);
        let text = wiki::from_ast(&once, &store);
        assert_idempotent(once, wiki::to_ast(&text, &mut store), &text)?;
        prop_assert_eq!(wiki::from_ast(&wiki::to_ast(&text, &mut store), &store), text);
    }

    /// The whole read/write path: ADF on the server, Markdown in the working
    /// copy, ADF back on push. An untouched issue must push back unchanged.
    #[test]
    fn an_untouched_issue_pushes_back_unchanged_on_cloud(doc in document()) {
        let mut store = RawStore::new();
        // What the server would hold after one jired-mediated write.
        let stored = adf::from_ast(&adf::to_ast(&adf::from_ast(&doc, &store), &mut store), &store);

        let md = markdown::render(&adf::to_ast(&stored, &mut store), &store);
        let pushed = adf::from_ast(&markdown::parse(&md, &store), &store);

        prop_assert_eq!(
            serde_json::to_string(&pushed).unwrap(),
            serde_json::to_string(&stored).unwrap(),
            "\n--- markdown ---\n{}", md
        );
    }

    /// Same, for Data Center.
    #[test]
    fn an_untouched_issue_pushes_back_unchanged_on_data_center(doc in document()) {
        let mut store = RawStore::new();
        let stored = wiki::from_ast(&wiki::to_ast(&wiki::from_ast(&doc, &store), &mut store), &store);

        let md = markdown::render(&wiki::to_ast(&stored, &mut store), &store);
        let pushed = wiki::from_ast(&markdown::parse(&md, &store), &store);

        prop_assert_eq!(&pushed, &stored, "\n--- markdown ---\n{}", md);
    }
}

/// A lossy region must come back byte-identical no matter what surrounds it.
///
/// This is the invariant that lets jired promise "leave the block alone and
/// your panel/macro/media survives": push rebuilds it from the stored original,
/// never from the Markdown approximation.
#[test]
fn a_preserved_region_is_reassembled_verbatim() {
    let panel = serde_json::json!({
        "type": "panel",
        "attrs": { "panelType": "warning" },
        "content": [{
            "type": "paragraph",
            "content": [{ "type": "text", "text": "Do not deploy." }]
        }]
    });
    let original = serde_json::json!({
        "type": "doc",
        "version": 1,
        "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "before" }] },
            panel,
            { "type": "paragraph", "content": [{ "type": "text", "text": "after" }] }
        ]
    });

    let mut store = RawStore::new();
    let ast = adf::to_ast(&original, &mut store);
    assert_eq!(store.blocks().len(), 1, "the panel must be preserved");

    // Ten cycles: an id that drifted or an approximation that leaked into the
    // AST would show up as divergence rather than staying stable.
    let mut current = ast;
    let mut text = String::new();
    for _ in 0..10 {
        text = markdown::render(&current, &store);
        current = markdown::parse(&text, &store);
    }

    assert!(
        text.contains("Do not deploy."),
        "approximation is shown to the user"
    );
    assert_eq!(
        serde_json::to_string(&adf::from_ast(&current, &store)).unwrap(),
        serde_json::to_string(&original).unwrap(),
    );
}

#[test]
fn an_edited_region_is_detected_as_edited() {
    let original = serde_json::json!({
        "type": "doc",
        "version": 1,
        "content": [{
            "type": "panel",
            "attrs": { "panelType": "info" },
            "content": [{
                "type": "paragraph",
                "content": [{ "type": "text", "text": "original text" }]
            }]
        }]
    });

    let mut store = RawStore::new();
    let ast = adf::to_ast(&original, &mut store);
    let text = markdown::render(&ast, &store);
    let raw = store.blocks().to_vec();

    assert!(jired::convert::edited_regions(&text, &raw).is_empty());

    let tampered = text.replace("original text", "tampered text");
    assert_eq!(
        jired::convert::edited_regions(&tampered, &raw),
        vec![raw[0].id.clone()],
    );

    // Deleting the block entirely also counts: pushing would drop content.
    assert_eq!(
        jired::convert::edited_regions("nothing here", &raw),
        vec![raw[0].id.clone()],
    );
}
