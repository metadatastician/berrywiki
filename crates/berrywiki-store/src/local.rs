//! `LocalFolderStore` — the plain-directory adapter.
//!
//! This is the first `WikiStore`: it lets the engine, tests and (soon) the
//! read-only UI operate on any folder of Markdown pages — including a local
//! clone of a GitHub `.wiki.git` — without git or network access. Git-aware
//! adapters compose on top of it later.

use std::fs;
use std::path::{Path, PathBuf};

use berrywiki_core::{
    generate_sidebar, serialize_source, PageGraph, PageMetadata, SidebarOptions, WikiPage,
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
        };
        store.reload()?;
        Ok(store)
    }

    /// The canonical store root.
    pub fn root(&self) -> &Path {
        &self.root
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
    fn safe_write(&self, relative: &str, content: &[u8]) -> Result<()> {
        let target = self.resolve(relative)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| StoreError::io(format!("creating directory for {relative:?}"), e))?;
        }
        let tmp = target.with_extension(format!("tmp-{}", std::process::id()));
        fs::write(&tmp, content)
            .map_err(|e| StoreError::io(format!("writing temp file for {relative:?}"), e))?;
        fs::rename(&tmp, &target).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            StoreError::io(format!("renaming temp file into {relative:?}"), e)
        })?;
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
    fn unique_filename(&self, ancestors: &[String], title: &str, id: &str) -> Result<String> {
        let name = page_filename(ancestors, title)?;
        if !self.root.join(&name).exists() {
            return Ok(name);
        }
        let suffixed = with_id_suffix(&name, id);
        if self.root.join(&suffixed).exists() {
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
        let mut pages = Vec::new();
        let entries = fs::read_dir(&self.root)
            .map_err(|e| StoreError::io("listing the wiki folder", e))?;
        for entry in entries {
            let entry = entry.map_err(|e| StoreError::io("listing the wiki folder", e))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.ends_with(".md") || name == SIDEBAR_FILE || name == FOOTER_FILE {
                continue;
            }
            let source = fs::read_to_string(&path)
                .map_err(|e| StoreError::io(format!("reading page {name:?}"), e))?;
            pages.push(WikiPage::parse(name.to_string(), source));
        }
        self.graph = PageGraph::build(pages);
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
        if self.graph.get(&input.id).is_some() {
            return Err(StoreError::DuplicateId(input.id));
        }
        if let Some(parent) = &input.parent_id {
            if self.graph.get(parent).is_none() {
                return Err(StoreError::ParentNotFound(parent.clone()));
            }
        }

        let ancestors = self.ancestor_titles(input.parent_id.as_deref())?;
        let filename = self.unique_filename(&ancestors, &input.title, &input.id)?;

        let mut meta = PageMetadata::new(input.id.clone());
        meta.parent_id = input.parent_id.clone();
        meta.position = input.position;
        meta.kind = input.kind.clone();
        meta.tags = input.tags.clone();

        let body = if input.body.trim_start().starts_with('#') {
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
        let page = self.page(&input.id)?;
        let Some(meta) = page.metadata.clone() else {
            return Err(StoreError::UnmanagedPage(input.id));
        };
        let old_path = page.path.clone();
        let title = page.title.clone();
        let body = page.body.clone();

        if let Some(parent) = &input.new_parent_id {
            if self.graph.get(parent).is_none() {
                return Err(StoreError::ParentNotFound(parent.clone()));
            }
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
        let new_path = if page_filename(&ancestors, &title)? == old_path {
            old_path.clone()
        } else {
            self.unique_filename(&ancestors, &title, &input.id)?
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
        self.page(page_id)?; // page must exist
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
