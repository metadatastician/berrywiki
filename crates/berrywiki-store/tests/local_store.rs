//! Integration tests: LocalFolderStore over a scratch copy of the fixture
//! wiki. Every test gets its own temp directory; the fixture itself is never
//! modified (spec: fixtures and import sources are read-only).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use berrywiki_core::PageKind;
use berrywiki_store::{CreatePageInput, LocalFolderStore, MovePageInput, StoreError, WikiStore};

const HOME_ID: &str = "0195f6d0-0000-7000-8000-000000000001";
const TEACHING_ID: &str = "0195f6d0-0000-7000-8000-000000000002";
const COURSE_A_ID: &str = "0195f6d0-0000-7000-8000-000000000003";
const PLAN_ID: &str = "0195f6ec-36a2-7a42-b519-5f558842e256";
const RESEARCH_ID: &str = "0195f6d0-0000-7000-8000-000000000004";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Copy the fixture wiki into a fresh scratch directory.
fn scratch_wiki() -> PathBuf {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-wiki")
        .canonicalize()
        .expect("fixture exists");
    let dir = std::env::temp_dir().join(format!(
        "berrywiki-store-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    for entry in fs::read_dir(&fixture).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
        }
    }
    dir
}

fn create_input(id: &str, title: &str, parent: Option<&str>) -> CreatePageInput {
    CreatePageInput {
        id: id.to_string(),
        title: title.to_string(),
        parent_id: parent.map(|s| s.to_string()),
        position: 50,
        kind: PageKind::Page,
        tags: vec!["new".to_string()],
        body: String::new(),
    }
}

#[test]
fn opens_fixture_and_lists_pages() {
    let dir = scratch_wiki();
    let store = LocalFolderStore::open(&dir).unwrap();
    let pages = store.list_pages();
    assert_eq!(pages.len(), 10);
    assert!(pages.iter().any(|p| p.id == HOME_ID && p.title == "Home"));
}

#[test]
fn reads_page_by_id() {
    let dir = scratch_wiki();
    let store = LocalFolderStore::open(&dir).unwrap();
    let page = store.read_page(PLAN_ID).unwrap();
    assert_eq!(page.title, "Assessment Plan");
    assert!(page.body.contains("## Weighting"));
}

#[test]
fn open_missing_root_fails_cleanly() {
    match LocalFolderStore::open("/definitely/not/a/real/dir") {
        Err(StoreError::RootNotFound(_)) => {}
        Err(other) => panic!("expected RootNotFound, got {other}"),
        Ok(_) => panic!("open must fail on a missing root"),
    }
}

#[test]
fn creates_child_page_with_hierarchical_filename_and_sidebar() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    store
        .create_page(create_input("new-child-1", "Reading List", Some(COURSE_A_ID)))
        .unwrap();

    let expected = dir.join("Teaching--Course-A--Reading-List.md");
    assert!(expected.exists(), "hierarchical filename created");

    let page = store.read_page("new-child-1").unwrap();
    assert_eq!(page.parent_id(), Some(COURSE_A_ID));

    let sidebar = fs::read_to_string(dir.join("_Sidebar.md")).unwrap();
    assert!(sidebar.contains("[Reading List](Teaching--Course-A--Reading-List)"));
}

#[test]
fn create_rejects_duplicate_id_and_missing_parent() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    let dup = store.create_page(create_input(HOME_ID, "Impostor", None));
    assert!(matches!(dup.unwrap_err(), StoreError::DuplicateId(_)));
    let orphan = store.create_page(create_input("x1", "Orphan", Some("ghost-parent")));
    assert!(matches!(orphan.unwrap_err(), StoreError::ParentNotFound(_)));
}

#[test]
fn filename_collision_falls_back_to_id_suffix() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    // "Research" as a root collides with the existing Research.md file.
    store
        .create_page(create_input("res-2222", "Research", None))
        .unwrap();
    assert!(dir.join("Research--2222.md").exists());
}

#[test]
fn update_preserves_metadata_bytes() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    let before_meta = store.read_page(PLAN_ID).unwrap().metadata.clone().unwrap();

    store
        .update_page(PLAN_ID, "# Assessment Plan\n\nRewritten body.\n")
        .unwrap();

    let after = store.read_page(PLAN_ID).unwrap();
    assert!(after.body.contains("Rewritten body."));
    assert_eq!(after.metadata.as_ref().unwrap(), &before_meta);
}

#[test]
fn update_missing_page_fails() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    assert!(matches!(
        store.update_page("no-such-id", "x").unwrap_err(),
        StoreError::PageNotFound(_)
    ));
}

#[test]
fn move_reparents_renames_and_regenerates_sidebar() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    // Move Assessment Plan from Course A to Research.
    store
        .move_page(MovePageInput {
            id: PLAN_ID.to_string(),
            new_parent_id: Some(RESEARCH_ID.to_string()),
            new_position: 5,
        })
        .unwrap();

    assert!(dir.join("Research--Assessment-Plan.md").exists());
    assert!(!dir.join("Teaching--Course-A--Assessment-Plan.md").exists());

    let page = store.read_page(PLAN_ID).unwrap();
    assert_eq!(page.parent_id(), Some(RESEARCH_ID));
    assert_eq!(page.position(), 5);
    assert!(page.body.contains("## Weighting"), "body survived the move");

    let sidebar = fs::read_to_string(dir.join("_Sidebar.md")).unwrap();
    assert!(sidebar.contains("[Assessment Plan](Research--Assessment-Plan)"));
}

#[test]
fn move_refuses_cycles() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    // Teaching under its own grandchild: Assessment Plan is under Course A
    // which is under Teaching.
    let err = store
        .move_page(MovePageInput {
            id: TEACHING_ID.to_string(),
            new_parent_id: Some(PLAN_ID.to_string()),
            new_position: 0,
        })
        .unwrap_err();
    assert!(matches!(err, StoreError::CycleDetected { .. }));
    // Nothing changed on disk.
    assert!(dir.join("Teaching.md").exists());
}

#[test]
fn move_nonleaf_cascades_descendant_filenames() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    // Teaching has Course A -> Assessment Plan beneath it. Move it under Research.
    store
        .move_page(MovePageInput {
            id: TEACHING_ID.to_string(),
            new_parent_id: Some(RESEARCH_ID.to_string()),
            new_position: 5,
        })
        .unwrap();

    // The whole subtree's filenames are recomputed…
    assert!(dir.join("Research--Teaching.md").exists());
    assert!(dir.join("Research--Teaching--Course-A.md").exists());
    assert!(dir.join("Research--Teaching--Course-A--Assessment-Plan.md").exists());
    // …and the old ones are gone (no orphans).
    assert!(!dir.join("Teaching.md").exists());
    assert!(!dir.join("Teaching--Course-A.md").exists());
    assert!(!dir.join("Teaching--Course-A--Assessment-Plan.md").exists());

    // Only the moved page is re-parented; descendants keep their (id-based)
    // parents — just their filenames changed.
    assert_eq!(store.read_page(TEACHING_ID).unwrap().parent_id(), Some(RESEARCH_ID));
    assert_eq!(store.read_page(COURSE_A_ID).unwrap().parent_id(), Some(TEACHING_ID));
    assert_eq!(store.read_page(PLAN_ID).unwrap().parent_id(), Some(COURSE_A_ID));
    assert_eq!(
        store.read_page(PLAN_ID).unwrap().path,
        "Research--Teaching--Course-A--Assessment-Plan.md"
    );
    // Content survived the cascade.
    assert!(store.read_page(PLAN_ID).unwrap().body.contains("## Weighting"));
}

#[test]
fn move_rewrites_inbound_links_so_they_still_resolve() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    // Baseline: Home links to the Assessment Plan by its (old) stem + heading,
    // so the plan has a backlink from Home.
    assert!(store.graph().backlinks_of(PLAN_ID).iter().any(|b| b.from_id == HOME_ID));

    store
        .move_page(MovePageInput {
            id: TEACHING_ID.to_string(),
            new_parent_id: Some(RESEARCH_ID.to_string()),
            new_position: 5,
        })
        .unwrap();

    // Without link rewriting this backlink would break; with it, the inbound
    // link was updated to the new stem and still resolves.
    assert!(
        store.graph().backlinks_of(PLAN_ID).iter().any(|b| b.from_id == HOME_ID),
        "Home's link to the moved page was rewritten and still resolves"
    );
    let home_src = &store.read_page(HOME_ID).unwrap().source;
    assert!(home_src.contains("Research--Teaching--Course-A--Assessment-Plan"));
    // No unexpected broken links introduced (the fixture's one deliberate
    // broken link to "Nonexistent Page" remains the only one).
    let broken = store
        .graph()
        .diagnostics()
        .iter()
        .filter(|d| d.code == "link.broken")
        .count();
    assert_eq!(broken, 1, "only the pre-existing broken link remains");
}

#[test]
fn move_with_duplicate_title_siblings_does_not_rotate_or_corrupt_links() {
    // Review finding #1/#2/#5: two siblings with the same title (one clean, one
    // id-suffixed) must keep their own filenames on a move — never swap — so
    // links stay pointing at the correct page and no page's only copy is lost.
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();

    // P and M are both NON-roots (under Home): a root contributes no filename
    // segment, so only a non-root ancestor prefixes its descendants. This makes
    // the move actually rename the subtree ("M--…" -> "P--M--…").
    store.create_page(create_input("pp", "P", Some(HOME_ID))).unwrap();
    store.create_page(create_input("mm", "M", Some(HOME_ID))).unwrap();
    store.create_page(create_input("child-aaa1", "Notes", Some("mm"))).unwrap(); // -> M--Notes.md
    store.create_page(create_input("child-bbb2", "Notes", Some("mm"))).unwrap(); // -> M--Notes--bbb2.md
    assert!(dir.join("M--Notes.md").exists());
    assert!(dir.join("M--Notes--bbb2.md").exists());

    // A page links to EACH sibling distinctly.
    let mut linker = create_input("linker", "Linker", None);
    linker.body = "See [[M--Notes]] and [[M--Notes--bbb2]].\n".to_string();
    store.create_page(linker).unwrap();
    assert!(store.graph().backlinks_of("child-aaa1").iter().any(|b| b.from_id == "linker"));
    assert!(store.graph().backlinks_of("child-bbb2").iter().any(|b| b.from_id == "linker"));

    // Move M under P — both children are renamed, but must not swap identities.
    store
        .move_page(MovePageInput {
            id: "mm".to_string(),
            new_parent_id: Some("pp".to_string()),
            new_position: 0,
        })
        .unwrap();

    // Both children survive with prefixed-but-still-distinct names.
    assert_eq!(store.read_page("child-aaa1").unwrap().path, "P--M--Notes.md");
    assert_eq!(store.read_page("child-bbb2").unwrap().path, "P--M--Notes--bbb2.md");
    assert!(dir.join("P--M--Notes.md").exists());
    assert!(dir.join("P--M--Notes--bbb2.md").exists());

    // Crucially: each link still points at ITS OWN page (no double-rewrite
    // collapsing both onto one). Both backlinks survive and are distinct.
    assert!(
        store.graph().backlinks_of("child-aaa1").iter().any(|b| b.from_id == "linker"),
        "link to the clean sibling preserved"
    );
    assert!(
        store.graph().backlinks_of("child-bbb2").iter().any(|b| b.from_id == "linker"),
        "link to the suffixed sibling not corrupted onto the other page"
    );
}

#[test]
fn move_rewrites_whitespace_padded_links() {
    // Review finding #3: links the parser accepts with surrounding whitespace
    // must be rewritten too, or they dangle after a move.
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    let mut wsp = create_input("wsp", "Whitespace Links", None);
    wsp.body = "See [[ Teaching--Course-A ]] and [d]( Teaching--Course-A ).\n".to_string();
    store.create_page(wsp).unwrap();
    // Baseline: both padded links resolve to Course A.
    assert!(store.graph().backlinks_of(COURSE_A_ID).iter().any(|b| b.from_id == "wsp"));

    store
        .move_page(MovePageInput {
            id: COURSE_A_ID.to_string(),
            new_parent_id: Some(RESEARCH_ID.to_string()),
            new_position: 0,
        })
        .unwrap();

    // After the move the padded links were rewritten and still resolve.
    assert!(
        store.graph().backlinks_of(COURSE_A_ID).iter().any(|b| b.from_id == "wsp"),
        "whitespace-padded links were rewritten and still resolve"
    );
    assert!(store.read_page("wsp").unwrap().source.contains("Research--Course-A"));
}

#[test]
fn move_refuses_unmanaged_pages() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    let err = store
        .move_page(MovePageInput {
            id: "Plain-Legacy-Page.md".to_string(), // unmanaged: id == path
            new_parent_id: Some(HOME_ID.to_string()),
            new_position: 0,
        })
        .unwrap_err();
    assert!(matches!(err, StoreError::UnmanagedPage(_)));
}

#[test]
fn delete_refuses_pages_with_children_then_deletes_leaf() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();

    let err = store.delete_page(TEACHING_ID).unwrap_err();
    assert!(matches!(err, StoreError::HasChildren { .. }));
    assert!(dir.join("Teaching.md").exists(), "refusal changed nothing");

    store.delete_page(PLAN_ID).unwrap();
    assert!(!dir.join("Teaching--Course-A--Assessment-Plan.md").exists());
    let sidebar = fs::read_to_string(dir.join("_Sidebar.md")).unwrap();
    assert!(!sidebar.contains("Assessment Plan"));
}

#[test]
fn attachments_are_id_keyed_and_traversal_proof() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();

    let att = store
        .add_attachment(PLAN_ID, "rubric.pdf", b"%PDF-fake")
        .unwrap();
    assert_eq!(att.path, format!("assets/{PLAN_ID}/rubric.pdf"));
    assert!(dir.join(&att.path).exists());

    let dup = store.add_attachment(PLAN_ID, "rubric.pdf", b"other");
    assert!(matches!(dup.unwrap_err(), StoreError::DuplicateAttachment { .. }));

    for evil in ["../evil.pdf", "..", "a/b.pdf", "a\\b.pdf", ".hidden"] {
        let err = store.add_attachment(PLAN_ID, evil, b"x").unwrap_err();
        assert!(
            matches!(err, StoreError::InvalidName { .. }),
            "{evil:?} must be rejected"
        );
    }
    assert!(!dir.parent().unwrap().join("evil.pdf").exists());
}

#[test]
fn sidebar_regeneration_is_write_if_changed() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    // First regeneration replaces the hand-written fixture sidebar.
    let first = store.regenerate_sidebar().unwrap();
    assert!(first, "hand-written sidebar differs from generated form");
    // Second regeneration: no semantic change → no write.
    let second = store.regenerate_sidebar().unwrap();
    assert!(!second, "unchanged sidebar must not be rewritten");
}

#[test]
fn page_titled_sidebar_cannot_clobber_generated_sidebar() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    store
        .create_page(CreatePageInput {
            id: "sb-trap".to_string(),
            title: "_Sidebar".to_string(),
            parent_id: None,
            position: 0,
            kind: PageKind::Page,
            tags: vec![],
            body: "# _Sidebar\n\nprecious content\n".to_string(),
        })
        .unwrap();

    // The page landed on a NON-reserved filename and is fully readable…
    assert!(dir.join("Sidebar.md").exists());
    let page = store.read_page("sb-trap").unwrap();
    assert!(page.body.contains("precious content"));

    // …and the real _Sidebar.md is the generated sidebar, not page content.
    let sidebar = fs::read_to_string(dir.join("_Sidebar.md")).unwrap();
    assert!(sidebar.starts_with("# Notebook"));
    assert!(!sidebar.contains("precious content"));
}

#[test]
fn create_rejects_hostile_or_empty_ids() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    for bad in ["", "a/ok", "../x", "a b", ".hidden"] {
        let err = store
            .create_page(create_input(bad, "Innocent Title", None))
            .unwrap_err();
        assert!(
            matches!(err, StoreError::InvalidName { .. }),
            "id {bad:?} must be rejected, got: {err}"
        );
    }
}

#[cfg(unix)]
#[test]
fn planted_symlink_at_temp_path_cannot_redirect_writes() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();

    // Hostile repo content: a symlink sitting exactly where safe_write puts
    // its temp file, pointing at a victim outside the wiki root.
    let victim = dir.parent().unwrap().join(format!(
        "berrywiki-victim-{}.txt",
        std::process::id()
    ));
    fs::write(&victim, "untouched").unwrap();
    let tmp_path = dir.join(format!("Teaching.md.tmp-{}", std::process::id()));
    std::os::unix::fs::symlink(&victim, &tmp_path).unwrap();

    store
        .update_page(TEACHING_ID, "# Teaching\n\nnew body\n")
        .unwrap();

    // The victim was never written through; the page updated correctly.
    assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
    let content = fs::read_to_string(dir.join("Teaching.md")).unwrap();
    assert!(content.contains("new body"));
    let _ = fs::remove_file(&victim);
}

#[test]
fn reload_ignores_symlinked_pages() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    #[cfg(unix)]
    {
        // A dangling symlink and a link to an outside file: neither may enter
        // the graph as content.
        std::os::unix::fs::symlink("/nonexistent/target.md", dir.join("Dangling.md")).unwrap();
        let outside = dir.parent().unwrap().join(format!(
            "berrywiki-outside-{}.md",
            std::process::id()
        ));
        fs::write(&outside, "# Smuggled\n").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("Smuggled.md")).unwrap();
        store.reload().unwrap();
        assert!(!store.list_pages().iter().any(|p| p.title == "Smuggled"));
        assert!(!store.list_pages().iter().any(|p| p.path == "Dangling.md"));
        let _ = fs::remove_file(&outside);
    }
    #[cfg(not(unix))]
    {
        store.reload().unwrap();
    }
}

#[test]
fn reposition_of_suffix_named_page_succeeds() {
    // Review finding #2/#7: a page whose file carries the id suffix (because
    // its plain name collided) must still be repositionable.
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    store
        .create_page(create_input("res-2222", "Research", None))
        .unwrap();
    assert!(dir.join("Research--2222.md").exists());

    // Pure reposition (same parent) must NOT fail with a self-collision.
    store
        .move_page(MovePageInput {
            id: "res-2222".to_string(),
            new_parent_id: None,
            new_position: 99,
        })
        .unwrap();
    assert_eq!(store.read_page("res-2222").unwrap().position(), 99);
    assert!(dir.join("Research--2222.md").exists());
}

#[test]
fn case_insensitive_filename_collision_is_suffixed() {
    // Review finding #8: "research" must not sit beside "Research.md".
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    store
        .create_page(create_input("res-lc", "research", None))
        .unwrap();
    // No case-colliding twin of the existing Research.md was created; the new
    // page landed on a distinct id-suffixed name.
    assert!(!dir.join("research.md").exists());
    let created = &store.read_page("res-lc").unwrap().path;
    assert!(created.to_lowercase().starts_with("research--"), "got {created}");
    assert!(dir.join(created).exists());
}

#[test]
fn create_rejects_unmanaged_parent() {
    // Review finding #16: an unmanaged page (id == filename) cannot be a parent.
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    let err = store
        .create_page(create_input("child-x", "Child", Some("Plain-Legacy-Page.md")))
        .unwrap_err();
    assert!(matches!(err, StoreError::UnmanagedParent(_)), "got {err}");
}

#[test]
fn create_rejects_hostile_tags() {
    // Review finding #10 at the input boundary.
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    let mut input = create_input("tag-evil", "Tagged", None);
    input.tags = vec!["fine".to_string(), "evil\n-->".to_string()];
    assert!(matches!(
        store.create_page(input).unwrap_err(),
        StoreError::InvalidName { .. }
    ));
}

#[test]
fn create_preserves_title_when_body_starts_with_h2() {
    // Review finding #20: a leading "## Section" is not a title.
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    let mut input = create_input("h2-body", "My Real Title", None);
    input.body = "## A Section\n\ntext\n".to_string();
    store.create_page(input).unwrap();
    let page = store.read_page("h2-body").unwrap();
    assert_eq!(page.title, "My Real Title", "supplied title kept, not dropped");
}

#[test]
fn non_utf8_file_is_skipped_not_fatal() {
    // Review finding #18: one bad file must not make the whole store unopenable.
    let dir = scratch_wiki();
    fs::write(dir.join("Broken.md"), [0x23, 0x20, 0xff, 0xfe, 0x0a]).unwrap();
    let store = LocalFolderStore::open(&dir).unwrap(); // must still open
    assert!(store.list_pages().iter().any(|p| p.title == "Home"));
    assert!(
        store.load_diagnostics().iter().any(|d| d.code == "store.unreadable-file"),
        "the skipped file is surfaced as a diagnostic"
    );
}

#[test]
fn duplicate_id_makes_mutations_refuse_safely() {
    // Review finding #9: with two files sharing an id (e.g. an interrupted
    // move), a mutation must refuse rather than act on an arbitrary file.
    let dir = scratch_wiki();
    let dup = "<!-- berrywiki\nid: 0195f6ec-36a2-7a42-b519-5f558842e256\nparent: null\nposition: 0\nkind: page\ntags: []\narchived: false\n-->\n\n# Twin\n";
    fs::write(dir.join("Twin.md"), dup).unwrap(); // same id as Assessment Plan
    let mut store = LocalFolderStore::open(&dir).unwrap();

    let err = store.update_page(PLAN_ID, "# x\n").unwrap_err();
    assert!(matches!(err, StoreError::AmbiguousId(_)), "update refused: {err}");
    let err = store.delete_page(PLAN_ID).unwrap_err();
    assert!(matches!(err, StoreError::AmbiguousId(_)), "delete refused: {err}");
    // Both files still on disk — nothing was acted on.
    assert!(dir.join("Twin.md").exists());
    assert!(dir.join("Teaching--Course-A--Assessment-Plan.md").exists());
}

#[test]
fn reload_is_deterministic() {
    // Review finding #15: repeated loads yield identical page order.
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    let first: Vec<String> = store.list_pages().into_iter().map(|p| p.id).collect();
    store.reload().unwrap();
    let second: Vec<String> = store.list_pages().into_iter().map(|p| p.id).collect();
    assert_eq!(first, second);
}

#[test]
fn external_modification_is_refused_as_stale_write() {
    // Guards the server + concurrent-terminal-git scenario: a file edited on
    // disk after we loaded it must not be silently clobbered.
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    fs::write(
        dir.join("Teaching.md"),
        "# Teaching\n\nedited by another process — clearly a different length\n",
    )
    .unwrap();

    match store.update_page(TEACHING_ID, "# Teaching\n\nour edit\n") {
        Err(StoreError::StaleWrite { .. }) => {}
        other => panic!("expected StaleWrite, got {other:?}"),
    }
    // The external change is intact; our write was refused.
    assert!(fs::read_to_string(dir.join("Teaching.md")).unwrap().contains("another process"));
}

/// Build one journal line for `old -> new`, stamping the old file's *current*
/// fingerprint (as move_page does), so recovery treats it as an unchanged
/// stale copy safe to delete.
fn journal_line(dir: &std::path::Path, old_rel: &str, new_rel: &str) -> String {
    let m = fs::metadata(dir.join(old_rel)).unwrap();
    let mtime = m
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{} {} {}\t{}\n", mtime, m.len(), old_rel, new_rel)
}

fn journal_path_for(dir: &std::path::Path) -> std::path::PathBuf {
    LocalFolderStore::open(dir)
        .unwrap()
        .appstate()
        .expect("app state resolved")
        .journal_path()
}

#[test]
fn crashed_move_is_recovered_on_open() {
    // Crash AFTER the new file was written but BEFORE the old one was deleted:
    // roll forward (delete the unchanged stale old file), never a tree reset.
    let dir = scratch_wiki();
    let jp = journal_path_for(&dir);
    fs::copy(dir.join("Home.md"), dir.join("Home-moved.md")).unwrap();
    fs::write(&jp, journal_line(&dir, "Home.md", "Home-moved.md")).unwrap();

    let store = LocalFolderStore::open(&dir).unwrap();
    assert!(!dir.join("Home.md").exists(), "stale old file cleaned up on open");
    assert!(dir.join("Home-moved.md").exists(), "moved content preserved");
    assert!(!jp.exists(), "journal cleared after recovery");
    assert_eq!(store.read_page(HOME_ID).unwrap().path, "Home-moved.md");
}

#[test]
fn recovery_preserves_an_old_file_edited_after_the_crash() {
    // Review finding #1: the user edits the old file directly between the crash
    // and reopening. Recovery must NOT delete it — its fingerprint no longer
    // matches the journalled one, so it is kept, not destroyed.
    let dir = scratch_wiki();
    let jp = journal_path_for(&dir);
    fs::copy(dir.join("Home.md"), dir.join("Home-moved.md")).unwrap();
    let line = journal_line(&dir, "Home.md", "Home-moved.md"); // fingerprint AS OF NOW
    fs::write(dir.join("Home.md"), "# Home\n\nEDITED AFTER THE CRASH — unique work\n").unwrap();
    fs::write(&jp, line).unwrap();

    let _ = LocalFolderStore::open(&dir).unwrap(); // runs recovery
    assert!(dir.join("Home.md").exists(), "post-crash edit must be preserved");
    assert!(fs::read_to_string(dir.join("Home.md")).unwrap().contains("EDITED AFTER THE CRASH"));
}

#[test]
fn recovery_completes_inbound_link_rewrites() {
    // Review finding #2/#7: a crash after the affected rename but before an
    // unaffected page's link was rewritten. Recovery heals the link so nothing
    // dangles to the deleted old stem.
    let dir = scratch_wiki();
    let jp = journal_path_for(&dir);
    fs::copy(dir.join("Teaching--Course-A.md"), dir.join("Research--Course-A.md")).unwrap();
    fs::write(dir.join("Linker.md"), "# Linker\n\nsee [[Teaching--Course-A]]\n").unwrap();
    fs::write(&jp, journal_line(&dir, "Teaching--Course-A.md", "Research--Course-A.md")).unwrap();

    let _ = LocalFolderStore::open(&dir).unwrap();
    assert!(!dir.join("Teaching--Course-A.md").exists(), "stale old file removed");
    assert!(dir.join("Research--Course-A.md").exists());
    let linker = fs::read_to_string(dir.join("Linker.md")).unwrap();
    assert!(linker.contains("[[Research--Course-A]]"), "dangling link healed: {linker}");
}

#[test]
fn reload_picks_up_external_edits() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    // Simulate an external editor adding a page (the "usable without the
    // app" guarantee in reverse).
    fs::write(dir.join("Hand-Written.md"), "# Hand Written\n\nvia $EDITOR\n").unwrap();
    store.reload().unwrap();
    assert!(store.list_pages().iter().any(|p| p.title == "Hand Written"));
}
