//! `LocalFolderStore` — the plain-directory adapter.
//!
//! This is the first `WikiStore`: it lets the engine, tests and (soon) the
//! read-only UI operate on any folder of Markdown pages — including a local
//! clone of a GitHub `.wiki.git` — without git or network access. Git-aware
//! adapters compose on top of it later.

use std::fs;
use std::path::{Path, PathBuf};

use berrywiki_core::{
    generate_sidebar, serialize_source, Diagnostic, PageGraph, PageMetadata, SidebarOptions,
    WikiPage,
};

use crate::error::StoreError;
use crate::paths::{page_filename, validate_component, with_id_suffix};
use crate::{Attachment, CreatePageInput, MovePageInput, PageSummary, Result, WikiStore};

const SIDEBAR_FILE: &str = "_Sidebar.md";
const FOOTER_FILE: &str = "_Footer.md";
const ASSETS_DIR: &str = "assets";

pub struct LocalFolderStore {
    root: PathBuf,
    graph: PageGraph,
    sidebar_options: SidebarOptions,
    /// Non-fatal problems encountered while loading (e.g. an unreadable or
    /// non-UTF-8 file that was skipped). Surfaced to the UI, never fatal.
    load_diagnostics: Vec<Diagnostic>,
}

impl LocalFolderStore {
    /// Open a wiki folder and build the initial graph.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root: PathBuf = root.into();
        let root = root
            .canonicalize()
            .map_err(|_| StoreError::RootNotFound(root.display().to_string()))?;
        if !root.is_dir() {
            return Err(StoreError::RootNotFound(root.display().to_string()));
        }
        let mut store = LocalFolderStore {
            root,
            graph: PageGraph::build(Vec::new()),
            sidebar_options: SidebarOptions::default(),
            load_diagnostics: Vec::new(),
        };
        store.reload()?;
        Ok(store)
    }

    /// The canonical store root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Non-fatal diagnostics from the last load (skipped/unreadable files).
    pub fn load_diagnostics(&self) -> &[Diagnostic] {
        &self.load_diagnostics
    }

    /// Validate a prospective parent: it must exist and be *managed* (have
    /// metadata), because an unmanaged page's id is its filename — a volatile
    /// value that would break every child the moment the parent is renamed.
    fn check_parent(&self, parent_id: Option<&str>) -> Result<()> {
        if let Some(parent) = parent_id {
            let page = self
                .graph
                .get(parent)
                .ok_or_else(|| StoreError::ParentNotFound(parent.to_string()))?;
            if page.metadata.is_none() {
                return Err(StoreError::UnmanagedParent(parent.to_string()));
            }
        }
        Ok(())
    }

    /// True if a mutation on `id` is unsafe because two files share that id.
    fn ensure_unambiguous(&self, id: &str) -> Result<()> {
        let ambiguous = self
            .graph
            .diagnostics()
            .iter()
            .any(|d| d.code == "graph.duplicate-id" && d.page.as_deref() == Some(id));
        if ambiguous {
            return Err(StoreError::AmbiguousId(id.to_string()));
        }
        Ok(())
    }

    /// True if some existing top-level entry matches `name` case-insensitively.
    /// Excludes `except` (a page's own current filename) so a rename-in-place
    /// does not collide with itself.
    fn filename_taken_ci(&self, name: &str, except: Option<&str>) -> bool {
        let lower = name.to_lowercase();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return self.root.join(name).exists();
        };
        for entry in entries.flatten() {
            if let Some(existing) = entry.file_name().to_str() {
                if Some(existing) == except {
                    continue;
                }
                if existing.to_lowercase() == lower {
                    return true;
                }
            }
        }
        false
    }

    /// Resolve a validated, root-relative file path. The single gate through
    /// which every write goes; guarantees the result stays under the root.
    fn resolve(&self, relative: &str) -> Result<PathBuf> {
        for component in relative.split(['/', '\\']) {
            validate_component(component).map_err(|_| StoreError::InvalidName {
                name: relative.to_string(),
                reason: "invalid path component".to_string(),
            })?;
        }
        let joined = self.root.join(relative);
        // Belt and braces: the parent that exists must still be under root.
        let mut probe = joined.clone();
        while !probe.exists() {
            match probe.parent() {
                Some(p) => probe = p.to_path_buf(),
                None => break,
            }
        }
        let canonical = probe
            .canonicalize()
            .map_err(|e| StoreError::io(format!("resolving {relative:?}"), e))?;
        if !canonical.starts_with(&self.root) {
            return Err(StoreError::InvalidName {
                name: relative.to_string(),
                reason: "escapes the wiki root".to_string(),
            });
        }
        Ok(joined)
    }

    /// Atomic write: temp file in the same directory, then rename over.
    ///
    /// The temp file is opened with `create_new` (O_CREAT|O_EXCL), which
    /// refuses to follow a pre-existing entry — including a dangling symlink
    /// planted at the temp path by hostile repo content. Any such leftover is
    /// removed (the symlink itself, never its target) and creation retried
    /// once. The final `rename` replaces the target *entry*, so a symlink at
    /// the target is replaced, not followed.
    fn safe_write(&self, relative: &str, content: &[u8]) -> Result<()> {
        use std::io::Write;

        let target = self.resolve(relative)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| StoreError::io(format!("creating directory for {relative:?}"), e))?;
        }
        let tmp_name = format!(
            "{}.tmp-{}",
            target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            std::process::id()
        );
        let tmp = target.with_file_name(&tmp_name);

        let open = |p: &Path| fs::OpenOptions::new().write(true).create_new(true).open(p);
        let mut file = match open(&tmp) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Stale temp or planted symlink: remove the entry, retry once.
                fs::remove_file(&tmp)
                    .map_err(|e| StoreError::io(format!("clearing stale temp for {relative:?}"), e))?;
                open(&tmp)
                    .map_err(|e| StoreError::io(format!("creating temp file for {relative:?}"), e))?
            }
            Err(e) => {
                return Err(StoreError::io(format!("creating temp file for {relative:?}"), e))
            }
        };
        file.write_all(content)
            .map_err(|e| StoreError::io(format!("writing temp file for {relative:?}"), e))?;
        // Flush the file's contents to disk BEFORE the rename, so the "both
        // files, never neither" move guarantee holds across a power loss: the
        // renamed entry can never point at partially-written data.
        file.sync_all()
            .map_err(|e| StoreError::io(format!("syncing temp file for {relative:?}"), e))?;
        drop(file);
        fs::rename(&tmp, &target).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            StoreError::io(format!("renaming temp file into {relative:?}"), e)
        })?;
        // Best-effort: persist the rename itself by syncing the directory.
        // Unsupported on some platforms/filesystems — a failure here does not
        // undo a successful rename, so it is intentionally not fatal.
        if let Some(parent) = target.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }

    fn page(&self, id: &str) -> Result<&WikiPage> {
        self.graph
            .get(id)
            .ok_or_else(|| StoreError::PageNotFound(id.to_string()))
    }

    /// Titles of the ancestor chain (root-first) for filename generation.
    ///
    /// Root pages (parent: null) contribute **no** segment: `Home.md` sits
    /// beside `Teaching.md` in a GitHub wiki, so a page under Home is
    /// `Teaching--Course-A.md`, not `Home--Teaching--Course-A.md`. Provisional
    /// under ADR-0001; revisit with the live-spike evidence.
    fn ancestor_titles(&self, parent_id: Option<&str>) -> Result<Vec<String>> {
        let mut chain = Vec::new();
        let mut cursor = parent_id.map(|s| s.to_string());
        let mut hops = 0;
        while let Some(pid) = cursor {
            let parent = self
                .graph
                .get(&pid)
                .ok_or_else(|| StoreError::ParentNotFound(pid.clone()))?;
            if parent.parent_id().is_some() {
                chain.push(parent.title.clone());
            }
            cursor = parent.parent_id().map(|s| s.to_string());
            hops += 1;
            if hops > 64 {
                // Cycle in stored metadata; the graph flags it, we refuse here.
                return Err(StoreError::CycleDetected {
                    page: pid,
                    parent: parent_id.unwrap_or_default().to_string(),
                });
            }
        }
        chain.reverse();
        Ok(chain)
    }

    /// True if `candidate_ancestor` appears in the ancestry of `page_id`.
    fn is_ancestor(&self, page_id: &str, candidate_ancestor: &str) -> bool {
        let mut cursor = Some(page_id.to_string());
        let mut hops = 0;
        while let Some(pid) = cursor {
            if pid == candidate_ancestor {
                return true;
            }
            cursor = self
                .graph
                .get(&pid)
                .and_then(|p| p.parent_id())
                .map(|s| s.to_string());
            hops += 1;
            if hops > 64 {
                return true; // treat runaway ancestry as a cycle: refuse
            }
        }
        false
    }

    /// Choose a collision-free filename, falling back to an id suffix.
    ///
    /// Collision detection is **case-insensitive** (a wiki cloned onto Windows
    /// or macOS cannot hold `Plan.md` and `plan.md` at once, and GitHub link
    /// resolution would be ambiguous). `own_path` is the moving page's current
    /// filename, excluded from the check so a same-place rename or a reposition
    /// of a suffix-named page does not collide with itself.
    fn unique_filename(
        &self,
        ancestors: &[String],
        title: &str,
        id: &str,
        own_path: Option<&str>,
    ) -> Result<String> {
        let name = page_filename(ancestors, title)?;
        if !self.filename_taken_ci(&name, own_path) {
            return Ok(name);
        }
        let suffixed = with_id_suffix(&name, id);
        // The suffix embeds caller-supplied id characters: re-validate the
        // final name rather than trusting the pre-suffix validation.
        validate_component(&suffixed)?;
        if self.filename_taken_ci(&suffixed, own_path) {
            return Err(StoreError::InvalidName {
                name: suffixed,
                reason: "filename collision even with id suffix".to_string(),
            });
        }
        Ok(suffixed)
    }

    /// Regenerate the sidebar, writing only when the content changed.
    fn write_sidebar_if_changed(&mut self) -> Result<bool> {
        let content = generate_sidebar(&self.graph, &self.sidebar_options);
        let existing = fs::read_to_string(self.root.join(SIDEBAR_FILE)).ok();
        if existing.as_deref() == Some(content.as_str()) {
            return Ok(false);
        }
        self.safe_write(SIDEBAR_FILE, content.as_bytes())?;
        Ok(true)
    }
}

impl WikiStore for LocalFolderStore {
    fn reload(&mut self) -> Result<()> {
        let mut load_diagnostics = Vec::new();

        // Collect eligible page filenames, then sort: read_dir order is
        // OS-dependent, and the graph's tie-breaking must be deterministic
        // (so a crash-mid-move duplicate-id state resolves the same way every
        // load).
        let read = fs::read_dir(&self.root).map_err(|e| StoreError::io("listing the wiki folder", e))?;
        let mut names: Vec<String> = Vec::new();
        for entry in read {
            let entry = entry.map_err(|e| StoreError::io("listing the wiki folder", e))?;
            // Symlinks are never managed content: `is_file()` follows links
            // (so a link to an outside file would be silently ingested) and
            // dangling links would otherwise linger invisibly.
            let file_type = entry
                .file_type()
                .map_err(|e| StoreError::io("inspecting a wiki folder entry", e))?;
            if file_type.is_symlink() || !entry.path().is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !name.ends_with(".md") || name == SIDEBAR_FILE || name == FOOTER_FILE {
                continue;
            }
            names.push(name);
        }
        names.sort();

        let mut pages = Vec::with_capacity(names.len());
        for name in names {
            match fs::read_to_string(self.root.join(&name)) {
                Ok(source) => pages.push(WikiPage::parse(name, source)),
                // A single unreadable/non-UTF-8 file must not make the whole
                // wiki unopenable — skip it with a diagnostic (non-negotiable
                // "malformed input degrades, never crashes").
                Err(e) => load_diagnostics.push(
                    Diagnostic::warning(
                        "store.unreadable-file",
                        format!("Skipped {name:?}: {e}. Fix or remove the file to include it."),
                    )
                    .with_page(name),
                ),
            }
        }
        self.graph = PageGraph::build(pages);
        self.load_diagnostics = load_diagnostics;
        Ok(())
    }

    fn graph(&self) -> &PageGraph {
        &self.graph
    }

    fn list_pages(&self) -> Vec<PageSummary> {
        self.graph
            .walk()
            .into_iter()
            .map(|(_, p)| PageSummary {
                id: p.id.clone(),
                title: p.title.clone(),
                path: p.path.clone(),
                archived: p.is_archived(),
            })
            .collect()
    }

    fn read_page(&self, id: &str) -> Result<&WikiPage> {
        self.page(id)
    }

    fn create_page(&mut self, input: CreatePageInput) -> Result<String> {
        // Ids flow into filenames, attachment directories and metadata:
        // reject anything but BerryWiki's own id alphabet up front.
        crate::paths::validate_page_id(&input.id)?;
        // Tags must not be able to corrupt the metadata block; reject rather
        // than silently sanitise so the caller sees a clear error.
        for tag in &input.tags {
            if berrywiki_core::sanitises_field(tag) {
                return Err(StoreError::InvalidName {
                    name: tag.clone(),
                    reason: "tag contains a newline, control character or '-->'".to_string(),
                });
            }
        }
        if self.graph.get(&input.id).is_some() {
            return Err(StoreError::DuplicateId(input.id));
        }
        self.check_parent(input.parent_id.as_deref())?;

        let ancestors = self.ancestor_titles(input.parent_id.as_deref())?;
        let filename = self.unique_filename(&ancestors, &input.title, &input.id, None)?;

        let mut meta = PageMetadata::new(input.id.clone());
        meta.parent_id = input.parent_id.clone();
        meta.position = input.position;
        meta.kind = input.kind.clone();
        meta.tags = input.tags.clone();

        // Prepend the title only when the body does not already open with an
        // H1. A leading `##`/`###` is NOT a title, so we must still prepend —
        // testing for a bare `#` would drop the caller's title.
        let body = if body_has_leading_h1(&input.body) {
            input.body.clone()
        } else if input.body.trim().is_empty() {
            format!("# {}\n", input.title)
        } else {
            format!("# {}\n\n{}", input.title, input.body)
        };
        let source = serialize_source(Some(&meta), &body);

        self.safe_write(&filename, source.as_bytes())?;
        self.reload()?;
        self.write_sidebar_if_changed()?;
        Ok(input.id)
    }

    fn update_page(&mut self, id: &str, new_body: &str) -> Result<()> {
        self.ensure_unambiguous(id)?;
        let page = self.page(id)?;
        let path = page.path.clone();
        let meta = page.metadata.clone();
        let source = serialize_source(meta.as_ref(), new_body);
        self.safe_write(&path, source.as_bytes())?;
        self.reload()?;
        // Title may have changed with the body's H1 → sidebar may change.
        self.write_sidebar_if_changed()?;
        Ok(())
    }

    fn move_page(&mut self, input: MovePageInput) -> Result<()> {
        self.ensure_unambiguous(&input.id)?;
        let page = self.page(&input.id)?;
        let Some(meta) = page.metadata.clone() else {
            return Err(StoreError::UnmanagedPage(input.id));
        };
        let old_path = page.path.clone();
        let title = page.title.clone();
        let body = page.body.clone();

        self.check_parent(input.new_parent_id.as_deref())?;
        if let Some(parent) = &input.new_parent_id {
            if parent == &input.id || self.is_ancestor(parent, &input.id) {
                return Err(StoreError::CycleDetected {
                    page: input.id,
                    parent: parent.clone(),
                });
            }
        }

        let mut new_meta = meta;
        new_meta.parent_id = input.new_parent_id.clone();
        new_meta.position = input.new_position;

        let ancestors = self.ancestor_titles(input.new_parent_id.as_deref())?;
        // Exclude the page's own current file from the collision check, so a
        // pure reposition (or reparent-and-return) of a suffix-named page is
        // not rejected as colliding with itself.
        let new_path = if page_filename(&ancestors, &title)? == old_path {
            old_path.clone()
        } else {
            self.unique_filename(&ancestors, &title, &input.id, Some(&old_path))?
        };

        let source = serialize_source(Some(&new_meta), &body);

        // Write-then-delete: a crash in between leaves both files (duplicate
        // id diagnostic, recoverable), never neither.
        self.safe_write(&new_path, source.as_bytes())?;
        if new_path != old_path {
            let old_abs = self.resolve(&old_path)?;
            fs::remove_file(&old_abs)
                .map_err(|e| StoreError::io(format!("removing old file {old_path:?}"), e))?;
        }
        self.reload()?;
        self.write_sidebar_if_changed()?;
        Ok(())
    }

    fn delete_page(&mut self, id: &str) -> Result<()> {
        self.ensure_unambiguous(id)?;
        let page = self.page(id)?;
        let path = page.path.clone();
        let children = self.graph.children_of(id);
        if !children.is_empty() {
            return Err(StoreError::HasChildren {
                page: id.to_string(),
                child_count: children.len(),
            });
        }
        let abs = self.resolve(&path)?;
        fs::remove_file(&abs)
            .map_err(|e| StoreError::io(format!("deleting page file {path:?}"), e))?;
        self.reload()?;
        self.write_sidebar_if_changed()?;
        Ok(())
    }

    fn add_attachment(&mut self, page_id: &str, filename: &str, bytes: &[u8]) -> Result<Attachment> {
        self.ensure_unambiguous(page_id)?;
        let page = self.page(page_id)?; // page must exist…
        if page.metadata.is_none() {
            // …and be managed: an unmanaged page's id is its filename, so an
            // assets/<id>/ directory keyed on it would break the instant the
            // page is renamed. Require a stable id first.
            return Err(StoreError::UnmanagedParent(page_id.to_string()));
        }
        validate_component(filename)?;
        // Attachment directories are keyed by page id (stable across renames).
        // Page ids come from metadata; validate them as a path component too.
        validate_component(page_id).map_err(|_| StoreError::InvalidName {
            name: page_id.to_string(),
            reason: "page id is not usable as a directory name".to_string(),
        })?;
        let relative = format!("{ASSETS_DIR}/{page_id}/{filename}");
        let abs = self.root.join(&relative);
        if abs.exists() {
            return Err(StoreError::DuplicateAttachment {
                page: page_id.to_string(),
                filename: filename.to_string(),
            });
        }
        self.safe_write(&relative, bytes)?;
        Ok(Attachment {
            page_id: page_id.to_string(),
            filename: filename.to_string(),
            path: relative,
        })
    }

    fn regenerate_sidebar(&mut self) -> Result<bool> {
        self.write_sidebar_if_changed()
    }
}

/// True when the body's first non-empty line is a level-1 ATX heading (`# …`).
/// A `##`/`###` opener is deliberately *not* treated as a title.
fn body_has_leading_h1(body: &str) -> bool {
    match body.lines().find(|l| !l.trim().is_empty()) {
        Some(line) => {
            let t = line.trim_start();
            t.starts_with("# ") || t == "#"
        }
        None => false,
    }
}
