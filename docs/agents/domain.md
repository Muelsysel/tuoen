# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

**Layout: single-context.** One `GLOSSARY.md` and one `docs/adr/` at the repo root.

## Before exploring, read these

- **`GLOSSARY.md`** at the repo root.
- **`docs/adr/`**: read ADRs that touch the area you're about to work in.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The `/domain-modeling` skill (reached via `/grill-with-docs` and `/improve-codebase-architecture`) creates them lazily when terms or decisions actually get resolved.

## File structure

```
/
├── GLOSSARY.md
├── docs/
│   ├── adr/
│   │   ├── 0001-<decision>.md
│   │   └── 0002-<decision>.md
│   ├── DESIGN.md        ← the agreed design spec (see note below)
│   ├── CODE_SIGNING.md
│   └── UNSIGNED_BUILD.md
└── crates/
```

## Note on `docs/DESIGN.md`

`docs/DESIGN.md` predates this setup: it is the record of a seven-round design interview, holding 35 numbered decisions **with their rationale**. Treat it as the project's decision log and as required reading before touching the areas it covers — it is the reason several otherwise-reasonable designs are forbidden here (for example: never write `PATH` with `setx`; never rely on symlinks being creatable without elevation).

**Its relationship to ADRs**: `DESIGN.md` is broad and pre-code. An **ADR** is for a single hard-to-reverse decision made *while working*, especially one that supersedes or refines something in `DESIGN.md`. When an ADR contradicts `DESIGN.md`, the ADR wins — but say so explicitly in the ADR, and update `DESIGN.md` in the same change.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `GLOSSARY.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal: either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0007 (junction-first switching), but worth reopening because…_
