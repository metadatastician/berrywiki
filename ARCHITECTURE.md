# BerryWiki Architecture

## Overview
BerryWiki is a deterministic, natively pluggable wiki engine designed for maximum performance, strict data integrity, and unparalleled advanced presentation capabilities. Rather than building a monolith, BerryWiki is split into three highly decoupled, modular layers.

## The Three Layers

### 1. The Deterministic Core (`berrywiki-core`)
**Language:** Rust
This is the foundational I/O and state management layer. It is built strictly for deterministic atomic updates, rapid parallel parsing, and generating the abstract semantic graph.
- Handles atomic file/blob writing.
- Generates and maintains the Semantic Knowledge Graph (RDFa / JSON-LD indices).
- Tracks broken links natively via a Broken Link Auto-Healer mechanism.

### 2. The Unified Gateway (`zig-unified-hexdeca-api`)
**Language:** Zig
Located in `api/zig-unified-hexdeca-api`, this is the external-facing service layer. 
- Wraps the Rust core with an extremely low-latency, memory-safe API.
- Provides endpoints for Git Forgers (like GitHub, GitLab) and external integrations.
- Makes BerryWiki seamlessly "pluggable" as a headless engine on third-party servers.

### 3. The Advanced Presentation Layer (BerryBlocks)
**Language:** Web / JS / WASM / A2ML
Rather than being tightly coupled to the engine, the presentation layer is built as a **Standalone Pluggable Component**. This allows the advanced UI to be injected into BerryWiki, `ddraig-ssg`, or any `nextgen-languages` previewer.
Inspired by `formatrix-docs`, features include:
- **Switchable Code Blocks:** Toggle blocks by OS, Shell, Language, or Citation Style.
- **A2ML Variable Substitution:** Interactively toggle between code view and preview, tweaking variables dynamically.
- **Smart Paste Parsing:** Paste buffers map intelligently into plain text, arrays, matrices, or tuples.
- **AAA WCAG Accessibility:** Extreme compliance for interactive markdown blocks.
- **Editor Tooling:** Side-by-side or top-and-bottom preview split-modes, "glyph/block mode" layouts, and live-linting sidebars.

## Out-of-the-Box Integrations
- **SEO Optimization:** BerryWiki ships pre-configured with `git-seo` hooks to ensure maximum search engine visibility for generated static wikis.
- **Semantic Network:** Every output page automatically renders Obsidian-style network graphs showing page inter-connectivity.
