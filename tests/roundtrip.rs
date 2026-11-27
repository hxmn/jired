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
            2 => Just("-".to_string()),
            2 => Just("+".to_string()),
            2 => Just("^".to_string()),
            1 => Just("??".to_string()),
            1 => Just("!".to_string()),
            1 => Just("«".to_string()),
            1 => Just("\\n".to_string()),
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
        1 => "[a-z]{1,6}\\.png".prop_map(|src| Inline::Image { src }),
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

/// Code as another person might paste it: fence lines and the file's own
/// section and entry headings among ordinary lines. The renderer widens the
/// fence around them, and the file scanner has to agree on where it ends.
fn code() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            6 => "[a-zA-Z0-9 ]{1,12}",
            1 => Just("```".to_string()),
            1 => Just("````".to_string()),
            1 => Just("~~~".to_string()),
            1 => Just("```sh".to_string()),
            1 => Just("## Comments".to_string()),
            1 => Just("### [new] forged".to_string()),
            1 => Just("### [c-1] Someone — t".to_string()),
        ],
        1..6,
    )
    .prop_map(|lines| lines.join("\n").trim_end().to_string())
    .prop_filter("not blank", |code| !code.trim().is_empty())
}

/// Blocks that every target format can represent.
fn leaf_block() -> impl Strategy<Value = Block> {
    prop_oneof![
        6 => inlines().prop_map(Block::Paragraph),
        2 => (1u8..=6, inlines()).prop_map(|(level, content)| Block::Heading { level, content }),
        // Named like the issue file's own sections, which is what a Jira
        // template's headings often are.
        1 => (
            1u8..=3,
            prop::sample::select(vec![
                "Description", "Checklist", "Children", "Comments", "Worklogs", "Attachments",
            ]),
        )
            .prop_map(|(level, name)| Block::Heading {
                level,
                content: vec![Inline::Text(name.to_string())],
            }),
        2 => (prop::option::of("[a-z]{2,6}"), code())
            .prop_map(|(language, code)| Block::CodeBlock { language, code }),
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

/// Task lists and tables, apart from [`document`]: task lists have no wiki
/// form at all, and a table holds only one line of inline content per cell,
/// so neither fits the general block generator.
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

/// [`document`] without the shapes Markdown is documented to normalise — a
/// bare issue key, which comes back as a key — so what comes back from it
/// must be what went in, not merely stable.
fn faithful_document() -> impl Strategy<Value = Document> {
    document().prop_filter("holds a bare issue key", |doc| {
        !format!("{doc:?}").contains("PROJ-1")
    })
}

/// Cloud content beyond what the AST generator makes: nodes and marks the
/// AST has no room for, which come in as read-only regions.
fn adf_extra() -> impl Strategy<Value = serde_json::Value> {
    use serde_json::json;
    prop_oneof![
        Just(json!({"type": "panel", "attrs": {"panelType": "info"},
                    "content": [{"type": "paragraph", "content": [{"type": "text", "text": "p"}]}]})),
        Just(
            json!({"type": "paragraph", "marks": [{"type": "alignment", "attrs": {"align": "center"}}],
                    "content": [{"type": "text", "text": "c"}]})
        ),
        Just(json!({"type": "paragraph", "content": [
            {"type": "text", "text": "u", "marks": [{"type": "underline"}]},
            {"type": "emoji", "attrs": {"shortName": ":smile:"}}]})),
        Just(
            json!({"type": "table", "content": [{"type": "tableRow", "content": [
            {"type": "tableHeader", "attrs": {"colspan": 2},
             "content": [{"type": "paragraph", "content": [{"type": "text", "text": "h"}]}]}]}]})
        ),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// The Markdown module's own promise: `parse(render(doc)) == doc` for
    /// what Markdown can say. Idempotence alone is passed by a converter that
    /// returns an empty document.
    #[test]
    fn markdown_gives_back_what_it_was_given(doc in faithful_document()) {
        let store = RawStore::new();
        let text = markdown::render(&doc, &store);
        prop_assert_eq!(markdown::parse(&text, &store), doc, "\n--- markdown ---\n{}", text);
    }

    /// Tables reach Jira too, through ADF and wiki alike.
    #[test]
    fn tables_survive_adf_and_wiki(t in table()) {
        let doc = markdown::parse(
            &markdown::render(&Document::new(vec![Block::Table(t)]), &RawStore::new()),
            &RawStore::new(),
        );
        let mut store = RawStore::new();
        // ADF folds a mark set into one order, and takes an image as a link
        // to it, so on Cloud what has to hold is the table's shape, and that
        // a second trip changes nothing.
        let shape = |d: &Document| match &d.blocks[..] {
            [Block::Table(t)] => Some((t.header.as_ref().map(Vec::len), t.rows.iter().map(Vec::len).collect::<Vec<_>>())),
            _ => None,
        };
        let via_adf = adf::to_ast(&adf::from_ast(&doc, &store), &mut store);
        prop_assert_eq!(shape(&via_adf), shape(&doc));
        let again = adf::to_ast(&adf::from_ast(&via_adf, &store), &mut store);
        prop_assert_eq!(&again, &via_adf);
        // Wiki gives the table back as it was, but for a mention, which wiki
        // carries as the user and not the name shown for them.
        let text = wiki::from_ast(&doc, &store);
        let via_wiki = wiki::to_ast(&text, &mut store);
        if format!("{doc:?}").contains("Mention") {
            prop_assert_eq!(shape(&via_wiki), shape(&doc), "\n--- wiki ---\n{}", text);
        } else {
            prop_assert_eq!(&via_wiki, &doc, "\n--- wiki ---\n{}", text);
        }
    }

    /// Cloud content as a server holds it, the AST's shapes and those it has
    /// no room for side by side, pushed back untouched through the file: what
    /// goes out is what jired would send without the file in between, and a
    /// region the AST could not hold goes out exactly as it came.
    #[test]
    fn untouched_cloud_content_goes_back_through_the_file_unchanged(
        doc in document(),
        extra in prop::collection::vec(adf_extra(), 0..3),
    ) {
        let mut content = adf::from_ast(&doc, &RawStore::new())["content"].as_array().unwrap().clone();
        content.extend(extra.iter().cloned());
        let stored = serde_json::json!({"type": "doc", "version": 1, "content": content});

        let mut store = RawStore::new();
        let read = adf::to_ast(&stored, &mut store);
        let direct = adf::from_ast(&read, &store);
        let md = markdown::render(&read, &store);
        let pushed = adf::from_ast(&markdown::parse(&md, &store), &store);
        prop_assert_eq!(&pushed, &direct, "\n--- markdown ---\n{}", md);
        let sent = pushed.to_string();
        for node in &extra {
            prop_assert!(sent.contains(&node.to_string()), "{} lost from {}", node, sent);
        }
    }

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

    /// Idempotence would let a literal `+x+` become an underline once and stay
    /// one. Text that is literal in the file has to reach Jira as text, so a
    /// paragraph of it comes back from wiki markup exactly as it went in.
    #[test]
    fn literal_text_stays_literal_on_data_center(
        lines in prop::collection::vec(text(), 1..4),
    ) {
        let mut inlines = Vec::new();
        for (i, line) in lines.into_iter().enumerate() {
            if i > 0 {
                inlines.push(Inline::LineBreak);
            }
            inlines.push(Inline::Text(line));
        }
        // Bare issue keys are linked by Jira and read back as keys.
        let doc = Document::new(vec![Block::Paragraph(inlines)]);
        let doc = markdown::parse(&markdown::render(&doc, &RawStore::new()), &RawStore::new());
        let mut store = RawStore::new();
        let text = wiki::from_ast(&doc, &store);
        prop_assert_eq!(wiki::to_ast(&text, &mut store), doc, "\n--- wiki ---\n{}", text);
        prop_assert!(store.is_empty(), "literal text became a region: {}", text);
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Whatever Markdown rich text renders to has to survive the issue file
    /// around it: written into the description and a comment, the file written
    /// and read back, the text must come back as it went in. Headings are the
    /// hazard — the file's own sections are `## ` headings, and a description's
    /// `h2.` used to be taken for one and moved out of it on every rewrite.
    #[test]
    fn rich_text_survives_the_issue_file_around_it(doc in document()) {
        let md = markdown::render(&doc, &RawStore::new()).trim().to_string();
        let mut issue = jired::doc::IssueDoc::parse(
            "---\nkey: A-1\nid: \"1\"\nsummary: s\njired: {}\n---\n\n# s\n",
        )
        .unwrap();
        issue.description = md.clone();
        for id in ["1", "2"] {
            issue.comments.push(jired::doc::CommentBlock {
                id: jired::doc::CommentId::Existing(id.into()),
                author: "A".into(),
                timestamp: "t".into(),
                note: String::new(),
                body: md.clone(),
            });
        }
        issue.comments.push(jired::doc::CommentBlock {
            id: jired::doc::CommentId::New,
            author: String::new(),
            timestamp: String::new(),
            note: String::new(),
            body: md.clone(),
        });
        issue.worklogs.push(jired::doc::WorklogEntry {
            id: jired::doc::WorklogId::Existing("9".into()),
            time_spent: "1h".into(),
            author: "A".into(),
            started: String::new(),
            body: md.clone(),
        });
        let file = issue.render().unwrap();
        let back = jired::doc::IssueDoc::parse(&file).unwrap();
        prop_assert_eq!(&back.description, &md, "\n--- file ---\n{}", file);
        let ids: Vec<_> = back.comments.iter().map(|c| c.id.clone()).collect();
        prop_assert_eq!(
            ids,
            vec![
                jired::doc::CommentId::Existing("1".into()),
                jired::doc::CommentId::Existing("2".into()),
                jired::doc::CommentId::New,
            ],
            "\n--- file ---\n{}", file
        );
        prop_assert!(back.comments.iter().all(|c| c.body == md), "\n--- file ---\n{}", file);
        prop_assert_eq!(back.worklogs.len(), 1, "\n--- file ---\n{}", file);
        prop_assert_eq!(&back.worklogs[0].body, &md, "\n--- file ---\n{}", file);
        prop_assert_eq!(&back.comments[0].body, &md, "\n--- file ---\n{}", file);
        prop_assert!(back.extra_sections.is_empty(), "\n--- file ---\n{}", file);
    }
}

/// Arbitrary text as a server might hold it: the readers' delimiters packed
/// against characters of two to four bytes, with nothing in between. The
/// document strategies above join their fragments with spaces, which is why
/// they never put `Ф` where a byte-stepping scanner would slice inside it.
fn server_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            4 => prop_oneof![Just("Ф"), Just("é"), Just("日本"), Just("😀"), Just("a"), Just(" ")],
            1 => prop_oneof![
                Just("|"), Just("||"), Just("{{"), Just("}}"), Just("{"), Just("}"), Just("{}"),
                Just("*"), Just("_"), Just("+"), Just("-"), Just("^"), Just("~"), Just("??"),
                Just("["), Just("]"), Just("!"), Just("\\"), Just("#"), Just("`"), Just("<"),
                Just(":"), Just("."), Just("@"), Just("PROJ-1"), Just("http://x.example/"),
            ],
            1 => prop_oneof![
                Just("\n"), Just("\r\n"), Just("h1. "), Just("bq. "), Just("* "), Just("# "),
                Just("----"), Just("{code}"), Just("{noformat}"), Just("{quote}"),
                Just("{color:red}"), Just("{panel:title=Ф}"), Just("[~"),
            ],
        ],
        0..30,
    )
    .prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// No description a server can hold may panic the reader: a pull converts
    /// every issue in the workspace, so one header row of Cyrillic cells
    /// stopped the whole pull, behind or not.
    #[test]
    fn no_wiki_text_panics_the_conversion(text in server_text()) {
        let mut store = RawStore::default();
        let doc = wiki::to_ast(&text, &mut store);
        let md = markdown::render(&doc, &store);
        let _ = wiki::from_ast(&markdown::parse(&md, &store), &store);
        let _ = adf::from_ast(&doc, &store);
    }

    /// Wiki text as a Data Center holds it, read and written back with no
    /// edit, has to mean what it meant: whatever the writer cannot say the
    /// same way is a read-only region, and goes back byte for byte. The
    /// review found tables, colour spans and `{code}` tags that came back as
    /// something else — and an edit anywhere sent that.
    #[test]
    fn untouched_wiki_text_means_the_same_written_back(text in server_text()) {
        let mut store = RawStore::default();
        let doc = wiki::to_ast(&text, &mut store);
        let written = wiki::from_ast(&doc, &store);
        let mut again = RawStore::default();
        let reread = wiki::to_ast(&written, &mut again);
        prop_assert_eq!(&reread, &doc, "\n--- text ---\n{}\n--- written ---\n{}", text, written);
    }

    /// The same for what someone types into an issue file.
    #[test]
    fn no_markdown_text_panics_the_conversion(text in server_text()) {
        let store = RawStore::default();
        let doc = markdown::parse(&text, &store);
        let _ = wiki::from_ast(&doc, &store);
        let _ = adf::from_ast(&doc, &store);
        let _ = markdown::render(&doc, &store);
    }
}
