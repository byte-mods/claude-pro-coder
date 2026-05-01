//! Rust language extractor. Walks a tree-sitter Rust AST and emits structured
//! [`ExtractedSymbol`] records, plus refs, calls, imports, and type-relations.

use std::collections::HashSet;

use tree_sitter::Node;

use crate::extract::{
    ExtractContext, ExtractedCall, ExtractedFile, ExtractedImport, ExtractedRef, ExtractedSymbol,
    ExtractedTypeRel,
};
use crate::lang::{LanguageExtractor, LanguageId};
use crate::parse::ParsedFile;

pub struct RustExtractor;

impl LanguageExtractor for RustExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::Rust
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_rust::language()
    }

    fn extract(&self, parsed: &ParsedFile, ctx: &ExtractContext) -> ExtractedFile {
        let mut out = ExtractedFile::empty(ctx.relative_path, LanguageId::Rust);
        let scope = Scope::file_root();
        walk(parsed.root_node(), &scope, ctx, parsed.source(), &mut out);
        // `emit_impl` synthesises the type-relation owner by prefixing the impl
        // target with the current module path, but the target may be an
        // imported or otherwise foreign type whose canonical qname lives in
        // another file. The storage layer requires every type-relation owner
        // to resolve to a same-file symbol (schema NOT NULL FK), so drop any
        // relation whose owner wasn't actually declared in this file.
        let local_qnames: HashSet<&str> =
            out.symbols.iter().map(|s| s.qualified_name.as_str()).collect();
        out.type_relations
            .retain(|t| local_qnames.contains(t.symbol_qualified_name.as_str()));
        out
    }

    /// Rust convention: drop the `.rs` extension and replace `/` with `::`,
    /// **collapsing** `lib.rs` / `main.rs` / `mod.rs` to their parent directory
    /// so that symbols inside `src/foo/mod.rs` get qname `src::foo::Sym`, not
    /// `src::foo::mod::Sym`.
    fn module_path_from_relative_path(&self, relative_path: &str) -> String {
        let (parent, last) = match relative_path.rfind('/') {
            Some(i) => (&relative_path[..i], &relative_path[i + 1..]),
            None => ("", relative_path),
        };
        let parent_joined = parent.replace('/', "::");
        match last {
            "lib.rs" | "main.rs" | "mod.rs" => parent_joined,
            _ => {
                let stem = match last.rfind('.') {
                    Some(i) => &last[..i],
                    None => last,
                };
                if parent_joined.is_empty() {
                    stem.to_string()
                } else if stem.is_empty() {
                    parent_joined
                } else {
                    format!("{parent_joined}::{stem}")
                }
            }
        }
    }
}

#[derive(Clone)]
struct Scope {
    /// Qualified name of the enclosing symbol, or `None` at the file root.
    parent_qname: Option<String>,
    kind: ScopeKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    File,
    Module,
    Impl,
    Trait,
}

impl Scope {
    fn file_root() -> Self {
        Self {
            parent_qname: None,
            kind: ScopeKind::File,
        }
    }
}

fn walk(node: Node, scope: &Scope, ctx: &ExtractContext, source: &[u8], out: &mut ExtractedFile) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_item" => emit_function(child, classify_function(scope), scope, ctx, source, out),
            "function_signature_item" => {
                emit_function(child, "trait_method", scope, ctx, source, out)
            }
            "struct_item" => emit_struct(child, scope, ctx, source, out),
            "enum_item" => emit_named(child, "enum", scope, ctx, source, out),
            "trait_item" => emit_trait(child, scope, ctx, source, out),
            "impl_item" => emit_impl(child, scope, ctx, source, out),
            "mod_item" => emit_mod(child, scope, ctx, source, out),
            "const_item" => emit_const_or_static(child, "const", scope, ctx, source, out),
            "static_item" => emit_const_or_static(child, "static", scope, ctx, source, out),
            "type_item" => emit_type_alias(child, scope, ctx, source, out),
            "associated_type" => emit_named(child, "associated_type", scope, ctx, source, out),
            "use_declaration" => extract_use_declaration(child, source, out),
            _ => {}
        }
    }
}

fn classify_function(scope: &Scope) -> &'static str {
    match scope.kind {
        ScopeKind::Impl => "method",
        _ => "function",
    }
}

fn emit_function(
    node: Node,
    kind: &'static str,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, source);
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    out.symbols.push(make_symbol(
        qname.clone(),
        name,
        kind,
        node,
        scope,
        extract_function_signature(&node, source),
        extract_visibility(&node, source),
        extract_doc_comment(&node, source),
    ));
    // Walk the entire function_item (signature + body) for refs and calls.
    // Refs in return types and parameter types live in the signature, so
    // restricting the walk to the body would miss them.
    walk_within_function(node, &qname, source, out);
}

fn emit_named(
    node: Node,
    kind: &'static str,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, source);
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    out.symbols.push(make_symbol(
        qname,
        name,
        kind,
        node,
        scope,
        extract_decl_signature(&node, source),
        extract_visibility(&node, source),
        extract_doc_comment(&node, source),
    ));
}

fn emit_struct(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, source);
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    out.symbols.push(make_symbol(
        qname.clone(),
        name,
        "struct",
        node,
        scope,
        extract_decl_signature(&node, source),
        extract_visibility(&node, source),
        extract_doc_comment(&node, source),
    ));
    if let Some(body) = node.child_by_field_name("body") {
        emit_field_type_relations(body, &qname, source, out);
    }
}

fn emit_field_type_relations(
    body: Node,
    struct_qname: &str,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    match body.kind() {
        // Regular `struct X { name: T, ... }` — each child is a
        // `field_declaration` with its own `type` field.
        "field_declaration_list" => {
            let mut cursor = body.walk();
            for child in body.named_children(&mut cursor) {
                if child.kind() == "field_declaration" {
                    if let Some(ty) = child.child_by_field_name("type") {
                        out.type_relations.push(ExtractedTypeRel {
                            symbol_qualified_name: struct_qname.to_string(),
                            relation: "field_type".to_string(),
                            target_raw_name: node_text(ty, source).trim().to_string(),
                            line: (child.start_position().row + 1) as u32,
                        });
                    }
                }
            }
        }
        // Tuple struct `struct X(T, U, ...)` — `type` is a multi-valued field
        // directly on the body node.
        "ordered_field_declaration_list" => {
            let mut cursor = body.walk();
            for ty in body.children_by_field_name("type", &mut cursor) {
                out.type_relations.push(ExtractedTypeRel {
                    symbol_qualified_name: struct_qname.to_string(),
                    relation: "field_type".to_string(),
                    target_raw_name: node_text(ty, source).trim().to_string(),
                    line: (ty.start_position().row + 1) as u32,
                });
            }
        }
        _ => {}
    }
}

fn emit_trait(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, source);
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    out.symbols.push(make_symbol(
        qname.clone(),
        name,
        "trait",
        node,
        scope,
        extract_decl_signature(&node, source),
        extract_visibility(&node, source),
        extract_doc_comment(&node, source),
    ));
    if let Some(body) = node.child_by_field_name("body") {
        let inner = Scope {
            parent_qname: Some(qname),
            kind: ScopeKind::Trait,
        };
        walk(body, &inner, ctx, source, out);
    }
}

fn emit_impl(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // Attribute methods inside `impl Foo` to the type Foo. The impl block
    // itself is not a symbol; for `impl Trait for Foo` we additionally emit
    // an "implements" type-relation row.
    let type_node = match node.child_by_field_name("type") {
        Some(n) => n,
        None => return,
    };
    let type_name = strip_generics(&node_text(type_node, source));
    let type_qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &type_name);

    // For `impl Trait for Foo`, methods are scoped to `Foo::Trait::*` rather
    // than `Foo::*` so that two traits providing methods of the same name
    // produce distinct qnames within the same file (Rust disambiguates them
    // via `<Foo as Trait>::method`, but a single qname column cannot). The
    // trait part is the trait's last path segment with generics stripped —
    // unique within a file in practice. Inherent impls (`impl Foo { ... }`)
    // keep the original `Foo::*` shape.
    let mut method_parent_qname = type_qname.clone();
    if let Some(trait_node) = node.child_by_field_name("trait") {
        let target_raw = strip_generics(&node_text(trait_node, source));
        out.type_relations.push(ExtractedTypeRel {
            symbol_qualified_name: type_qname.clone(),
            relation: "implements".to_string(),
            target_raw_name: target_raw.clone(),
            line: (node.start_position().row + 1) as u32,
        });
        let trait_short = target_raw
            .rsplit("::")
            .next()
            .unwrap_or(&target_raw)
            .to_string();
        if !trait_short.is_empty() {
            method_parent_qname.push_str("::");
            method_parent_qname.push_str(&trait_short);
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        let inner = Scope {
            parent_qname: Some(method_parent_qname),
            kind: ScopeKind::Impl,
        };
        walk(body, &inner, ctx, source, out);
    }
}

fn emit_mod(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, source);
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    out.symbols.push(make_symbol(
        qname.clone(),
        name,
        "module",
        node,
        scope,
        extract_decl_signature(&node, source),
        extract_visibility(&node, source),
        extract_doc_comment(&node, source),
    ));
    // Inline modules have a body; extern (`mod foo;`) declarations do not.
    if let Some(body) = node.child_by_field_name("body") {
        let inner = Scope {
            parent_qname: Some(qname),
            kind: ScopeKind::Module,
        };
        walk(body, &inner, ctx, source, out);
    }
}

fn emit_const_or_static(
    node: Node,
    kind: &'static str,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, source);
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    let signature = node_text(node, source);
    let signature = signature.trim().trim_end_matches(';').trim().to_string();
    out.symbols.push(make_symbol(
        qname,
        name,
        kind,
        node,
        scope,
        if signature.is_empty() { None } else { Some(signature) },
        extract_visibility(&node, source),
        extract_doc_comment(&node, source),
    ));
}

fn emit_type_alias(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, source);
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    let kind = match scope.kind {
        ScopeKind::Trait => "associated_type",
        _ => "type",
    };
    out.symbols.push(make_symbol(
        qname,
        name,
        kind,
        node,
        scope,
        extract_decl_signature(&node, source),
        extract_visibility(&node, source),
        extract_doc_comment(&node, source),
    ));
}

fn make_symbol(
    qualified_name: String,
    name: String,
    kind: &'static str,
    node: Node,
    scope: &Scope,
    signature: Option<String>,
    visibility: Option<String>,
    doc_comment: Option<String>,
) -> ExtractedSymbol {
    let (sl, sc, el, ec) = position_of(&node);
    ExtractedSymbol {
        qualified_name,
        name,
        kind: kind.to_string(),
        start_line: sl,
        start_col: sc,
        end_line: el,
        end_col: ec,
        body_start_byte: node.start_byte() as u32,
        body_end_byte: node.end_byte() as u32,
        signature,
        visibility,
        parent_qualified_name: scope.parent_qname.clone(),
        doc_comment,
    }
}

fn build_qname(module_path: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}::{name}"),
        None if module_path.is_empty() => name.to_string(),
        None => format!("{module_path}::{name}"),
    }
}

/// Walk the prev_sibling chain of a declaration node, harvesting contiguous
/// Rust doc comments. Returns `None` when no doc was attached.
///
/// What counts as a doc:
///   - Outer line doc: `/// some text`
///   - Inner line doc: `//! some text` (less common at item position)
///   - Outer block doc: `/** some text */`
///   - Inner block doc: `/*! some text */`
///
/// Plain `//` line comments and plain `/* */` block comments do NOT count —
/// those are author scratchpad, not API documentation. Lines are joined with
/// `\n`; marker prefixes are stripped; per-line whitespace is trimmed.
fn extract_doc_comment(node: &Node, source: &[u8]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cursor = node.prev_sibling();
    while let Some(sib) = cursor {
        match sib.kind() {
            "line_comment" => {
                let txt = node_text(sib, source);
                let stripped = txt
                    .strip_prefix("///")
                    .or_else(|| txt.strip_prefix("//!"));
                match stripped {
                    Some(s) => lines.push(s.trim().to_string()),
                    None => break,
                }
            }
            "block_comment" => {
                let txt = node_text(sib, source);
                if txt.starts_with("/**") || txt.starts_with("/*!") {
                    let inner = txt
                        .trim_start_matches("/**")
                        .trim_start_matches("/*!")
                        .trim_end_matches("*/")
                        .trim()
                        .to_string();
                    lines.push(inner);
                } else {
                    break;
                }
            }
            _ => break,
        }
        cursor = sib.prev_sibling();
    }
    if lines.is_empty() {
        None
    } else {
        // We walked backwards (newest-first); reverse to source order.
        lines.reverse();
        Some(lines.join("\n"))
    }
}

/// Decode a node's byte range as UTF-8. **Non-UTF-8 fallback (v1):** if the
/// bytes are not valid UTF-8, returns an empty string. This means a
/// non-UTF-8 source file produces degenerate qnames (empty name segments)
/// rather than a panic. v1 assumes source is UTF-8; a future version may
/// reject non-UTF-8 sources up front in [`crate::parse::parse`].
fn node_text(node: Node, source: &[u8]) -> String {
    std::str::from_utf8(&source[node.byte_range()])
        .unwrap_or("")
        .to_string()
}

fn position_of(node: &Node) -> (u32, u32, u32, u32) {
    let s = node.start_position();
    let e = node.end_position();
    (
        (s.row + 1) as u32,
        s.column as u32,
        (e.row + 1) as u32,
        e.column as u32,
    )
}

fn extract_visibility(node: &Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            return Some(node_text(child, source));
        }
    }
    None
}

/// Signature for a function-like item: source from the item start to its
/// body's start (or to a trailing `;` for trait method declarations).
fn extract_function_signature(node: &Node, source: &[u8]) -> Option<String> {
    let end = match node.child_by_field_name("body") {
        Some(body) => body.start_byte(),
        None => node.end_byte(),
    };
    let s = std::str::from_utf8(&source[node.start_byte()..end]).ok()?;
    let s = s.trim().trim_end_matches(';').trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Signature for a declaration with a body (struct, enum, trait, mod, type
/// alias). Source from the item start to its body's start, or the full node
/// text if no body field exists.
fn extract_decl_signature(node: &Node, source: &[u8]) -> Option<String> {
    let end = match node.child_by_field_name("body") {
        Some(body) => body.start_byte(),
        None => node.end_byte(),
    };
    let s = std::str::from_utf8(&source[node.start_byte()..end]).ok()?;
    let s = s.trim().trim_end_matches(';').trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Strip generic parameters (`Foo<T, U>` → `Foo`) and any leading whitespace.
fn strip_generics(type_text: &str) -> String {
    match type_text.find('<') {
        Some(i) => type_text[..i].trim().to_string(),
        None => type_text.trim().to_string(),
    }
}

/// Walk a function body, attributing every call_expression to `caller_qname`
/// and emitting a ref for every type_identifier encountered.
fn walk_within_function(
    node: Node,
    caller_qname: &str,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    match node.kind() {
        "call_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                let pos = node.start_position();
                out.calls.push(ExtractedCall {
                    caller_qualified_name: caller_qname.to_string(),
                    callee_raw_name: node_text(func, source),
                    line: (pos.row + 1) as u32,
                    col: pos.column as u32,
                });
            }
        }
        "type_identifier" => {
            let (sl, sc, el, ec) = position_of(&node);
            out.refs.push(ExtractedRef {
                raw_name: node_text(node, source),
                kind: "type".to_string(),
                line: sl,
                col: sc,
                end_line: el,
                end_col: ec,
            });
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_within_function(child, caller_qname, source, out);
    }
}

/// Decompose a `use_declaration` node into one or more
/// [`ExtractedImport`] rows. Handles simple imports, `use as` aliases, group
/// imports (`use foo::{a, b}`), and wildcards (`use foo::*`).
fn extract_use_declaration(node: Node, source: &[u8], out: &mut ExtractedFile) {
    let line = (node.start_position().row + 1) as u32;
    let arg = match node.child_by_field_name("argument") {
        Some(a) => a,
        None => return,
    };
    walk_use_clause(arg, source, line, "", out);
}

fn walk_use_clause(
    node: Node,
    source: &[u8],
    line: u32,
    prefix: &str,
    out: &mut ExtractedFile,
) {
    let prepend = |path: &str| -> String {
        if prefix.is_empty() {
            path.to_string()
        } else {
            format!("{prefix}::{path}")
        }
    };
    match node.kind() {
        "use_as_clause" => {
            let path = node
                .child_by_field_name("path")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            let alias = node
                .child_by_field_name("alias")
                .map(|n| node_text(n, source));
            out.imports.push(ExtractedImport {
                raw_path: prepend(&path),
                alias,
                line,
            });
        }
        "scoped_use_list" => {
            let path = node
                .child_by_field_name("path")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            let new_prefix = prepend(&path);
            if let Some(list) = node.child_by_field_name("list") {
                let mut cursor = list.walk();
                for child in list.named_children(&mut cursor) {
                    walk_use_clause(child, source, line, &new_prefix, out);
                }
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_use_clause(child, source, line, prefix, out);
            }
        }
        "use_wildcard" => {
            let text = node_text(node, source);
            out.imports.push(ExtractedImport {
                raw_path: prepend(&text),
                alias: None,
                line,
            });
        }
        // Simple paths reach the leaf as one of these node kinds.
        _ => {
            let text = node_text(node, source);
            if text.is_empty() {
                return;
            }
            out.imports.push(ExtractedImport {
                raw_path: prepend(&text),
                alias: None,
                line,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_rust(src: &str, module_path: &str) -> ExtractedFile {
        let parsed = crate::parse::parse(src.as_bytes().to_vec(), &RustExtractor)
            .expect("parse rust source");
        let ctx = ExtractContext {
            relative_path: "src/test.rs",
            module_path,
        };
        RustExtractor.extract(&parsed, &ctx)
    }

    fn names(out: &ExtractedFile) -> Vec<&str> {
        out.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn qnames(out: &ExtractedFile) -> Vec<&str> {
        out.symbols
            .iter()
            .map(|s| s.qualified_name.as_str())
            .collect()
    }

    fn find<'a>(out: &'a ExtractedFile, name: &str) -> &'a ExtractedSymbol {
        out.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol named {name}"))
    }

    #[test]
    fn test_rust_extracts_top_level_function_with_signature() {
        let src = "fn add(x: i32, y: i32) -> i32 { x + y }";
        let out = extract_rust(src, "src::math");
        assert_eq!(names(&out), vec!["add"]);
        let s = find(&out, "add");
        assert_eq!(s.kind, "function");
        assert_eq!(s.qualified_name, "src::math::add");
        assert_eq!(s.parent_qualified_name, None);
        assert_eq!(s.signature.as_deref(), Some("fn add(x: i32, y: i32) -> i32"));
        assert_eq!(s.start_line, 1);
        assert!(s.body_end_byte > s.body_start_byte);
    }

    #[test]
    fn test_rust_extracts_struct_definition() {
        let src = "pub struct Point { x: i32, y: i32 }";
        let out = extract_rust(src, "src::geom");
        let s = find(&out, "Point");
        assert_eq!(s.kind, "struct");
        assert_eq!(s.qualified_name, "src::geom::Point");
        assert_eq!(s.visibility.as_deref(), Some("pub"));
        assert_eq!(s.parent_qualified_name, None);
    }

    #[test]
    fn test_rust_extracts_enum_definition() {
        let src = "pub enum Direction { North, South, East, West }";
        let out = extract_rust(src, "src::geom");
        let s = find(&out, "Direction");
        assert_eq!(s.kind, "enum");
        assert_eq!(s.qualified_name, "src::geom::Direction");
        assert_eq!(s.visibility.as_deref(), Some("pub"));
        // Variants are intentionally NOT separate symbols in v1.
        assert_eq!(out.symbols.len(), 1);
    }

    #[test]
    fn test_rust_extracts_impl_block_methods_qualified_to_type() {
        let src = "\
struct Counter { n: u32 }
impl Counter {
    pub fn new() -> Self { Self { n: 0 } }
    pub fn inc(&mut self) { self.n += 1; }
}";
        let out = extract_rust(src, "src::ctr");
        let qns: Vec<&str> = qnames(&out);
        assert!(qns.contains(&"src::ctr::Counter"));
        assert!(qns.contains(&"src::ctr::Counter::new"));
        assert!(qns.contains(&"src::ctr::Counter::inc"));
        let new_method = find(&out, "new");
        assert_eq!(new_method.kind, "method");
        assert_eq!(
            new_method.parent_qualified_name.as_deref(),
            Some("src::ctr::Counter")
        );
    }

    #[test]
    fn test_rust_extracts_trait_method_declarations() {
        let src = "\
pub trait Greet {
    fn hello(&self) -> String;
    fn farewell(&self) { println!(\"bye\"); }
}";
        let out = extract_rust(src, "src::greet");
        let qns: Vec<&str> = qnames(&out);
        assert!(qns.contains(&"src::greet::Greet"));
        assert!(qns.contains(&"src::greet::Greet::hello"));
        assert!(qns.contains(&"src::greet::Greet::farewell"));
        let hello = find(&out, "hello");
        assert_eq!(hello.kind, "trait_method");
        assert_eq!(
            hello.signature.as_deref(),
            Some("fn hello(&self) -> String")
        );
    }

    #[test]
    fn test_rust_extracts_nested_inline_modules() {
        let src = "\
pub mod outer {
    pub fn outer_fn() {}
    pub mod inner {
        pub fn inner_fn() {}
    }
}";
        let out = extract_rust(src, "src::nested");
        let qns: Vec<&str> = qnames(&out);
        assert!(qns.contains(&"src::nested::outer"));
        assert!(qns.contains(&"src::nested::outer::outer_fn"));
        assert!(qns.contains(&"src::nested::outer::inner"));
        assert!(qns.contains(&"src::nested::outer::inner::inner_fn"));
        let inner_fn = find(&out, "inner_fn");
        assert_eq!(
            inner_fn.parent_qualified_name.as_deref(),
            Some("src::nested::outer::inner")
        );
    }

    #[test]
    fn test_rust_extracts_visibility_pub_pub_crate_and_default() {
        let src = "\
pub fn p() {}
pub(crate) fn pc() {}
fn private() {}
";
        let out = extract_rust(src, "src::vis");
        assert_eq!(find(&out, "p").visibility.as_deref(), Some("pub"));
        assert_eq!(
            find(&out, "pc").visibility.as_deref(),
            Some("pub(crate)")
        );
        assert_eq!(find(&out, "private").visibility, None);
    }

    #[test]
    fn test_rust_qualified_name_uses_module_path_at_file_root() {
        let src = "fn alone() {}";
        let out_with_path = extract_rust(src, "deeply::nested::path");
        assert_eq!(
            find(&out_with_path, "alone").qualified_name,
            "deeply::nested::path::alone"
        );
        let out_no_path = extract_rust(src, "");
        assert_eq!(find(&out_no_path, "alone").qualified_name, "alone");
    }

    #[test]
    fn test_rust_handles_generic_function_signatures() {
        let src = "\
pub fn map<T, U, F: Fn(T) -> U>(xs: Vec<T>, f: F) -> Vec<U> { xs.into_iter().map(f).collect() }
";
        let out = extract_rust(src, "src::g");
        let s = find(&out, "map");
        let sig = s.signature.as_deref().unwrap_or("");
        assert!(sig.contains("fn map<T, U, F: Fn(T) -> U>"), "sig was: {sig}");
        assert!(sig.contains("Vec<U>"), "return type missing in sig: {sig}");
    }

    #[test]
    fn test_rust_extracts_const_and_static() {
        let src = "\
pub const MAX: u32 = 42;
static NAME: &str = \"lens\";
";
        let out = extract_rust(src, "src::k");
        let m = find(&out, "MAX");
        assert_eq!(m.kind, "const");
        assert_eq!(m.qualified_name, "src::k::MAX");
        assert_eq!(m.visibility.as_deref(), Some("pub"));
        let n = find(&out, "NAME");
        assert_eq!(n.kind, "static");
    }

    #[test]
    fn test_rust_extracts_type_alias_top_level() {
        let src = "pub type Bytes = Vec<u8>;";
        let out = extract_rust(src, "src::t");
        let s = find(&out, "Bytes");
        assert_eq!(s.kind, "type");
        assert_eq!(s.qualified_name, "src::t::Bytes");
    }

    #[test]
    fn test_rust_extracts_associated_type_in_trait() {
        let src = "\
pub trait Iter {
    type Item;
}";
        let out = extract_rust(src, "src::t");
        let item = find(&out, "Item");
        assert_eq!(item.kind, "associated_type");
        assert_eq!(
            item.parent_qualified_name.as_deref(),
            Some("src::t::Iter")
        );
    }

    #[test]
    fn test_rust_methods_have_parent_qname_set_to_type() {
        let src = "\
struct A;
impl A { fn a_method() {} }
";
        let out = extract_rust(src, "src::p");
        let m = find(&out, "a_method");
        assert_eq!(m.parent_qualified_name.as_deref(), Some("src::p::A"));
    }

    #[test]
    fn test_rust_top_level_symbols_have_no_parent_qname() {
        let src = "fn solo() {}";
        let out = extract_rust(src, "src::p");
        let s = find(&out, "solo");
        assert_eq!(s.parent_qualified_name, None);
    }

    #[test]
    fn test_rust_extracts_methods_under_generic_impl_strips_generics_in_qname() {
        let src = "\
struct Wrap<T>(T);
impl<T: Clone> Wrap<T> {
    pub fn dup(&self) -> Self { Self(self.0.clone()) }
}
";
        let out = extract_rust(src, "src::w");
        let m = find(&out, "dup");
        assert_eq!(m.kind, "method");
        assert_eq!(m.parent_qualified_name.as_deref(), Some("src::w::Wrap"));
        assert_eq!(m.qualified_name, "src::w::Wrap::dup");
    }

    #[test]
    fn test_rust_extern_mod_declaration_emits_module_symbol_without_recursion() {
        let src = "pub mod external;\nfn after() {}\n";
        let out = extract_rust(src, "src::e");
        let qns: Vec<&str> = qnames(&out);
        assert!(qns.contains(&"src::e::external"));
        assert!(qns.contains(&"src::e::after"));
        let m = find(&out, "external");
        assert_eq!(m.kind, "module");
    }

    #[test]
    fn test_rust_extracts_no_symbols_from_empty_source() {
        let out = extract_rust("", "src::empty");
        assert!(out.symbols.is_empty());
    }

    #[test]
    fn test_rust_extracts_tuple_and_unit_structs() {
        let src = "pub struct Tup(u32, String);\npub struct Unit;\n";
        let out = extract_rust(src, "src::s");
        let qns: Vec<&str> = qnames(&out);
        assert!(qns.contains(&"src::s::Tup"));
        assert!(qns.contains(&"src::s::Unit"));
        let tup = find(&out, "Tup");
        assert_eq!(tup.kind, "struct");
        // For unit/tuple structs there is no body field; signature falls back
        // to full node text with trailing `;` stripped.
        let unit = find(&out, "Unit");
        assert_eq!(unit.signature.as_deref(), Some("pub struct Unit"));
    }

    #[test]
    fn test_rust_does_not_extract_items_nested_inside_function_bodies() {
        // Local items inside a function body are deliberately not part of
        // the public symbol surface — the walker only recurses into
        // mod/trait/impl bodies.
        let src = "fn outer() { fn inner() {} }\n";
        let out = extract_rust(src, "src::n");
        let names: Vec<&str> = names(&out);
        assert_eq!(names, vec!["outer"], "local fn `inner` must not surface");
    }

    #[test]
    fn test_rust_default_extract_for_non_overriding_extractor_returns_empty() {
        // Sanity: the default `extract` impl on the trait returns an empty
        // ExtractedFile for any extractor that doesn't override it.
        struct Bare;
        impl LanguageExtractor for Bare {
            fn language_id(&self) -> LanguageId {
                LanguageId::Rust
            }
            fn extensions(&self) -> &'static [&'static str] {
                &["rs"]
            }
            fn tree_sitter_language(&self) -> tree_sitter::Language {
                tree_sitter_rust::language()
            }
        }
        let parsed = crate::parse::parse(b"fn x() {}".to_vec(), &Bare).unwrap();
        let ctx = ExtractContext {
            relative_path: "src/test.rs",
            module_path: "src::test",
        };
        let out = Bare.extract(&parsed, &ctx);
        assert_eq!(out.relative_path, "src/test.rs");
        assert_eq!(out.language, LanguageId::Rust);
        assert!(out.symbols.is_empty());
    }

    // ----- T5: refs / calls / imports / type-relations -----

    #[test]
    fn test_rust_extracts_simple_use_statement() {
        let src = "use std::collections::HashMap;\n";
        let out = extract_rust(src, "src::u");
        assert_eq!(out.imports.len(), 1, "expected one import");
        let i = &out.imports[0];
        assert_eq!(i.raw_path, "std::collections::HashMap");
        assert_eq!(i.alias, None);
        assert_eq!(i.line, 1);
    }

    #[test]
    fn test_rust_extracts_use_with_alias() {
        let src = "use std::io::Result as IoResult;\n";
        let out = extract_rust(src, "src::u");
        assert_eq!(out.imports.len(), 1);
        let i = &out.imports[0];
        assert_eq!(i.raw_path, "std::io::Result");
        assert_eq!(i.alias.as_deref(), Some("IoResult"));
    }

    #[test]
    fn test_rust_extracts_use_groups_into_individual_imports() {
        let src = "use std::collections::{HashMap, HashSet, BTreeMap};\n";
        let out = extract_rust(src, "src::u");
        let paths: Vec<&str> = out.imports.iter().map(|i| i.raw_path.as_str()).collect();
        assert!(paths.contains(&"std::collections::HashMap"));
        assert!(paths.contains(&"std::collections::HashSet"));
        assert!(paths.contains(&"std::collections::BTreeMap"));
        assert_eq!(out.imports.len(), 3);
    }

    #[test]
    fn test_rust_extracts_use_wildcard() {
        let src = "use std::prelude::v1::*;\n";
        let out = extract_rust(src, "src::u");
        assert_eq!(out.imports.len(), 1);
        assert!(
            out.imports[0].raw_path.contains('*'),
            "wildcard import not preserved: {:?}",
            out.imports[0]
        );
    }

    #[test]
    fn test_rust_extracts_function_call_attributed_to_caller() {
        let src = "\
fn helper() {}
fn main() {
    helper();
}
";
        let out = extract_rust(src, "src::c");
        let calls: Vec<(&str, &str)> = out
            .calls
            .iter()
            .map(|c| (c.caller_qualified_name.as_str(), c.callee_raw_name.as_str()))
            .collect();
        assert!(calls.contains(&("src::c::main", "helper")));
    }

    #[test]
    fn test_rust_extracts_method_call_attributed_to_caller() {
        let src = "\
struct W;
impl W {
    fn run(&self) {
        self.tick();
    }
    fn tick(&self) {}
}
";
        let out = extract_rust(src, "src::c");
        let callees: Vec<&str> = out
            .calls
            .iter()
            .filter(|c| c.caller_qualified_name == "src::c::W::run")
            .map(|c| c.callee_raw_name.as_str())
            .collect();
        assert!(
            callees.iter().any(|s| s.contains("tick")),
            "self.tick call not captured: {callees:?}"
        );
    }

    #[test]
    fn test_rust_extracts_impl_for_trait_emits_implements_relation() {
        let src = "\
struct Counter;
trait Tick { fn tick(&self); }
impl Tick for Counter {
    fn tick(&self) {}
}
";
        let out = extract_rust(src, "src::r");
        let rels: Vec<(&str, &str, &str)> = out
            .type_relations
            .iter()
            .map(|t| (
                t.symbol_qualified_name.as_str(),
                t.relation.as_str(),
                t.target_raw_name.as_str(),
            ))
            .collect();
        assert!(
            rels.contains(&("src::r::Counter", "implements", "Tick")),
            "rels were {rels:?}"
        );
    }

    #[test]
    fn test_rust_drops_implements_relation_when_target_type_is_imported() {
        // Repro for the pounze `DataStoreSession` bug: when the impl target
        // type is brought in via `use` (or otherwise not declared in this
        // file), the synthesised owner qname `<module>::DataStoreSession`
        // points at a phantom symbol. Prior behaviour emitted the row anyway
        // and the storage layer aborted the entire index with
        // "type relation … has unknown owner … extractor invariant violated".
        // The fix drops the relation post-walk.
        let src = "\
use crate::store::DataStoreSession;
trait SellerPlanStore { fn get(&self); }
impl SellerPlanStore for DataStoreSession {
    fn get(&self) {}
}
";
        let out = extract_rust(src, "pounze_api::queries::seller_plan");
        let implements: Vec<_> = out
            .type_relations
            .iter()
            .filter(|t| t.relation == "implements")
            .collect();
        assert!(
            implements.is_empty(),
            "expected no implements relation when target is foreign, got {implements:?}"
        );
        // Inherited methods are still emitted as symbols so the file stays
        // navigable; they just have no resolvable parent in this file. The
        // qname is disambiguated by the trait's short name to keep distinct
        // trait impls of the same method on the same type from colliding.
        let qns = qnames(&out);
        assert!(
            qns.iter().any(|q| *q
                == "pounze_api::queries::seller_plan::DataStoreSession::SellerPlanStore::get"),
            "expected trait-disambiguated method symbol under foreign impl, got {qns:?}"
        );
    }

    #[test]
    fn test_rust_methods_in_distinct_trait_impls_have_distinct_qnames() {
        // Repro for the pounze `mock_data_store.rs` collision: two trait impls
        // for the same type, each providing a method named `fetch_hsn_master`.
        // Rust permits this (disambiguated via `<T as Trait>::m`); the storage
        // layer enforces a `(file_id, qualified_name)` UNIQUE constraint, so
        // the extractor MUST produce distinct qnames.
        let src = "\
struct DataMockStoreSession;
trait InventoryStore { fn fetch(&self); }
trait CategoriesStore { fn fetch(&self); }
impl InventoryStore for DataMockStoreSession {
    fn fetch(&self) {}
}
impl CategoriesStore for DataMockStoreSession {
    fn fetch(&self) {}
}
";
        let out = extract_rust(src, "mods::mock");
        let qns = qnames(&out);
        assert!(
            qns.contains(&"mods::mock::DataMockStoreSession::InventoryStore::fetch"),
            "missing InventoryStore::fetch, got {qns:?}"
        );
        assert!(
            qns.contains(&"mods::mock::DataMockStoreSession::CategoriesStore::fetch"),
            "missing CategoriesStore::fetch, got {qns:?}"
        );
    }

    #[test]
    fn test_rust_extracts_inherent_impl_emits_no_implements_relation() {
        let src = "struct X;\nimpl X { fn x() {} }\n";
        let out = extract_rust(src, "src::r");
        let implements: Vec<_> = out
            .type_relations
            .iter()
            .filter(|t| t.relation == "implements")
            .collect();
        assert!(
            implements.is_empty(),
            "inherent impl must not produce an `implements` relation"
        );
    }

    #[test]
    fn test_rust_extracts_struct_field_type_relations() {
        let src = "struct Pair { left: u32, right: String }\n";
        let out = extract_rust(src, "src::s");
        let field_types: Vec<&str> = out
            .type_relations
            .iter()
            .filter(|t| t.symbol_qualified_name == "src::s::Pair" && t.relation == "field_type")
            .map(|t| t.target_raw_name.as_str())
            .collect();
        assert!(field_types.contains(&"u32"));
        assert!(field_types.contains(&"String"));
        assert_eq!(field_types.len(), 2);
    }

    #[test]
    fn test_rust_extracts_tuple_struct_field_type_relations() {
        let src = "pub struct Pair(pub u32, pub String);\n";
        let out = extract_rust(src, "src::s");
        let field_types: Vec<&str> = out
            .type_relations
            .iter()
            .filter(|t| t.symbol_qualified_name == "src::s::Pair" && t.relation == "field_type")
            .map(|t| t.target_raw_name.as_str())
            .collect();
        assert!(field_types.contains(&"u32"));
        assert!(field_types.contains(&"String"));
    }

    #[test]
    fn test_rust_extracts_type_identifier_refs_inside_function_body() {
        // Note: tree-sitter-rust tags a name only as `type_identifier` when it
        // is in a type context (annotation, return type, generic argument).
        // Inside `String::new()` the `String` node is a plain `identifier`
        // wrapped in `scoped_identifier`; v1 refs only capture the type-context
        // occurrences.
        let src = "\
fn make() -> String {
    let x: Vec<u8> = Vec::new();
    String::new()
}
";
        let out = extract_rust(src, "src::r");
        let refs: Vec<&str> = out.refs.iter().map(|r| r.raw_name.as_str()).collect();
        // `Vec` from `let x: Vec<u8>` and `String` from the return type.
        assert!(refs.contains(&"Vec"), "no Vec ref: {refs:?}");
        assert!(refs.contains(&"String"), "no String ref: {refs:?}");
    }

    #[test]
    fn test_rust_extracts_no_calls_for_function_signature_item_in_trait() {
        let src = "trait T { fn m(&self); }\n";
        let out = extract_rust(src, "src::t");
        // Trait method declarations have no body, so no calls inside them.
        let calls_in_m: Vec<_> = out
            .calls
            .iter()
            .filter(|c| c.caller_qualified_name == "src::t::T::m")
            .collect();
        assert!(calls_in_m.is_empty());
    }

    #[test]
    fn test_rust_extracts_use_at_correct_line() {
        let src = "\nfn before() {}\nuse foo::bar;\n";
        let out = extract_rust(src, "src::l");
        let i = out
            .imports
            .iter()
            .find(|i| i.raw_path == "foo::bar")
            .expect("foo::bar import");
        assert_eq!(i.line, 3);
    }

    // ----- T7: module_path_from_relative_path overrides -----

    #[test]
    fn test_rust_module_path_for_lib_rs_collapses_to_parent_dir() {
        let r = RustExtractor;
        assert_eq!(r.module_path_from_relative_path("src/lib.rs"), "src");
        assert_eq!(r.module_path_from_relative_path("crates/foo/src/lib.rs"), "crates::foo::src");
    }

    #[test]
    fn test_rust_module_path_for_main_rs_collapses_to_parent_dir() {
        let r = RustExtractor;
        assert_eq!(r.module_path_from_relative_path("src/main.rs"), "src");
    }

    #[test]
    fn test_rust_module_path_for_mod_rs_collapses_to_parent_dir() {
        let r = RustExtractor;
        assert_eq!(r.module_path_from_relative_path("src/foo/mod.rs"), "src::foo");
        assert_eq!(r.module_path_from_relative_path("mod.rs"), "");
    }

    #[test]
    fn test_rust_module_path_for_regular_rs_file_strips_extension() {
        let r = RustExtractor;
        assert_eq!(r.module_path_from_relative_path("src/foo.rs"), "src::foo");
        assert_eq!(r.module_path_from_relative_path("a/b/c.rs"), "a::b::c");
    }

    #[test]
    fn test_rust_module_path_at_root_with_no_parent() {
        let r = RustExtractor;
        assert_eq!(r.module_path_from_relative_path("foo.rs"), "foo");
        assert_eq!(r.module_path_from_relative_path("lib.rs"), "");
    }

    // ----- T8: extra coverage -----

    #[test]
    fn test_rust_nested_group_imports_decompose_to_individual_paths() {
        let src = "use foo::{a::{b, c}, d};\n";
        let out = extract_rust(src, "src::g");
        let paths: Vec<&str> = out.imports.iter().map(|i| i.raw_path.as_str()).collect();
        assert!(paths.contains(&"foo::a::b"), "missing foo::a::b: {paths:?}");
        assert!(paths.contains(&"foo::a::c"), "missing foo::a::c: {paths:?}");
        assert!(paths.contains(&"foo::d"), "missing foo::d: {paths:?}");
    }

    #[test]
    fn test_rust_impl_trait_for_generic_type_strips_generics_in_qname() {
        // `impl Foo for Bar<T>` — type is `Bar<T>`, generics stripped to `Bar`.
        let src = "\
struct Bar<T>(T);
trait Foo { fn foo(&self); }
impl<T> Foo for Bar<T> {
    fn foo(&self) {}
}
";
        let out = extract_rust(src, "src::g");
        let rels: Vec<(&str, &str, &str)> = out
            .type_relations
            .iter()
            .map(|t| (
                t.symbol_qualified_name.as_str(),
                t.relation.as_str(),
                t.target_raw_name.as_str(),
            ))
            .collect();
        assert!(
            rels.contains(&("src::g::Bar", "implements", "Foo")),
            "rels were {rels:?}"
        );
    }

    #[test]
    fn test_rust_attribute_prefixed_item_start_line_documents_current_behavior() {
        // tree-sitter-rust 0.21 emits attributes as **siblings** of the
        // struct_item node, not as part of it. So the `struct_item`'s
        // `start_position` is the `struct` keyword line (line 2 here), not
        // the `#[derive(Debug)]` line (line 1). Pin this so a future
        // grammar bump that subsumes attributes is caught.
        let src = "#[derive(Debug)]\nstruct X;\n";
        let out = extract_rust(src, "src::a");
        let s = find(&out, "X");
        assert_eq!(
            s.start_line, 2,
            "attribute-prefixed start_line: {} (expected 2 — `struct` keyword line)",
            s.start_line
        );
    }

    #[test]
    fn test_rust_associated_const_inside_trait_emits_const_kind() {
        let src = "trait T { const MAX: i32; }\n";
        let out = extract_rust(src, "src::t");
        // `const_item` is matched at the trait-body level just like at the
        // top level. The kind stays `const` (no separate `associated_const`).
        let max = find(&out, "MAX");
        assert_eq!(max.kind, "const");
        assert_eq!(max.parent_qualified_name.as_deref(), Some("src::t::T"));
        assert_eq!(max.qualified_name, "src::t::T::MAX");
    }

    #[test]
    fn test_rust_macro_rules_definition_is_not_extracted_as_a_symbol() {
        // `macro_rules!` is a separate AST node (`macro_definition` or
        // `macro_rules`) — the walker's match arms do not cover it, so it
        // is silently dropped. Document this contract.
        let src = "macro_rules! m { () => {} }\nfn after() {}\n";
        let out = extract_rust(src, "src::m");
        let names: Vec<&str> = names(&out);
        assert_eq!(names, vec!["after"], "macro_rules must not surface as a symbol");
    }

    #[test]
    fn test_rust_primitive_type_in_signature_does_not_emit_a_ref() {
        // `u32`, `i64`, `bool`, etc. are `primitive_type` AST nodes, NOT
        // `type_identifier`. v1 only collects `type_identifier` refs, so
        // primitives are deliberately omitted.
        let src = "fn add(x: u32, y: u32) -> u32 { x + y }\n";
        let out = extract_rust(src, "src::p");
        let refs: Vec<&str> = out.refs.iter().map(|r| r.raw_name.as_str()).collect();
        assert!(
            !refs.contains(&"u32"),
            "primitive_type u32 must not be in refs: {refs:?}"
        );
    }
}
