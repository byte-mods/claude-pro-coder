# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project does not yet adhere to semantic versioning — pre-1.0 releases
may break compatibility on minor bumps.

## [Unreleased]

## [0.5.1] - 2026-09-18

Fixes `/pro-coder` being rejected outright on Fable 5.1 with
`API Error: ... safeguards flagged this message ... [reasoning_extraction]`.

### Fixed
- **Skill no longer reads as a reasoning-extraction attempt.** The
  mandatory pre-implementation and pre-QA blocks were specified in
  language that asked the model to surface its *internal reasoning*
  — "chain-of-thought ... walks through your reasoning step-by-step",
  "this is not internal `<thinking>`", "internal thinking is not
  enough". On Fable 5.1 that phrasing tripped an input safeguard and
  the request was refused before the skill could run.

  The blocks are now specified as what they always were in practice:
  authored planning artifacts written for the user, not a dump of
  internal deliberation. Renamed and reworded:
  - `**Chain-of-thought (T<n>):**` → `**Task brief (T<n>):**`
  - `**Super-qa chain-of-thought:**` → `**Super-qa test plan:**`
  - `**Super-qa chain-of-thought (section):**` → `**Super-qa test plan (section):**`
  - `**Super-qa briefing:**` — unchanged.

  **Behaviour is unchanged.** Every required bullet, the enforcement
  rules (rule 18, rule 9), the pre-flight checklist gates, the
  fast-path one-line collapse, and the "reject a verdict that arrives
  without its plan" rule all survive verbatim — only the framing and
  the block headings changed. Affects P4 step 0a, P4.5, and P5.

### Changed
- `README.md`: the "Chain-of-thought enforcement" feature row is now
  "Written-brief enforcement", with the block names above.

## [0.5.0] - 2026-09-18

Every file in a project is now indexed — not just text. Lens crate local
version 0.3.0 (`lens/VENDOR.txt`, local patch set 3).

### Added
- **Assets: images, video, audio, PDF, Office documents, archives.**
  Binary files used to be skipped; they now get a searchable record
  (schema v4, `assets` table + the shared FTS body) built from three
  sources: (1) what lens extracts on its own with no external tools —
  PNG/JPEG/GIF/WebP/BMP dimensions, MP4/MOV and WAV duration, PDF page
  count and text from FlateDecode content streams, Office
  (`.docx`/`.xlsx`/`.pptx`/`.odt`/`.ods`/`.odp`) text via a built-in zip
  reader; (2) `pdftotext` and `ffprobe` when they happen to be installed
  (`LENS_NO_EXTERNAL_TOOLS=1` disables); (3) a **stored description**.
  Large files are never read whole — metadata comes from a bounded
  header probe and identity is `blake3(prefix || size)`.
- **`lens describe <file> [--text "…" --by NAME] [--clear]`** and the
  `lens_describe` MCP tool. Show what lens knows about an asset, or
  store what a model learned by viewing it once. Descriptions survive
  re-indexing of the file and are part of the search body, so any model
  in any later session finds the file with `lens search` instead of
  re-reading the bytes: view once, describe once, search forever.
- `lens search --kind` accepts every kind: `code`, `text`, `image`,
  `video`, `audio`, `pdf`, `office`, `archive`, `binary`.
- pro-coder protocol: token-discipline rule 6 (media and documents —
  `lens describe` before opening, `lens describe --text` after viewing),
  two tooling-table rows, a checklist line; `skill_meta.sh` checks for
  `lens describe`.
- Tests: lens core 506 → 518, CLI 119 → 123 (+40 e2e); new `assets`
  unit tests cover every parser with synthetic files.

### Changed
- Oversized text files (> 512 KiB) are still skipped; oversized binaries
  are indexed by metadata only.
- lens-core gains the `flate2` crate (pure-Rust inflate for PDF streams
  and zip entries).

## [0.4.0] - 2026-09-18

Lens becomes an IDE-grade index and a cross-session memory for any MCP
client, and the pro-coder protocol moves to v7 with token discipline and a
one-shot build mode. The vendored lens crate is at local version 0.2.0
(`lens/VENDOR.txt`, local patch set 2).

### Added
- **`lens search <words>`** — keyword search over *every* text file in
  the project: any language (including ones without a symbol extractor),
  markdown, config, shell, and the agent's own notes under
  `.claude/state/` (always indexed, even when gitignored). Backed by a
  new SQLite FTS5 table (`docs` / `docs_fts`, schema v3), BM25-ranked per
  file, prefix-matched per word, budget-capped, and each hit is annotated
  with the enclosing symbol. This is the capped, ranked replacement for a
  tree-wide `Grep`, and what makes lens usable as a memory for a model.
- **`lens deps <file>`** — file-level connections: imports in both
  directions (resolved to project files where possible), call edges
  aggregated per counterpart file, and the file's most-called symbols.
  The cheapest way to size a blast radius without reading the file.
- **Code-aware token estimator** (`lens_core::tokens`). Budgets for
  `follow` and `slice` are now fitted by estimated tokens (identifier
  sub-words, operator merging, indentation, newlines) instead of a flat
  4-chars-per-token rule that under-counted dense code by 20–40%.
  Calibrated at ~3.2 chars/token across Rust, Python, TypeScript, shell
  and Markdown.
- **Tokens footer on every read verb** — `_tokens: ~N emitted • ~M saved
  vs reading K files whole_`. The meter (`lens meter`) now records
  `lens_calls`, `lens_emitted_tokens`, `lens_saved_tokens` automatically
  and prints the leverage ratio; `--json` carries the new keys.
  Opt out with `LENS_NO_METER=1`.
- **Import-aware call/reference resolution** (`storage::resolve_heuristics`).
  Cross-file calls written the way real code writes them — Python
  `from pkg.mod import f; f()` and `import pkg.mod as m; m.f()`
  (absolute and relative), Rust `use crate::a::b; b()` (crate roots
  detected in workspaces), TypeScript/JavaScript `import { f } from
  "./mod"` (extension and `index.*` resolution), Go package imports
  `store.Open()`, and `self.` / `this.` receivers — now link to their
  definitions. A bare name defined exactly once in the project also
  resolves; dotted names never fall through to that rule, so
  `list.append` cannot bind to an unrelated project `append`. On this
  repository the number of resolved call edges went from same-file-only
  to 2 833 of 10 319, which is what `lens refs`, `lens follow`'s caller
  list, `lens map`'s hot-spot ranking and `lens query`'s traversal see.
- **`lens map --budget N`** — depth is reduced automatically until the
  rendering fits, with a note saying how far it folded.
- **MCP server hardening** — every `tools/call` now runs the same
  throttled auto-freshness check as the CLI (v1 served stale results
  from a long-lived server), a missing index is built automatically on
  first use, the in-memory graph is cached per root and invalidated by
  an index stamp, every tool accepts a `root` argument so one server can
  serve several projects (`lens mcp --root` sets the default), `ping` is
  answered, and `lens_search` / `lens_deps` join the catalogue (nine
  tools). Works unchanged with Codex, Cursor, or any MCP client — see the
  README section *Using lens from other agents*.
- **Project-root discovery** — read verbs work from any sub-directory
  (walk up to `.lens/index.db`, else `.git`).
- **pro-coder protocol v7** (`pro-coder/SKILL.md`): a *Token discipline*
  block in P1 (default budgets, narrow before raising, no whole-file
  `Read` of a sliced file, `lens search` instead of tree-wide `Grep`,
  `lens meter --diff` reported at every section close in a new
  **Tokens** line of the user-facing summary); `lens search` / `lens
  deps` in the tooling table with a "recall prior-session notes" row;
  a new **One-shot build mode** section (objective contract with
  verifiable acceptance checks in `current-tasks.md`, all sections
  planned up front, continuous section boundaries re-anchored from
  disk, done only when the end-to-end verification task passes
  super-qa); hard rules 19–20, three checklist lines and drift anchor 10.
- **Tests:** lens core 452 → 506, lens CLI unit 91 → 118 plus 40
  end-to-end (666 total, all green); `skill_meta.sh` 27 → 36 checks
  including a regression guard that fails if `SKILL.md`, `README.md` or
  `scripts/install.sh` reintroduce fallback-mode language; CI gains a
  `cargo test --workspace` job.

### Changed
- **`lens index` is idempotent** — re-running replaces the index in one
  transaction instead of failing on the `files.path` UNIQUE constraint.
- **Freshness checks cost one `stat` per file** — unchanged files
  (matching size + mtime) are not re-read or re-hashed; drifted stamps
  are refreshed in place. On this repository a no-change `lens update`
  takes ~40 ms.
- `lens follow` / `lens refs` resolve symbols with indexed SQL lookups
  instead of loading the whole graph.
- `lens query`'s per-node token weight corrected from 80 to 24 (a
  rendered node line is ~24 tokens), so a 2000-token budget now returns
  the neighbourhood it promised.
- `lens init` default config lists `csharp`.
- README rewritten for the new surface; clone URLs point at
  `byte-mods/claude-pro-coder`; CHANGELOG link footers restored for
  0.2.4 → 0.4.0.

### Fixed
- README line describing an empty index still claimed the skill "falls
  back to Read/Grep/Glob at runtime" (retired in v6). Now guarded by a
  test.
- `examples/README.md` and `CONTRIBUTING.md` still described the v5
  fallback mode.

## [0.3.0] - 2026-05-17

### BREAKING
- **Lens is now required.** v6 of the pro-coder protocol removes the
  `Read`/`Grep`/`Glob` fallback mode entirely. The skill aborts at
  bootstrap with an install pointer if the `lens` binary is not on
  `$PATH`. Rationale: fallback was a strictly worse code-comprehension
  strategy and the agent would silently degrade to it on machines
  where the install had failed, producing protocol-compliant-looking
  output backed by stale or shallow context. Existing users on
  machines without lens must run `./scripts/install.sh` (or
  `./scripts/install-lens.sh` directly) to enable the skill after
  upgrading. The `--no-lens` install flag still exists as a build-time
  escape hatch but the resulting installation will refuse to run at
  first invocation.

### Added
- **Chain-of-thought is now mandatory and visible at every task and
  every super-qa spawn.** Three new visible blocks are required:
  (1) `**Chain-of-thought (T<n>):**` before any code is written in
  P4 — goal, files implicated, edge cases, failure modes, verification
  approach, out-of-scope; (2) `**Super-qa briefing (T<n>):**` before
  every P4.5 spawn — requirements, files in diff, tests, budgets,
  task-specific adversarial probes, gotchas; (3) `**Super-qa
  chain-of-thought:**` emitted by the subagent before its verdict —
  requirements as it reads them, what the diff does per requirement,
  divergence points, edge-case plan, what would change the verdict.
  Section-level super-qa (P5) gets parallel briefing + CoT blocks.
  Internal `<thinking>` is not sufficient; the user must see what
  the agent is about to do before it does it. Fast-path tasks collapse
  the pre-implementation CoT to a single `> fast-path: <reason>` line.

### Changed
- **`.history/` and `current-tasks.md` are re-verified at every P1**,
  not just bootstrap. They are load-bearing artifacts and can be
  deleted between sessions (fresh checkouts, `git clean`, accidental
  removal); P1 now creates them if missing and **aborts the loop**
  with an explicit error if creation fails (permission denied,
  read-only FS, disk full). The skill will not proceed on a
  half-bootstrapped tree. Existing projects already pass the
  re-verification cleanly; the change is defence in depth, not new
  policy.
- **Hard rules grew from 17 to 18**, and Drift anchors from 8 to 9,
  to encode the new illustration requirement.
- **README, install.sh, and the FAQ rewritten** to reflect lens-
  required policy. The "Auto-fallback" feature row has been replaced
  with "Lens-required" and "Chain-of-thought enforcement" rows.

## [0.2.6] - 2026-05-03

### Changed
- **`pro-coder/SKILL.md`: lens-first is now a precedence rule, not a
  preference.** Tightened the P1 tooling guidance to explicitly require
  that the *first* code-comprehension call in lens-mode projects go to
  `lens query` / `lens follow` / `lens refs` / `lens path`, not to
  `Grep` or `Read` on a code symbol. Read is reserved for full-file
  context (e.g. immediately before editing); Grep is reserved for
  literal strings (config keys, error messages, TODO markers). Closes
  the silent drift toward built-in tools that the protocol previously
  only nudged against.
- **Pre-response checklist: lens-first guard at the decision point.**
  Added an explicit P1 checklist line — *"If P1 and lens mode: was the
  first code-comprehension call a lens command? If the first reach was
  Grep or Read on a code symbol, you drifted — restart with lens."* —
  so the discipline gets verified before every response, not just
  described in prose.
- **Pre-response checklist: per-task `schema.txt` verification for
  database projects.** Added a new line that fires at task close:
  *"If P4 task complete and database project and this task
  added/removed/renamed/re-typed any table, column, index, or constraint:
  was `schema.txt` appended in the same task, before the `.history/`
  snapshot?"* Previously, schema.txt was only verified at section close
  (P5); per-task verification keeps the archived `.history/` snapshot
  consistent with the schema state it claims to capture.

## [0.2.5] - 2026-05-03

### Added
- **C# language support in lens.** `lens` now indexes `.cs` files via
  `tree-sitter-c-sharp`. Extracts: namespaces, classes, structs, interfaces,
  enums, delegates, methods, constructors, properties, fields, events;
  `using` imports; base-list `extends` relations; calls and type refs.
  Symbols are scoped under namespace + type parent with `::` separator.
- **`.gitignore` now covers `*.md`.** All Markdown files are ignored by
  default; tracked `.md` files can still be staged explicitly.

## [0.2.4] - 2026-05-01

### Fixed
- **Rust extractor: `impl Trait for ImportedType` no longer aborts the index.**
  The extractor synthesised the type-relation owner by prefixing the impl
  target with the current module path; when the target type was imported
  from another file (e.g. `impl SellerPlanStore for DataStoreSession`), the
  resulting owner qname pointed at a phantom symbol and the storage layer
  rejected the row with "extractor invariant violated", aborting the entire
  index. Type-relations whose owner is not declared in the same file are
  now dropped post-walk, since the storage schema requires
  `types.symbol_id NOT NULL` and the row is unrecoverable.
- **Rust extractor: methods in distinct trait impls of the same type get
  distinct qnames.** A file containing `impl InventoryStore for T { fn m() }`
  and `impl CategoriesStore for T { fn m() }` previously produced two
  symbols with qname `module::T::m`, hitting the
  `(file_id, qualified_name)` UNIQUE constraint and aborting the index.
  Methods inside `impl <Trait> for <Type>` are now scoped under
  `module::Type::Trait::method`. Inherent impls (`impl T { ... }`) keep
  the original `module::T::method` shape.
- These two fixes together let lens index large polyglot Rust+Dart+TS
  monorepos (e.g. pounze) without `*.rs` exclusions or other workarounds.

### Changed
- `lens/VENDOR.txt` now records `LOCAL_PATCHES=1` to flag that the
  vendored copy diverges from upstream `a29f523`. Re-vendor a fresh
  upstream SHA once these patches land.

## [0.2.3] - 2026-05-01

### Fixed
- CHANGELOG sections were in inverted order — `[0.2.0]` appeared above `[0.2.2]`
  and `[0.2.1]`. Reordered to newest-first per Keep a Changelog convention.
- Stale "Java" reference in the 0.2.0 entry's `### Changed` section — listed
  Java alongside Ruby and C++ as an unsupported language, but Java has been
  supported since 0.2.2.

## [0.2.2] - 2026-05-01

### Added
- **Java language support in lens.** Added `JavaExtractor` implementing the
  `LanguageExtractor` trait over `tree-sitter-java 0.21`. Extracts classes,
  interfaces, enums, annotations (with nested declarations), methods,
  constructors, fields, enum constants, imports (including static and wildcard),
  and type references. Doc comments (`/** */`) are harvested at index time
  and surfaced by `lens follow`. Call-site extraction walks method and
  constructor bodies for `method_invocation` and `new` expressions. 44 tests
  pass in all configurations.

## [0.2.1] - 2026-05-01

### Added
- **`/diagram` skill** — generates visual Mermaid architecture flowcharts of
  the current codebase. Uses lens (`lens map`, `lens query`, `lens follow`,
  `lens refs`, `lens path`) for symbol-aware structural analysis; falls back
  to filesystem tools when lens is unavailable. Writes `ARCHITECTURE.md` at
  the project root, viewable in GitHub, VS Code, or any Mermaid renderer.
- `install.sh` and `uninstall.sh` now handle multiple skills via a loop over
  a `skill_names` array. Adding a new skill requires only creating its
  directory with a `SKILL.md` and appending its name to the array — no
  script refactoring needed.

- **Dart language support in lens.** Added `DartExtractor` implementing the
  `LanguageExtractor` trait over `tree-sitter-dart`. Extracts top-level
  functions, classes, methods, getters, setters, constructors (including
  factory, const, and named constructors), mixins, enums with constants,
  extensions, extension types, type aliases (typedefs), imports/exports,
  and type references. Doc comments (`///`, `/** */`) are harvested at
  index time and surfaced by `lens follow`.
- **Dart call-site extraction.** The `DartExtractor` now walks function and
  constructor bodies for call expressions — bare calls (`greet()`), method
  chains (`obj.method()`, `a.b.c()`), `new` expressions (`new Point()`),
  and cascade notation (`obj..a()..b()`). Each call is attributed to its
  enclosing function or method qualified name, enabling `lens refs` impact
  analysis for Dart codebases.

### Changed
- **SKILL.md: mandatory code commenting after QA PASS.** P4 task flow now
  includes step 6 — comment every function, method, struct, module, and
  non-trivial logic block with why-comments after super-qa returns PASS.
  New hard-rule invariant #13 and a pre-response checklist item enforce
  this. Comments use the language's native doc format so `lens follow`
  surfaces them; well-commented code lets future AI sessions skip reading
  function bodies.
- **README Dart coverage.** Doc-comment extraction now lists Dart alongside
  Rust (`///` / `/** */`). FAQ answer updated to include Dart. `--version`
  verify step no longer hardcodes `0.1.0` — references `lens/Cargo.toml:9`.

## [0.2.0] - 2026-05-01

### Changed
- **BREAKING — skill renamed `super-coder` → `pro-coder`.** The slash command
  is now `/pro-coder`. The skill directory at `~/.claude/skills/super-coder/`
  is no longer installed to or read from; `install.sh` writes to
  `~/.claude/skills/pro-coder/` instead. Existing installations should
  remove the old directory manually:
  ```
  rm -rf ~/.claude/skills/super-coder
  bash scripts/install.sh    # installs the renamed pro-coder skill
  ```
  Defence-in-depth tail-checks (`*/super-coder` → `*/pro-coder`) and the
  staging-dir prefix (`.super-coder.staging.*` → `.pro-coder.staging.*`)
  also moved. The internal QA role `super-qa` is unchanged.

### Added
- `install.sh --dry-run` — print intended actions, make no filesystem changes.
  Skips the cargo build entirely under `--dry-run`; logs `would build lens`
  in its place. Pass-throughs `--dry-run` to `install-mcp.sh`.
- `install.sh --quiet` — suppress non-error stdout (mirrors `uninstall.sh`).
  Errors still go to stderr. Pass-throughs `--quiet` to `install-lens.sh`
  and `install-mcp.sh`.
- `--flag=VALUE` form for every value-taking flag across all four scripts:
  `install.sh`, `uninstall.sh`, `install-lens.sh`, `install-mcp.sh`.
  Both `--flag VALUE` and `--flag=VALUE` are now accepted; empty values
  (`--flag=`) are explicitly rejected.
- `--strict` flag on all four scripts — adds a positive allow-list on top
  of the existing unsafe-dest guard. With `--strict`, every operative
  path (`--dest`, `--bin-dir`, `--claude-json`, `--lens-bin`) must
  resolve to a location under `~/.claude/`. Layered, not replacing.
- `--allow-root` flag on all four scripts — required when running as
  root. Without it, every script refuses `EUID=0` with a clear error.
  Refusing root by default prevents the "root-owned files under \$HOME"
  footgun where the user's normal shell can't edit them later.
- `sc_assert_not_root` and `sc_assert_strict_allowed` helpers in
  `scripts/_lib.sh`. Both use dependency-injected inputs so unit tests
  can exercise both code paths without actually being root or relying
  on real `${HOME}`.
- User-facing summary format prescribed in `pro-coder/SKILL.md` —
  the three mandated summary surfaces (plan presentation, task close,
  section close) now use a clean *What changed / Why it matters /
  Tests / What's next* block. Internal artifacts (`.claude/state/`
  snapshot, code-map notes, super-qa verdicts) keep their structured
  technical format because they are read by future Claude sessions.
- `examples/` directory — five sample prompts (Rust feature, Python
  bug-fix, cross-language refactor, fast-path docs, section boundary)
  with annotations explaining what the skill does internally for each
  shape of work.
- `scripts/test/skill_meta.sh` — meta-tests for `pro-coder/SKILL.md`.
  27 sub-checks: frontmatter validity, required sections present, every
  P-reference resolves to a defined header, code-fence balance, no
  `<TODO>` / `<FIXME>` / `<TBD>` placeholder leaks. Wired into
  `round_trip.sh` as test 7 with one delegating assertion.
- `CHANGELOG.md` — this file.
- `CONTRIBUTING.md` — dev workflow, testing, code-style invariants.
- `scripts/test/round_trip.sh` grew from 49 → 73 assertions:
  test 6 (`test_install_extended_flags`, +12) covers `--dest=VALUE`,
  `--quiet`, `--dry-run`, and `--flag=` empty-value rejection;
  test 7 (`test_skill_meta`, +1, delegates to `skill_meta.sh`'s 27
  sub-checks) covers SKILL.md structural integrity; test 8
  (`test_strict_and_root_guards`, +11) covers the `--strict`
  allow-list and `sc_assert_not_root` end-to-end.

### Added (continued)
- `VERSION` file at repo root — single source of truth for the project
  version. Read by `sc_version()` in `scripts/_lib.sh`.
- `--version` flag on all four scripts (`install.sh`, `uninstall.sh`,
  `install-lens.sh`, `install-mcp.sh`). Echoes the project version from
  `VERSION` and exits 0.
- `CLAUDE.md` at repo root — machine-readable project contract covering
  shell-script invariants, vendored-code policy, security-sensitive code,
  and the release process.

### Changed
- README verify step 3 now sets clearer expectations for projects in
  unsupported languages (Ruby, C++, etc.) — `lens index` producing
  0 symbols is expected there.
- README `lens --version` verify step now anchors the expected version to
  `lens/Cargo.toml` workspace version, not a bare hardcoded string.

### Fixed
- Vendored `lens/README.md` contains a stale `super-coder` reference (the
  renamed skill). This is vendored upstream content pinned at
  `lens/VENDOR.txt` SHA `a29f523` and intentionally not edited in place;
  the known issue is documented in `CLAUDE.md`.

## [0.1.0] - 2026-04-29

Initial public release.

### Added

#### `pro-coder` skill (Brainiac-OS v5)
- 6-phase loop (Comprehend → Plan → Implement → Test → Audit → Section
  Boundary) with section-boundary context resets.
- Two-agent team loop: pro-coder (architect + implementer) plus a
  fresh-context super-qa adversarial reviewer spawned via the Agent
  tool. Per-task super-qa gate plus optional section-level super-qa
  before P6 closure.
- Bootstrap protocol: state directory creation, gitignore-policy ask-once
  marker, CLAUDE.md presence check, code-map directory check, lens index
  detection (lens-mode vs fallback-mode).
- Five non-negotiable invariants: schema-constrained output, FSM-enforced
  transitions, adapter boundaries, hot-path discipline, code-map freshness.
- Persistent state at `.claude/state/` covering section snapshots, the
  code-map, gitignore policy, and queued CLAUDE.md proposals (proposed
  but never auto-applied — the user reviews and merges manually).

#### `lens` Rust CLI (vendored at `lens/` — pinned at SHA `a29f523`)
- SQLite-backed symbol index over Rust, Python, TypeScript/TSX,
  JavaScript/JSX/MJS/CJS, and Go.
- Verbs: `init`, `index`, `update`, `query`, `follow`, `refs`, `slice`,
  `add`, `path`, `explain`, `map`, `meter`, `watch`, `mcp`. Plus a
  graphify-compat positional form (`lens .` / `lens . --update`).
- Token-budget caps on `query`, `follow`, `slice` (default 2000 tokens).
- Doc-comment-first surfacing: `lens follow` extracts the leading doc
  (`///`, docstring, JSDoc, `//`) at index time and prints it as a
  blockquote ahead of signature and body.
- Cross-language disambiguation: when a symbol resolves in multiple
  languages, candidates are tagged with their language. `--from FILE:LINE`
  resolves the ambiguity.
- Auto-freshness: every read-mode verb checks for file drift and runs
  an incremental update if anything changed since last index. Throttled
  via `.lens/freshness.txt` (default 5 s window). Disable with
  `LENS_NO_AUTO_UPDATE=1`; tune with `LENS_FRESHNESS_THROTTLE_SECONDS=N`.
- MCP server (`lens mcp`) — Claude Code spawns lens at startup and
  exposes `lens_follow`, `lens_refs`, `lens_query`, `lens_explain`,
  `lens_path`, `lens_slice`, `lens_map` as structured MCP tools.
- Persistent token meter across sessions / `/clear` (`lens meter`).

#### Install pipeline
- `scripts/install.sh` — orchestrator. Default flow: install skill →
  build lens → register MCP entry. `--copy` (default), `--symlink`
  (developer-mode hot-edits), `--force`, `--dest`, `--bin-dir`,
  `--claude-json`, `--no-lens`, `--no-mcp`.
- `scripts/install-lens.sh` — builds the vendored lens crate via cargo
  and installs the binary to `~/.claude/bin/lens`. Source-hash
  idempotency marker. Cargo-absent fall-through (`--skip-if-no-cargo`
  default; `--require-cargo` opts in to a hard failure).
- `scripts/install-mcp.sh` — Python-driven safe JSON surgery on
  `~/.claude.json`. Atomic `os.replace` write. Backs up to
  `~/.claude.json.bak.YYYYMMDD-HHMMSS` before every write. `--remove`,
  `--dry-run`, `--quiet`, `--lens-bin`, `--claude-json`.
- `scripts/uninstall.sh` — clean removal of skill, lens binary, and
  MCP entry. Reaps orphan staging dirs from interrupted prior installs.
  `--keep-lens`, `--keep-mcp`, `--dry-run`, `--quiet`.
- `scripts/_lib.sh` — shared helpers. `sc_set_default_home`,
  `sc_canonicalize_dest` (purely textual; no `realpath` dep; bash 3.2
  compatible), `sc_assert_safe_dest`. Single source of truth for the
  unsafe-dest case-list (covers macOS and Linux system paths).

#### Tests
- `scripts/test/round_trip.sh` — 49 integration tests covering
  canonicalisation, the unsafe-dest guard, orphan-staging reap,
  install-uninstall round-trip in copy and symlink modes, plus
  idempotency on every action.

#### Documentation
- Comprehensive `README.md` (~620 lines) — Quickstart, install steps,
  command reference for every script and every lens subcommand, the
  6-phase loop with tool-per-phase mapping, the MCP tool surface,
  state-persistence layers, troubleshooting, FAQ.

### Known limitations
- Languages outside lens's supported set (Rust, Python, TS/TSX/JS/JSX/
  MJS/CJS, Go) trigger fallback mode (Read/Grep/Glob). Shell, Markdown,
  YAML, Lua, C/C++, Ruby, Java are not indexed.
- The skill's bootstrap creates `.claude/state/` per project; the
  gitignore-policy ask-once is per-project (keyed by
  `.claude/state/gitignore_policy`).
- `--dry-run` on `install.sh` skips the cargo build entirely rather
  than running it in a no-write mode (cargo offers no such mode).

[Unreleased]: https://github.com/byte-mods/claude-pro-coder/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/byte-mods/claude-pro-coder/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/byte-mods/claude-pro-coder/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/byte-mods/claude-pro-coder/compare/v0.2.6...v0.3.0
[0.2.6]: https://github.com/byte-mods/claude-pro-coder/compare/v0.2.5...v0.2.6
[0.2.5]: https://github.com/byte-mods/claude-pro-coder/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/byte-mods/claude-pro-coder/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/byte-mods/claude-pro-coder/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/byte-mods/claude-pro-coder/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/byte-mods/claude-pro-coder/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/byte-mods/claude-pro-coder/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/byte-mods/claude-pro-coder/releases/tag/v0.1.0
