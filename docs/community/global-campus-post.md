<!--
SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# GitHub Global Campus Teachers — post drafts

**Status: DRAFT. Not posted, and not to be posted by an agent.**

Decision D-9 (ruled 2026-09-02) names the venue as **GitHub Global Campus
Teachers** (`community/Global-Campus-Teachers`), the private Discussions
repository for teachers holding GitHub Education benefits. Package `P6-edu-post`
is explicitly **owner-written and owner-posted**; this file exists so the owner
has something to edit down and paste, never so that anything is published
automatically.

## Before either variant can be posted

Both drafts below are written to be *true on the day they are posted*, and the
drafts are annotated where they depend on a precondition.

**Variant B is postable now.** Ruling R-H (2026-09-15) records that the two
unmet preconditions gate **Variant A only**: Variant B neither mentions a
release nor asserts a GitHub Wiki behaviour, so nothing it says depends on
either. Under D-11(c) the template is the delivery and this post is only an
amplifier, and under D-12(b) a teacher installs nothing — so `v0.1.0` is the
*developer* door, not the teacher one. Cutting `v0.1.0` (and with it P5-guix,
per R-C) remains required for Variant A and is tracked separately.

| Precondition | State as of 2026-09-15 | Gates which variant |
|---|---|---|
| A `v0.1.0` release exists | **Not met.** Zero tags, zero releases on the repo | **Variant A only.** Variant B makes no claim about a release, tag, version or download — it says there is nothing to install |
| `P1-spike-read` has run against a real `.wiki.git` | **Not met.** Credential-gated; every GitHub Wiki behaviour is still reasoned from documentation, not measured | **Variant A only.** Variant B's sole GitHub-behaviour claim is that the template's pages render, which is a browser observation rather than the spike |
| `metadatastician/berrywiki-course-template` exists | **Met, 2026-09-15.** Public, `isTemplate: true`, CC-BY-SA-4.0, and its `structure check` workflow is green at `9e83349` | Variant B only |

The first two are why neither draft makes a single claim about how GitHub Wikis
behave. That is a deliberate constraint from the roadmap: *the post must not
claim GitHub behaviours the spikes have not verified.* Everything asserted below
is a property of BerryWiki's own code and its own published wiki, both of which
are measured.

Two further house rules are baked in:

* **Do not lead with AI.** A recent thread in that forum is about limiting
  Copilot by default. Neither draft mentions AI at all.
* **Do not oversell accessibility.** The structural gate is real and enforced on
  every route; the screen-reader walkthrough has never been run. Variant A says
  exactly that, in those terms.

---

## Variant A — show-and-tell (General, or GitHub Classroom)

> **Suggested title:** A hierarchical notebook view over a GitHub Wiki — plain
> Markdown, no JavaScript, and it still works if you stop using it

Hello all,

<!-- OWNER: open in your own words and with your own experience — how you came to
     the problem, and in what teaching context. Everything below this line is a
     measured property of the code; this opening is the one part only you can
     write, and it is the part readers weigh most. -->

A GitHub Wiki is a *flat* list of pages. Once a module wiki passes twenty or
thirty pages — syllabus, weekly notes, lab sheets, reading, assessment briefs —
nobody can find anything in it.

The tools that solve this well (CherryTree, Zim, Obsidian) all solve it by
owning the storage. That is exactly what I did not want for course material,
because course material outlives the tool you wrote it in.

So I built **BerryWiki**, and the whole design rests on one rule:

> The wiki must stay fully usable when BerryWiki is not.

Every page stays an ordinary Markdown file in the ordinary `.wiki.git`
repository. The hierarchy lives in an HTML comment at the top of each page,
which GitHub does not render. The sidebar BerryWiki generates is a plain
`_Sidebar.md`, which GitHub renders natively. If I stopped maintaining the
project tomorrow, you would be left with a wiki — not with an export problem.

**What you get on top of a normal wiki**

* A real page tree with sibling ordering and nesting, plus backlinks.
* Consistency diagnostics — broken links, missing parents, cycles, duplicate
  ids — surfaced in the UI and as a CLI command with a non-zero exit code, so it
  can run in CI over a course wiki.
* A zero-JavaScript editor. No `<script>` is served on any route, and a test
  asserts that over every route rather than trusting me to remember.
* Saves that refuse rather than clobber: if a page changed since you opened the
  editor, the save is rejected with your text preserved both in the form and as
  a draft.
  <!-- OWNER: if this has actually mattered in your own teaching — two people
       editing the same week's page — say so here in your own words. Leave the
       line out rather than assert it secondhand. -->
* Commit-on-save: one save is one commit, sidebar included. It never
  force-pushes, never starts a merge, and never merges two authored sides on
  your behalf.
* Moving a page moves its whole subtree — every descendant renamed, every
  inbound link rewritten, the sidebar regenerated, as a single commit that
  cannot half-apply. There is a Preview that writes nothing.

**Where it is honest about its limits**

* It runs on `localhost` (port 23779, loopback only). It is a single-user
  companion, not a server, and there is no hosted version.
* Accessibility: there is a structural gate of fourteen rules — heading order,
  landmarks, a skip link, accessible names, text alternatives wherever colour
  alone would carry meaning — and it runs over every rendered route in the test
  suite. Writing it found a route that two separate "every route" tests had both
  missed. **But the manual screen-reader walkthrough has not been run yet**, so
  I am not claiming screen-reader support, only that the structure is gated.
* It imports CherryTree notebooks today. Zim import is next.
* Licence is MPL-2.0 for code, CC-BY-SA-4.0 for the prose.

**The wiki is the demo.** The project's own wiki is written in BerryWiki's
format and its sidebar is BerryWiki-generated, so you can see the output without
installing anything:

<!-- Link to the wiki. -->
https://github.com/metadatastician/berrywiki/wiki

<!-- Insert the v0.1.0 release link here once the release exists. Do not post
     this variant before then: "try it" with nothing to download is the single
     most common complaint on show-and-tell threads. -->

I would genuinely like to know whether the hierarchy-in-a-comment idea survives
contact with how you actually run a module wiki, and whether the diagnostics
catch the things that break in yours. Happy to answer anything.

---

## Variant B — Starter code exchange

> **Suggested title:** Course wiki starter — a pre-structured module wiki you
> can "Use this template" and fill in

<!-- CLEARED TO POST as of 2026-09-15 (ruling R-G + R-H).
     metadatastician/berrywiki-course-template is public, isTemplate: true, and
     its structure check is green at 9e83349. Under ruling D-11(c) the template
     is the delivery and this post is only an amplifier — the template stands on
     its own. Remaining owner steps: fill the OWNER markers below, read it once
     against the two house rules (do not lead with AI; do not oversell
     accessibility), and post. D-9: the owner posts, never an agent. -->

Hello all,

This is a **template repository for a module wiki**, not an application:

<https://github.com/metadatastician/berrywiki-course-template>

You press *Use this template*, and you get a wiki tree already laid out the way
a module actually runs:

* `Home`, `Syllabus`
* `Weeks/` — Weeks 1 to 12
* `Assignments/`, `Groups/`, `Resources/`
* a `Staff/` subtree for answer keys and drafts

Every page is plain Markdown. Keep the tree as a folder in the repository, where
it can be reviewed in a pull request, or publish it to the repository's GitHub
Wiki with a single `git subtree push` — the README shows both.

There is nothing to install and nothing to run — if you never touch the tooling,
you still have a properly structured course wiki, which is the point.

The structure is machine-checkable: a CLI command validates the tree (broken
links, missing parents, cycles, duplicate ids) and exits non-zero, so you can
run it in Actions over your own course wiki and find out that Week 7 links to a
page you deleted *before* a student does.

Optionally, the same tree opens as a hierarchical notebook in
[BerryWiki](https://github.com/metadatastician/berrywiki) — a local,
zero-JavaScript companion for GitHub Wikis. That is strictly optional, and the
template is designed so that it reads correctly without it.

Licence is MPL-2.0 for code and CC-BY-SA-4.0 for the prose, so you can adapt it
for your own institution.

---

## Notes for the owner, not for posting

* The `Staff/` subtree in Variant B uses an `access:` key that **does not do
  anything yet** — `P7-access-model` has not been built. Until it has, the key is
  preserved as unknown metadata, which is correct behaviour but is *not* access
  control. Do not describe it as a permission, a lock, or anything that implies
  students cannot read it. Classroom repositories being private by default is
  what is actually protecting it.
* If asked "does it work with GitHub Enterprise / a self-hosted instance", the
  honest answer is that it has only ever been pointed at `github.com` wikis and
  the compatibility notes are a hypothesis list, not a test report.
* If asked about multi-user or hosting, the answer is no by design: ruling D-2
  settled on one team, one wiki, on-prem, and there is no registry and no
  org-wide index.
