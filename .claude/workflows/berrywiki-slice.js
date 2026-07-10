// BerryWiki tiered slice workflow.
//
// Runs one work package through implement -> adversarial-verify, using the
// Claude tier appropriate to the package. Pass args:
//   { id, tier, prompt, verifyTier }
// where `tier` is 'opus' | 'sonnet' | 'haiku' (implementation model) and
// `prompt` describes the package (usually lifted from
// docs/execution/work-packages.adoc). Verification always defaults to opus,
// because catching a subtle breakage is itself a hard-tier job.
//
// Example:
//   Workflow({ name: 'berrywiki-slice', args: {
//     id: 'P1-render', tier: 'sonnet',
//     prompt: 'Build berrywiki-render: comrak GFM -> HTML, stated raw-HTML policy, deterministic, tests.'
//   }})

export const meta = {
  name: 'berrywiki-slice',
  description: 'Implement one BerryWiki work package at the right model tier, then adversarially verify it',
  phases: [
    { title: 'Implement' },
    { title: 'Verify' },
  ],
}

const id = (args && args.id) || 'unnamed-package'
const tier = (args && args.tier) || 'sonnet'
const verifyTier = (args && args.verifyTier) || 'opus'
const packagePrompt = (args && args.prompt) || 'No package prompt supplied.'

const CONSTRAINTS = `
BerryWiki locked constraints (from the repo CLAUDE.md — violations are bugs):
- TypeScript and hand-written JavaScript are banned. Core = Rust. UI = zero-<script> SSR (axum + maud + comrak).
- Native GitHub wiki reader must work; content usable without the app.
- Git safety: never force-push, never discard local work, fetch before push, atomic logical commits.
- Determinism; malformed input degrades with a diagnostic, never panics/corrupts.
- Never claim live GitHub behaviour verified unless actually run against a real wiki.
- Technical docs are AsciiDoc; wiki/community-health files are Markdown.
- Repo is WSL-only for writes; cargo test --workspace + warning-free build gate completion.
Read docs/execution/work-packages.adoc for this package's contract and escalation triggers.
`

phase('Implement')
const implementation = await agent(
  `You are implementing BerryWiki work package ${id}.\n${CONSTRAINTS}\n\nPackage:\n${packagePrompt}\n\n` +
    `Deliver a small, complete, tested vertical slice. Add unit + integration tests. If a package escalation trigger fires (git history / data-safety / subtree cascade / ADR contradiction / unverified-GitHub assumption), STOP and report what must go to a higher tier instead of guessing. Report the exact files changed, tests added, and test results.`,
  { label: `implement:${id}`, phase: 'Implement', model: tier },
)

phase('Verify')
const verdict = await agent(
  `You are an adversarial reviewer for BerryWiki work package ${id}.\n${CONSTRAINTS}\n\n` +
    `Here is the implementer's report:\n${implementation}\n\n` +
    `Read the actual changed code in the repo. Hunt for real defects with concrete failing scenarios: data-loss / corruption paths, doctrine violations (any JS of any provenance, force-push reachability), non-determinism, unhandled malformed input, and untested claimed behaviour. For each finding give file, a concrete input->wrong-outcome scenario, and a fix. Only report defects you can substantiate; an empty list is a valid result for sound work.`,
  { label: `verify:${id}`, phase: 'Verify', model: verifyTier, effort: 'high' },
)

return { id, tier, implementation, verdict }
