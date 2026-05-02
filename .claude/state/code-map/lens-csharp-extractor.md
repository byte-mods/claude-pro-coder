# code-map: lens-csharp-extractor

**Scope:** `lens/crates/lens-core/src/lang/csharp.rs`, `lens/crates/lens-core/src/lang/mod.rs`, `lens/crates/lens-core/src/lang/registry.rs`
**Last verified:** 2026-05-03 — section 1

## Purpose
C# language support for lens. Extracts symbols (namespaces, classes, structs, interfaces, enums, delegates, methods, constructors, properties, fields, events), imports (`using`), type relations (base list), calls (invocation + object creation), and type refs from `.cs` files via tree-sitter-c-sharp.

## Public API
- `CSharpExtractor::new()` (`csharp.rs:27`) — const constructor, zero-sized type.
- `CSharpExtractor.extract()` (`csharp.rs:33`) — walks AST, returns `ExtractedFile`.

## Invariants
- Module path derives from `namespace_declaration` qualified name; falls back to `ctx.module_path`.
- Qualified names use `::` separator. Nested types parent their children: `Ns::Outer::Inner::Method`.
- `build_qname` does NOT double-prefix module_path when parent is present — parent already carries the full prefix.

## Concurrency model
- Extractor is `Send + Sync`, stateless. Safe for rayon pipeline.

## Error idioms
- Never panics on malformed AST. `child_by_kind` returns `Option`; missing children are silently skipped.

## Callers / callees
- `Registry::with_default_languages()` → registers `CSharpExtractor` for `.cs` (`registry.rs:48`)
- `discover()` → resolves `.cs` → `LanguageId::CSharp` via registry
- `extract::pipeline` → calls `CSharpExtractor.extract()` for discovered `.cs` files

## Gotchas
- `tree-sitter-c-sharp` 0.21.x uses the old `language()` fn API (returns `tree_sitter::Language` directly). 0.23.x switched to `tree-sitter-language` crate which is incompatible with `tree-sitter = "0.22"`. We pin 0.21.
- `invocation_expression` in tree-sitter-c-sharp does NOT expose a `function` field name — callee is the first child. Fallback to first-child in `walk_body`.
- Constructor name equals class name; `find()` by name in tests can collide with the class symbol. Tests search by `kind == "constructor"` instead.
- `signature_text` truncates at `block`, `arrow_expression_clause`, `accessor_list`, or `;` — captures the declaration signature only.

## Open questions
- Property accessors (`get`/`set`) are not extracted as individual symbols — v1 treats the property as a single symbol.
- Indexers are not extracted.
- `using static` and `using alias = Type` are best-effort; alias resolution is raw text only.
