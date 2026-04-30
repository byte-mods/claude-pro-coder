# CLAUDE.md Proposal Queue

**STATUS — 2026-04-30 (end of section 7 of the lens project):** All 4
proposals below have been accepted and integrated into the lens repo's
`~/projects/lens/CLAUDE.md` as of section 7. Specifically:
- "Schema discipline" → `## Schema discipline` in lens CLAUDE.md
- "Public API discipline (lens-core)" → `## Public API discipline (lens-core)`
- "CLI contract" → `## CLI contract`
- "Extraction discipline + Qualified-name conventions" → split into
  `## Per-language extractors` and `## Qualified-name conventions`

The entries are retained below as a historical record. The lens repo
also has its own queue (`~/projects/lens/.claude/state/claude_md_proposals.md`)
which holds the section 2 part 2 / section 4 proposals that were
likewise integrated.

---

_Pending review by user. Accepted entries to be copied into `CLAUDE.md` by hand._

_Note: these proposals target a future `/Users/sudeepdasgupta/projects/lens/CLAUDE.md` (the lens repo's contract), not the claude-skill repo's CLAUDE.md. Lens does not yet have a CLAUDE.md; the user can create one when convenient._

---

## Proposal — 2026-04-28 — Section 1

**Suggested addition:**

```
## Schema discipline

- All SQLite tables MUST use STRICT mode (`... ) STRICT;`) — typed columns prevent silent type coercion bugs.
- Foreign keys with ON DELETE CASCADE wherever a parent row owns its children (files → symbols → refs/calls/imports/types). Pragma `foreign_keys = ON` is set per-connection in `Storage::configure()` (`crates/lens-core/src/storage/db.rs`).
- Migrations are FORWARD-ONLY and APPEND to the `MIGRATIONS` array in `crates/lens-core/src/storage/migrations.rs`. NEVER edit an existing migration's SQL in place — write a new migration string with the next version number.
- The `meta` table is a singleton (`CHECK (id = 0)`). Schema version is read from `meta.schema_version`, NOT from build constants.
```

**Section:** `## Schema discipline` (new top-level section)

**Justification:** observed across T3 + T4 — the schema design assumes STRICT-mode invariants (rejected non-zero meta.id) and FK CASCADE (verified by test_storage_fk_cascade_on_file_delete_removes_symbols at db.rs:132). A future contributor adding a v2 migration without these invariants would break Storage::open's idempotency contract or leak ghost rows. The constraint is load-bearing; codifying it prevents regression.

**Confidence:** high

---

## Proposal — 2026-04-28 — Section 1

**Suggested addition:**

```
## Public API discipline (lens-core)

- All user-facing types in `lens-core` MUST be reachable via `lens_core::{Type}` (re-exported from `lib.rs`). Internal modules like `storage::db` remain accessible but consumers should import from the crate root, not internal paths.
- Public surface as of v0.1.0: `LensError`, `Result`, `Storage`, `apply_migrations`, `current_schema_version`, `Migration`, `MIGRATIONS`, `version()`.
- Helper constructors on `LensError` (`io_at`, `other`, `invalid_path`) are the preferred constructor surface — direct struct construction is allowed but the helpers normalise `impl Into<...>` ergonomics.
```

**Section:** `## Public API discipline` (new top-level section)

**Justification:** observed across T2 + T4 — the helper constructors (`io_at`, `other`, `invalid_path`) only get exercised because lens-cli imports `LensError` from `lens_core::` (the re-export), not from `lens_core::error::`. If consumers start importing from internal module paths, refactoring `storage::db` becomes a breaking change. Codifying the re-export discipline keeps the public surface frozen at the crate root.

**Confidence:** high

---

## Proposal — 2026-04-28 — Section 1

**Suggested addition:**

```
## CLI contract

- The v1 CLI subcommand contract is FROZEN. New args are additive only — never rename, never remove, never change semantics.
- Subcommand-specific args belong on the subcommand variant (e.g. `Init { path, no_gitignore }`), NOT on the top-level `Cli` struct. The top-level `path: Option<PathBuf>` and `--update` exist solely for graphify-compat (`lens <path>` and `lens . --update`).
- Stub subcommands route through `cmd::stub::not_yet(name, section_num)` with the section number that will implement them. When a subcommand is implemented, replace the stub call site, NOT the routing in `main.rs::dispatch` (which is the single source of truth for the dispatch table).
- Test naming: `test_<component>_<scenario>_<expected>` — verified by clippy and by per-task super-qa.
```

**Section:** `## CLI contract` (new top-level section)

**Justification:** observed across T5 + T6 — the dispatch table in `main.rs:18-44` was the single edit point when T6 wired `lens init` from stub to real. Centralising routing decisions there (vs scattering parsing logic across `cmd/*.rs`) is what made T6 a one-line change to dispatch and zero changes to the existing 11-subcommand wiring. A future contributor splitting routing across files would void the locality benefit. Test naming convention is already in the super-coder SKILL.md but bears repeating in lens's own contract for grep-ability.

**Confidence:** medium (test-naming clause is the weakest — already covered by super-coder SKILL; the dispatch-table clause is high-confidence)

---

## Proposal — 2026-04-28 — Section 2 (part 1)

**Suggested addition:**

```
## Extraction discipline

- Every `LanguageExtractor` produces `ExtractedFile` records with the same shape regardless of language. Field shapes mirror the SQLite schema (`crates/lens-core/src/extract/types.rs`) — they are the contract between extractors and the storage layer. Schema-changing modifications require a new SQLite migration AND a coordinated update to the record types.
- Cross-file resolution (`refs.symbol_id`, `calls.callee_symbol_id`) is **deferred to the resolver in Section 3**. Extractors emit raw names + per-file qualified names only.
- Parent linkage is emitted as `parent_qualified_name: Option<String>`. Resolution to `parent_symbol_id` happens at insert time (Section 4 storage layer). **Never resolve in the extractor.**
- Tree-sitter parsers are not `Sync`. Use the per-thread, per-language pool in `crates/lens-core/src/parse/mod.rs` (`thread_local!`). NEVER share a `tree_sitter::Parser` across threads.
- Tree-sitter grammar versions are pinned in `Cargo.toml` (currently `tree-sitter-rust = "0.21"`, `tree-sitter-python = "0.21"`). AST node names drift between grammar minor versions; bump deliberately and re-test extractors.

## Qualified-name conventions

- Rust: `<module_path>::<scope>::<name>` using `::` separators. Inside `impl Foo`, methods become `<module_path>::Foo::<method>`. Generics on the implementing type are stripped (`impl<T> Wrap<T>` → `Wrap`).
- Python: `<module_path>.<scope>.<name>` using `.` separators (Section 2 part 2).
- The pipeline computes `module_path` from the file's project-relative path; extractors take it as input via `ExtractContext` and never derive it themselves.
- Empty `module_path` (e.g. project root files) is supported: qname falls back to the bare `name`.
```

**Section:** `## Extraction discipline` and `## Qualified-name conventions` (two new top-level sections).

**Justification:** Observed across T1+T3+T4+T5 — the `extract::types` shapes are now consumed in 6 places (RustExtractor, default trait impl, the section-level QA review, future Python extractor, future pipeline, future storage insert). The cross-file resolution boundary is non-obvious from code alone (the schema's nullable FK columns hint at it but don't enforce the workflow). The thread_local parser-pool pattern was load-bearing for T3+T7 and is invisible to anyone scanning `lang/rust.rs`. Qualified-name conventions are observable at runtime and consumers will rely on them; codifying them prevents accidental format drift between languages.

**Confidence:** high

---
