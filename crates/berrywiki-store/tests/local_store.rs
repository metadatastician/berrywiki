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
fn reload_picks_up_external_edits() {
    let dir = scratch_wiki();
    let mut store = LocalFolderStore::open(&dir).unwrap();
    // Simulate an external editor adding a page (the "usable without the
    // app" guarantee in reverse).
    fs::write(dir.join("Hand-Written.md"), "# Hand Written\n\nvia $EDITOR\n").unwrap();
    store.reload().unwrap();
    assert!(store.list_pages().iter().any(|p| p.title == "Hand Written"));
}
