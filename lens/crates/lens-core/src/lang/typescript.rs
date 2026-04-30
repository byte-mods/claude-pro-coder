//! TypeScript language extractor. Walks a tree-sitter-typescript AST and
//! emits structured [`ExtractedSymbol`] records, plus refs, calls, imports,
//! and type relations.
//!
//! Two grammars under one extractor: `.ts` uses `tree_sitter_typescript::language_typescript()`,
//! `.tsx` uses `tree_sitter_typescript::language_tsx()`. Both register against
//! the same [`LanguageId::TypeScript`]; the registry's `by_ext` map disambiguates
//! at parse time.
//!
//! Qualified-name convention is `::` (Rust-style) for cross-language
//! consistency. Module path strips the file extension and joins directory
//! segments with `::`. `index.ts` collapses to its parent directory (mirrors
//! Python's `__init__.py` rule), since TypeScript treats `import { x } from
//! './foo'` as resolving to `./foo/index.ts`.
//!
//! v1 scope: top-level functions / arrow-function consts / classes / interfaces /
//! type aliases / enums / methods / class heritage / imports / call sites. Not
//! covered: namespaces, decorators (recorded as raw refs but not expanded),
//! generics analysis, dynamic imports, JSX-specific extraction.

use tree_sitter::Node;

use crate::extract::{
    ExtractContext, ExtractedCall, ExtractedFile, ExtractedImport, ExtractedRef, ExtractedSymbol,
    ExtractedTypeRel,
};
use crate::lang::{LanguageExtractor, LanguageId};
use crate::parse::ParsedFile;

/// TypeScript extractor — single struct, two flavours (ts, tsx) selected at
/// construction time. The two flavours share the entire walker; only the
/// tree-sitter grammar and the registered extension differ.
pub struct TypeScriptExtractor {
    is_tsx: bool,
}

impl TypeScriptExtractor {
    pub const fn ts() -> Self {
        Self { is_tsx: false }
    }

    pub const fn tsx() -> Self {
        Self { is_tsx: true }
    }
}

impl LanguageExtractor for TypeScriptExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::TypeScript
    }

    fn extensions(&self) -> &'static [&'static str] {
        if self.is_tsx {
            &["tsx"]
        } else {
            &["ts"]
        }
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        if self.is_tsx {
            tree_sitter_typescript::language_tsx()
        } else {
            tree_sitter_typescript::language_typescript()
        }
    }

    fn extract(&self, parsed: &ParsedFile, ctx: &ExtractContext) -> ExtractedFile {
        let mut out = ExtractedFile::empty(ctx.relative_path, LanguageId::TypeScript);
        let scope = Scope::file_root();
        walk(parsed.root_node(), &scope, ctx, parsed.source(), &mut out);
        out
    }

    /// TypeScript convention:
    ///   - strip `.ts` / `.tsx`
    ///   - join with `::`
    ///   - collapse `index.ts` / `index.tsx` to its parent directory
    fn module_path_from_relative_path(&self, relative_path: &str) -> String {
        let (parent, last) = match relative_path.rfind('/') {
            Some(i) => (&relative_path[..i], &relative_path[i + 1..]),
            None => ("", relative_path),
        };
        let parent_joined = parent.replace('/', "::");
        if last == "index.ts" || last == "index.tsx" {
            return parent_joined;
        }
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

#[derive(Clone)]
pub(crate) struct Scope {
    /// Qualified name of the enclosing symbol, or `None` at the file root.
    pub(crate) parent_qname: Option<String>,
    pub(crate) kind: ScopeKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    File,
    Class,
    Interface,
}

impl Scope {
    pub(crate) fn file_root() -> Self {
        Self {
            parent_qname: None,
            kind: ScopeKind::File,
        }
    }
}

/// Top-level walker. Called at the file root and on class / interface bodies.
/// Function / method bodies are not walked recursively for symbol emission;
/// they are walked by [`walk_within_function`] which collects calls and refs
/// without emitting nested definitions as separate symbols.
///
/// Exposed `pub(crate)` so the JavaScript extractor can reuse it — JS and
/// TS share a tree-sitter AST shape for the constructs lens cares about
/// (function_declaration, class_declaration, import_statement, call_expression,
/// etc.); TS-specific node kinds simply don't appear in JS code.
pub(crate) fn walk(node: Node, scope: &Scope, ctx: &ExtractContext, source: &[u8], out: &mut ExtractedFile) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => emit_function(child, scope, ctx, source, out),
            "class_declaration" | "abstract_class_declaration" => {
                emit_class(child, scope, ctx, source, out)
            }
            "interface_declaration" => emit_interface(child, scope, ctx, source, out),
            "type_alias_declaration" => emit_type_alias(child, scope, ctx, source, out),
            "enum_declaration" => emit_enum(child, scope, ctx, source, out),
            "lexical_declaration" | "variable_statement" | "variable_declaration" => {
                emit_lexical(child, scope, ctx, source, out)
            }
            "export_statement" => {
                // Recurse into the export — the inner declaration is what we
                // care about. We do NOT emit a separate symbol for the export
                // itself; that would double-count. Tracked-only-by-import
                // export information is out of scope for v1.
                walk(child, scope, ctx, source, out);
            }
            "import_statement" => extract_import(child, source, out),
            "method_definition" | "method_signature" => {
                if scope.kind == ScopeKind::Class || scope.kind == ScopeKind::Interface {
                    emit_method(child, scope, ctx, source, out);
                }
            }
            // public_field_definition is TS-specific; field_definition is the
            // JavaScript variant; property_signature shows up in interfaces.
            "public_field_definition" | "field_definition" | "property_signature" => {
                if scope.kind == ScopeKind::Class || scope.kind == ScopeKind::Interface {
                    emit_class_field(child, scope, ctx, source, out);
                }
            }
            _ => {}
        }
    }
}

fn emit_function(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let Some(name) = child_field_text(&node, "name", source) else {
        return;
    };
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    let sig = signature_until_body(&node, source);
    let visibility = if has_export_ancestor(&node) {
        Some("export".to_string())
    } else {
        None
    };
    let sym = make_symbol(&qname, &name, "function", &node, source, sig, visibility, scope.parent_qname.clone());
    out.symbols.push(sym);

    // Walk the parameter list for type annotations (e.g. `x: User`) — these
    // live outside the body but are still references that callers care
    // about for impact analysis. Run before the body walk so refs are
    // ordered roughly in source order.
    if let Some(params) = node.child_by_field_name("parameters") {
        walk_for_type_refs(params, source, out);
    }
    // Return-type annotation (e.g. `): User { ... }`) sits on the function
    // node itself via the `return_type` field on most arrow/function nodes.
    if let Some(rt) = node.child_by_field_name("return_type") {
        walk_for_type_refs(rt, source, out);
    }

    // Walk the body for calls + refs.
    if let Some(body) = node.child_by_field_name("body") {
        walk_within_function(body, &qname, source, out);
    }
}

/// Recursive scan that records `type_identifier` and `predefined_type` nodes
/// as ref entries, without descending into nested function/class bodies. Used
/// for parameter lists, return types, and similar annotation regions.
fn walk_for_type_refs(node: Node, source: &[u8], out: &mut ExtractedFile) {
    let mut stack: Vec<Node> = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "type_identifier" | "predefined_type" => {
                let line = (n.start_position().row as u32).saturating_add(1);
                let col = n.start_position().column as u32;
                out.refs.push(ExtractedRef {
                    raw_name: node_text(n, source),
                    kind: "type".to_string(),
                    line,
                    col,
                    end_line: (n.end_position().row as u32).saturating_add(1),
                    end_col: n.end_position().column as u32,
                });
            }
            // Don't descend into bodies — callers handle those separately.
            "statement_block" | "function_body" | "class_body" => continue,
            _ => {}
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

fn emit_class(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let Some(name) = child_field_text(&node, "name", source) else {
        return;
    };
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    let sig = signature_until_body(&node, source);
    let visibility = if has_export_ancestor(&node) {
        Some("export".to_string())
    } else {
        None
    };
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "class",
        &node,
        source,
        sig,
        visibility,
        scope.parent_qname.clone(),
    ));

    // Heritage: `extends X` and `implements X, Y`.
    extract_class_heritage(&node, &qname, source, out);

    // Walk the class body for methods.
    if let Some(body) = node.child_by_field_name("body") {
        let inner = Scope {
            parent_qname: Some(qname.clone()),
            kind: ScopeKind::Class,
        };
        walk(body, &inner, ctx, source, out);
    }
}

fn emit_interface(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let Some(name) = child_field_text(&node, "name", source) else {
        return;
    };
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    let sig = signature_until_body(&node, source);
    let visibility = if has_export_ancestor(&node) {
        Some("export".to_string())
    } else {
        None
    };
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "interface",
        &node,
        source,
        sig,
        visibility,
        scope.parent_qname.clone(),
    ));

    // Interface heritage: `interface X extends Y` (no `implements` in TS interfaces).
    extract_interface_heritage(&node, &qname, source, out);

    // Walk the interface body for property/method signatures.
    if let Some(body) = node.child_by_field_name("body") {
        let inner = Scope {
            parent_qname: Some(qname.clone()),
            kind: ScopeKind::Interface,
        };
        walk(body, &inner, ctx, source, out);
    }
}

fn emit_type_alias(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let Some(name) = child_field_text(&node, "name", source) else {
        return;
    };
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    let sig = node_text(node, source);
    let visibility = if has_export_ancestor(&node) {
        Some("export".to_string())
    } else {
        None
    };
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "type",
        &node,
        source,
        Some(sig),
        visibility,
        scope.parent_qname.clone(),
    ));
}

fn emit_enum(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let Some(name) = child_field_text(&node, "name", source) else {
        return;
    };
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    let sig = signature_until_body(&node, source);
    let visibility = if has_export_ancestor(&node) {
        Some("export".to_string())
    } else {
        None
    };
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "enum",
        &node,
        source,
        sig,
        visibility,
        scope.parent_qname.clone(),
    ));
}

fn emit_method(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let Some(name) = child_field_text(&node, "name", source) else {
        return;
    };
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    let sig = signature_until_body(&node, source);
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "method",
        &node,
        source,
        sig,
        None,
        scope.parent_qname.clone(),
    ));
    if let Some(body) = node.child_by_field_name("body") {
        walk_within_function(body, &qname, source, out);
    }
}

/// `class Foo { bar = (x) => ... }` — emit the field as a "method" symbol
/// when its initialiser is an arrow_function or function_expression. Plain
/// data fields are not emitted as symbols (tracking them adds noise).
fn emit_class_field(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // Field shapes:
    //   TS public_field_definition: has `name` and `value` named fields.
    //   JS field_definition: positional — first identifier-like child is the
    //     name; the value (if any) follows the `=` token. We try named
    //     fields first, then fall back to positional scan.
    let (name, value) = match child_field_text(&node, "name", source) {
        Some(n) => (n, node.child_by_field_name("value")),
        None => {
            // Positional scan: identifier (or property_identifier) first,
            // then locate the `=` token to find the value.
            let mut name_node: Option<Node> = None;
            let mut value_node: Option<Node> = None;
            let mut after_equals = false;
            let mut cursor = node.walk();
            for c in node.children(&mut cursor) {
                if after_equals && value_node.is_none() {
                    value_node = Some(c);
                    continue;
                }
                match c.kind() {
                    "property_identifier" | "identifier" => {
                        if name_node.is_none() {
                            name_node = Some(c);
                        }
                    }
                    "=" => after_equals = true,
                    _ => {}
                }
            }
            let Some(n) = name_node else {
                return;
            };
            (node_text(n, source), value_node)
        }
    };
    let is_function_field = value.is_some_and(|v| {
        matches!(v.kind(), "arrow_function" | "function_expression" | "function")
    });
    if !is_function_field {
        return;
    }
    let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
    let sig = node_text(node, source);
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "method",
        &node,
        source,
        Some(sig),
        None,
        scope.parent_qname.clone(),
    ));
    if let Some(v) = value {
        if let Some(body) = v.child_by_field_name("body") {
            walk_within_function(body, &qname, source, out);
        }
    }
}

/// `const foo = () => {}` / `let bar = function () {}` / `const x = 1`.
/// The function-valued declarators emit "function" symbols; plain data
/// declarators are not currently emitted (data-shape extraction is out of
/// scope for v1). All other declarators are silently skipped.
fn emit_lexical(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "variable_declarator" {
            continue;
        }
        let Some(name) = child_field_text(&child, "name", source) else {
            continue;
        };
        let value = child.child_by_field_name("value");
        let is_function_value = value.is_some_and(|v| {
            matches!(v.kind(), "arrow_function" | "function_expression" | "function")
        });
        if !is_function_value {
            // Skip non-function lexical declarations in v1.
            continue;
        }
        let qname = build_qname(ctx.module_path, scope.parent_qname.as_deref(), &name);
        let sig = match value {
            Some(v) => format!("{} = {}", name, signature_until_body(&v, source).unwrap_or_default()),
            None => name.clone(),
        };
        let visibility = if has_export_ancestor(&node) {
            Some("export".to_string())
        } else {
            None
        };
        out.symbols.push(make_symbol(
            &qname,
            &name,
            "function",
            &child,
            source,
            Some(sig),
            visibility,
            scope.parent_qname.clone(),
        ));
        if let Some(v) = value {
            if let Some(body) = v.child_by_field_name("body") {
                walk_within_function(body, &qname, source, out);
            }
        }
    }
}

fn extract_import(node: Node, source: &[u8], out: &mut ExtractedFile) {
    // import_statement → string source path, plus optional clause with
    // identifiers / namespace import. We record:
    //   - one import per declarator, with raw_path = the source string
    //   - alias = the local name when it differs from the imported name
    let source_node = node.child_by_field_name("source");
    let raw_path = match source_node {
        Some(s) => unquote(&node_text(s, source)),
        None => return,
    };
    let line = (node.start_position().row as u32).saturating_add(1);

    // Walk the import_clause for named/namespace/default imports.
    let mut cursor = node.walk();
    let mut emitted = 0;
    for child in node.children(&mut cursor) {
        if child.kind() != "import_clause" {
            continue;
        }
        let mut sub = child.walk();
        for sc in child.children(&mut sub) {
            match sc.kind() {
                // `import Foo from "./mod"` — default import.
                "identifier" => {
                    let alias = node_text(sc, source);
                    out.imports.push(ExtractedImport {
                        raw_path: raw_path.clone(),
                        alias: Some(alias),
                        line,
                    });
                    emitted += 1;
                }
                // `import * as Foo from "./mod"` — namespace import.
                "namespace_import" => {
                    let alias = match sc.child_by_field_name("alias") {
                        Some(n) => Some(node_text(n, source)),
                        None => {
                            // Fallback: scan child identifiers for the name.
                            // `let mut sub` must outlive the `find` call; the
                            // borrow checker rejects an inline cursor here.
                            let mut sub = sc.walk();
                            let mut found: Option<String> = None;
                            for n in sc.children(&mut sub) {
                                if n.kind() == "identifier" {
                                    found = Some(node_text(n, source));
                                    break;
                                }
                            }
                            found
                        }
                    };
                    out.imports.push(ExtractedImport {
                        raw_path: raw_path.clone(),
                        alias,
                        line,
                    });
                    emitted += 1;
                }
                // `import { a, b as c } from "./mod"` — named imports.
                "named_imports" => {
                    let mut sub2 = sc.walk();
                    for ni in sc.children(&mut sub2) {
                        if ni.kind() == "import_specifier" {
                            let imported = ni.child_by_field_name("name").map(|n| node_text(n, source));
                            let alias = ni.child_by_field_name("alias").map(|n| node_text(n, source));
                            out.imports.push(ExtractedImport {
                                raw_path: raw_path.clone(),
                                alias: alias.or(imported),
                                line,
                            });
                            emitted += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Side-effect import (`import "./styles.css"`) — record one row with
    // alias=None so callers can see the raw_path participated.
    if emitted == 0 {
        out.imports.push(ExtractedImport { raw_path, alias: None, line });
    }
}

fn extract_class_heritage(node: &Node, qname: &str, source: &[u8], out: &mut ExtractedFile) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "class_heritage" {
            continue;
        }
        // The shape of `class_heritage` differs between TS and JS grammars:
        //
        //   TS: class_heritage > extends_clause > identifier
        //                       implements_clause > identifier (one or more)
        //   JS: class_heritage > `extends` (leaf) + identifier (one)
        //
        // So we look for the wrapper clauses first; if absent, fall through
        // to a JS-shaped scan that treats the immediate identifier children
        // as `extends` targets (JS classes have no `implements`).
        let mut saw_wrapper = false;
        let mut hcursor = child.walk();
        for h in child.children(&mut hcursor) {
            match h.kind() {
                "extends_clause" => {
                    saw_wrapper = true;
                    extract_heritage_targets(&h, qname, "extends", source, out);
                }
                "implements_clause" => {
                    saw_wrapper = true;
                    extract_heritage_targets(&h, qname, "implements", source, out);
                }
                _ => {}
            }
        }
        if !saw_wrapper {
            // JS shape — treat the whole class_heritage node as one
            // implicit "extends" clause. JavaScript only supports
            // single-extends, so this captures all cases correctly.
            extract_heritage_targets(&child, qname, "extends", source, out);
        }
    }
}

fn extract_interface_heritage(node: &Node, qname: &str, source: &[u8], out: &mut ExtractedFile) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "extends_type_clause" || child.kind() == "extends_clause" {
            extract_heritage_targets(&child, qname, "extends", source, out);
        }
    }
}

fn extract_heritage_targets(
    clause: &Node,
    qname: &str,
    relation: &str,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        match child.kind() {
            "identifier" | "type_identifier" | "nested_type_identifier" => {
                let target = node_text(child, source);
                let line = (child.start_position().row as u32).saturating_add(1);
                out.type_relations.push(ExtractedTypeRel {
                    symbol_qualified_name: qname.to_string(),
                    relation: relation.to_string(),
                    target_raw_name: target,
                    line,
                });
            }
            // `extends Foo<T>` — the inner identifier is what we want.
            "generic_type" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let target = node_text(name_node, source);
                    let line = (name_node.start_position().row as u32).saturating_add(1);
                    out.type_relations.push(ExtractedTypeRel {
                        symbol_qualified_name: qname.to_string(),
                        relation: relation.to_string(),
                        target_raw_name: target,
                        line,
                    });
                }
            }
            _ => {}
        }
    }
}

/// Walk inside a function/method body. Collects:
///   - call_expression nodes → ExtractedCall
///   - identifier refs at statement positions → ExtractedRef (best-effort)
fn walk_within_function(node: Node, caller_qname: &str, source: &[u8], out: &mut ExtractedFile) {
    let mut stack: Vec<Node> = vec![node];
    while let Some(n) = stack.pop() {
        let mut children = n.walk();
        for c in n.children(&mut children) {
            stack.push(c);
        }
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = n.child_by_field_name("function") {
                    let callee_text = node_text(callee, source);
                    // For `obj.method(...)` we want just `method` as the raw
                    // name — the rightmost segment after a `.`. For `foo(...)`
                    // it's the whole text. For computed accesses (`obj[k]`)
                    // we record the whole text and let resolution skip it.
                    let raw_name = match callee_text.rsplit('.').next() {
                        Some(tail) if !tail.is_empty() => tail.to_string(),
                        _ => callee_text,
                    };
                    let line = (n.start_position().row as u32).saturating_add(1);
                    let col = n.start_position().column as u32;
                    out.calls.push(ExtractedCall {
                        caller_qualified_name: caller_qname.to_string(),
                        callee_raw_name: raw_name.clone(),
                        line,
                        col,
                    });
                    // Also record as a ref so resolution / refs queries see it.
                    out.refs.push(ExtractedRef {
                        raw_name,
                        kind: "call".to_string(),
                        line,
                        col,
                        end_line: (n.end_position().row as u32).saturating_add(1),
                        end_col: n.end_position().column as u32,
                    });
                }
            }
            // Type annotations and declarations reference type names.
            "type_identifier" | "predefined_type" => {
                let line = (n.start_position().row as u32).saturating_add(1);
                let col = n.start_position().column as u32;
                out.refs.push(ExtractedRef {
                    raw_name: node_text(n, source),
                    kind: "type".to_string(),
                    line,
                    col,
                    end_line: (n.end_position().row as u32).saturating_add(1),
                    end_col: n.end_position().column as u32,
                });
            }
            _ => {}
        }
    }
}

// ----- Helpers -----

fn make_symbol(
    qname: &str,
    name: &str,
    kind: &str,
    node: &Node,
    source: &[u8],
    signature: Option<String>,
    visibility: Option<String>,
    parent: Option<String>,
) -> ExtractedSymbol {
    let doc_comment = extract_doc_comment(node, source);
    let (sl, sc, el, ec) = position_of(node);
    ExtractedSymbol {
        qualified_name: qname.to_string(),
        name: name.to_string(),
        kind: kind.to_string(),
        start_line: sl,
        start_col: sc,
        end_line: el,
        end_col: ec,
        body_start_byte: node.start_byte() as u32,
        body_end_byte: node.end_byte() as u32,
        signature,
        visibility,
        parent_qualified_name: parent,
        doc_comment,
    }
}

/// Walk the prev_sibling chain harvesting JSDoc / line / block comments
/// immediately preceding `node`. Returns `None` when no comment was attached.
///
/// What counts:
///   - JSDoc block: `/** ... */`
///   - Plain block: `/* ... */`
///   - Line comments: `// ...` (multiple consecutive)
///
/// Plain // and /* */ are accepted as docs in TS/JS — unlike Rust's `///`
/// distinction, the JS ecosystem doesn't have a "doc-only" comment marker;
/// JSDoc tooling treats any `/** */` immediately preceding a declaration as
/// documentation, and developer convention extends that to plain `//` lines.
pub(crate) fn extract_doc_comment(node: &Node, source: &[u8]) -> Option<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut cursor = node.prev_sibling();
    while let Some(sib) = cursor {
        if sib.kind() != "comment" {
            break;
        }
        let txt = node_text(sib, source);
        chunks.push(normalise_js_comment(&txt));
        cursor = sib.prev_sibling();
    }
    if chunks.is_empty() {
        None
    } else {
        chunks.reverse();
        Some(chunks.join("\n").trim().to_string())
    }
}

/// Strip JSDoc / block / line comment markers and per-line `*` indentation.
/// Leaves textual content with whitespace trimmed per line.
fn normalise_js_comment(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with("/*") {
        let inner = raw
            .trim_start_matches("/**")
            .trim_start_matches("/*")
            .trim_end_matches("*/")
            .trim_matches('\n');
        // Drop a leading `*` (with optional space) on each line — the
        // canonical JSDoc layout. Preserves blank lines.
        inner
            .lines()
            .map(|l| {
                let l = l.trim();
                l.strip_prefix("* ").or_else(|| l.strip_prefix("*")).unwrap_or(l).to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    } else if let Some(rest) = raw.strip_prefix("//") {
        rest.trim().to_string()
    } else {
        raw.to_string()
    }
}

fn build_qname(module_path: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}::{name}"),
        None => {
            if module_path.is_empty() {
                name.to_string()
            } else {
                format!("{module_path}::{name}")
            }
        }
    }
}

fn child_field_text(node: &Node, field: &str, source: &[u8]) -> Option<String> {
    node.child_by_field_name(field).map(|n| node_text(n, source))
}

fn node_text(node: Node, source: &[u8]) -> String {
    let s = node.start_byte();
    let e = node.end_byte();
    String::from_utf8_lossy(&source[s..e]).to_string()
}

fn position_of(node: &Node) -> (u32, u32, u32, u32) {
    let sp = node.start_position();
    let ep = node.end_position();
    (
        (sp.row as u32).saturating_add(1),
        sp.column as u32,
        (ep.row as u32).saturating_add(1),
        ep.column as u32,
    )
}

/// Pull a signature-shaped slice from a declaration node — everything before
/// the body's `{`. Returns `None` for nodes without a body field.
fn signature_until_body(node: &Node, source: &[u8]) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let start = node.start_byte();
    let end = body.start_byte();
    if end <= start || end > source.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&source[start..end]).trim().to_string())
}

fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        || (trimmed.starts_with('`') && trimmed.ends_with('`'))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn has_export_ancestor(node: &Node) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if p.kind() == "export_statement" {
            return true;
        }
        cur = p.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ExtractContext;
    use crate::parse;

    fn extract_ts(src: &str, module_path: &str) -> ExtractedFile {
        let ext = TypeScriptExtractor::ts();
        let parsed = parse(src.as_bytes().to_vec(), &ext).expect("parse");
        let ctx = ExtractContext {
            relative_path: "test.ts",
            module_path,
        };
        ext.extract(&parsed, &ctx)
    }

    fn extract_tsx(src: &str, module_path: &str) -> ExtractedFile {
        let ext = TypeScriptExtractor::tsx();
        let parsed = parse(src.as_bytes().to_vec(), &ext).expect("parse");
        let ctx = ExtractContext {
            relative_path: "test.tsx",
            module_path,
        };
        ext.extract(&parsed, &ctx)
    }

    fn names(out: &ExtractedFile) -> Vec<&str> {
        out.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn qnames(out: &ExtractedFile) -> Vec<&str> {
        out.symbols.iter().map(|s| s.qualified_name.as_str()).collect()
    }

    fn find<'a>(out: &'a ExtractedFile, name: &str) -> &'a ExtractedSymbol {
        out.symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no symbol named {name}; have {:?}", names(out)))
    }

    #[test]
    fn test_typescript_extracts_top_level_function_with_signature() {
        let src = "function add(a: number, b: number): number { return a + b; }\n";
        let out = extract_ts(src, "math");
        assert_eq!(names(&out), vec!["add"]);
        let sym = find(&out, "add");
        assert_eq!(sym.kind, "function");
        assert_eq!(sym.qualified_name, "math::add");
        assert!(sym.signature.as_deref().unwrap_or("").contains("function add"));
    }

    #[test]
    fn test_typescript_extracts_arrow_function_const() {
        let src = "const greet = (name: string) => `hi ${name}`;\n";
        let out = extract_ts(src, "greet_mod");
        assert_eq!(names(&out), vec!["greet"]);
        let sym = find(&out, "greet");
        assert_eq!(sym.kind, "function");
        assert_eq!(sym.qualified_name, "greet_mod::greet");
    }

    #[test]
    fn test_typescript_extracts_function_expression_const() {
        let src = "const square = function (n: number) { return n * n; };\n";
        let out = extract_ts(src, "m");
        assert_eq!(names(&out), vec!["square"]);
        assert_eq!(find(&out, "square").kind, "function");
    }

    #[test]
    fn test_typescript_skips_non_function_const_declarators() {
        let src = "const PI = 3.14;\nconst greeting: string = 'hello';\n";
        let out = extract_ts(src, "consts");
        assert!(out.symbols.is_empty(), "data const should not emit a symbol in v1");
    }

    #[test]
    fn test_typescript_extracts_class_with_method() {
        let src = "class Greeter {\n    name: string;\n    greet() { return this.name; }\n}\n";
        let out = extract_ts(src, "g");
        let qns = qnames(&out);
        assert!(qns.contains(&"g::Greeter"));
        assert!(qns.contains(&"g::Greeter::greet"));
        let cls = find(&out, "Greeter");
        assert_eq!(cls.kind, "class");
        let m = find(&out, "greet");
        assert_eq!(m.kind, "method");
        assert_eq!(m.parent_qualified_name.as_deref(), Some("g::Greeter"));
    }

    #[test]
    fn test_typescript_extracts_interface_with_method_signature() {
        let src = "interface Logger {\n    info(msg: string): void;\n    error(msg: string): void;\n}\n";
        let out = extract_ts(src, "logging");
        let qns = qnames(&out);
        assert!(qns.contains(&"logging::Logger"));
        assert!(qns.contains(&"logging::Logger::info"));
        assert!(qns.contains(&"logging::Logger::error"));
    }

    #[test]
    fn test_typescript_extracts_type_alias() {
        let src = "type UserId = string;\n";
        let out = extract_ts(src, "types");
        let s = find(&out, "UserId");
        assert_eq!(s.kind, "type");
        assert_eq!(s.qualified_name, "types::UserId");
    }

    #[test]
    fn test_typescript_extracts_enum() {
        let src = "enum Status { Active, Inactive }\n";
        let out = extract_ts(src, "m");
        let s = find(&out, "Status");
        assert_eq!(s.kind, "enum");
    }

    #[test]
    fn test_typescript_extracts_class_extends_relation() {
        let src = "class Cat extends Animal {}\n";
        let out = extract_ts(src, "zoo");
        assert!(out
            .type_relations
            .iter()
            .any(|t| t.relation == "extends"
                && t.target_raw_name == "Animal"
                && t.symbol_qualified_name == "zoo::Cat"));
    }

    #[test]
    fn test_typescript_extracts_class_implements_relation() {
        let src = "class Dog implements Pet, Mammal {}\n";
        let out = extract_ts(src, "zoo");
        let impls: Vec<&str> = out
            .type_relations
            .iter()
            .filter(|t| t.relation == "implements")
            .map(|t| t.target_raw_name.as_str())
            .collect();
        assert!(impls.contains(&"Pet"));
        assert!(impls.contains(&"Mammal"));
    }

    #[test]
    fn test_typescript_extracts_named_import() {
        let src = "import { foo, bar as baz } from './mod';\n";
        let out = extract_ts(src, "user");
        let aliases: Vec<&str> = out
            .imports
            .iter()
            .map(|i| i.alias.as_deref().unwrap_or(""))
            .collect();
        assert!(aliases.contains(&"foo"));
        assert!(aliases.contains(&"baz"));
        for imp in &out.imports {
            assert_eq!(imp.raw_path, "./mod");
        }
    }

    #[test]
    fn test_typescript_extracts_default_import() {
        let src = "import React from 'react';\n";
        let out = extract_ts(src, "app");
        assert_eq!(out.imports.len(), 1);
        assert_eq!(out.imports[0].alias.as_deref(), Some("React"));
        assert_eq!(out.imports[0].raw_path, "react");
    }

    #[test]
    fn test_typescript_extracts_namespace_import() {
        let src = "import * as fs from 'fs';\n";
        let out = extract_ts(src, "app");
        assert_eq!(out.imports.len(), 1);
        assert_eq!(out.imports[0].alias.as_deref(), Some("fs"));
        assert_eq!(out.imports[0].raw_path, "fs");
    }

    #[test]
    fn test_typescript_extracts_side_effect_import() {
        let src = "import './styles.css';\n";
        let out = extract_ts(src, "app");
        assert_eq!(out.imports.len(), 1);
        assert_eq!(out.imports[0].raw_path, "./styles.css");
        assert!(out.imports[0].alias.is_none());
    }

    #[test]
    fn test_typescript_records_call_inside_function_body() {
        let src = "function helper() {}\nfunction main() { helper(); }\n";
        let out = extract_ts(src, "m");
        assert!(out
            .calls
            .iter()
            .any(|c| c.caller_qualified_name == "m::main" && c.callee_raw_name == "helper"));
    }

    #[test]
    fn test_typescript_records_method_call_unqualified_callee_name() {
        let src = "function main() { logger.info('hi'); }\n";
        let out = extract_ts(src, "m");
        assert!(out
            .calls
            .iter()
            .any(|c| c.caller_qualified_name == "m::main" && c.callee_raw_name == "info"));
    }

    #[test]
    fn test_typescript_records_call_inside_arrow_function() {
        let src = "const main = () => { helper(); };\nfunction helper() {}\n";
        let out = extract_ts(src, "m");
        assert!(out
            .calls
            .iter()
            .any(|c| c.caller_qualified_name == "m::main" && c.callee_raw_name == "helper"));
    }

    #[test]
    fn test_typescript_records_call_inside_class_method() {
        let src = "class A {\n    run() { this.helper(); }\n    helper() {}\n}\n";
        let out = extract_ts(src, "m");
        assert!(out
            .calls
            .iter()
            .any(|c| c.caller_qualified_name == "m::A::run" && c.callee_raw_name == "helper"));
    }

    #[test]
    fn test_typescript_module_path_drops_extension() {
        let ext = TypeScriptExtractor::ts();
        assert_eq!(ext.module_path_from_relative_path("src/auth/session.ts"), "src::auth::session");
        assert_eq!(ext.module_path_from_relative_path("a.ts"), "a");
    }

    #[test]
    fn test_typescript_module_path_collapses_index_files_to_parent_dir() {
        let ext = TypeScriptExtractor::ts();
        assert_eq!(ext.module_path_from_relative_path("src/auth/index.ts"), "src::auth");
        let extx = TypeScriptExtractor::tsx();
        assert_eq!(extx.module_path_from_relative_path("src/ui/index.tsx"), "src::ui");
    }

    #[test]
    fn test_typescript_export_is_recorded_as_visibility() {
        let src = "export function pub() {}\nfunction priv() {}\n";
        let out = extract_ts(src, "m");
        assert_eq!(find(&out, "pub").visibility.as_deref(), Some("export"));
        assert!(find(&out, "priv").visibility.is_none());
    }

    #[test]
    fn test_tsx_grammar_parses_jsx_and_extracts_component_function() {
        // The .tsx flavour must accept JSX inside a function body. We only
        // assert symbol extraction succeeds; JSX-specific semantics are out
        // of scope.
        let src = "function App() { return <div>hi</div>; }\n";
        let out = extract_tsx(src, "ui");
        assert_eq!(names(&out), vec!["App"]);
    }

    #[test]
    fn test_typescript_extracts_arrow_method_field_in_class() {
        let src = "class A {\n    handle = (e: Event) => { console.log(e); };\n}\n";
        let out = extract_ts(src, "m");
        let qns = qnames(&out);
        assert!(qns.contains(&"m::A"));
        assert!(qns.contains(&"m::A::handle"));
        assert_eq!(find(&out, "handle").kind, "method");
    }

    #[test]
    fn test_typescript_export_default_function_is_extracted() {
        let src = "export default function defaultFn() {}\n";
        let out = extract_ts(src, "m");
        assert_eq!(names(&out), vec!["defaultFn"]);
        assert_eq!(find(&out, "defaultFn").visibility.as_deref(), Some("export"));
    }

    #[test]
    fn test_typescript_handles_empty_file_without_panicking() {
        let out = extract_ts("", "");
        assert!(out.symbols.is_empty());
        assert!(out.imports.is_empty());
    }

    #[test]
    fn test_typescript_records_type_ref_inside_function() {
        let src = "function f(x: User): void { console.log(x); }\n";
        let out = extract_ts(src, "m");
        assert!(out.refs.iter().any(|r| r.raw_name == "User" && r.kind == "type"));
    }

    #[test]
    fn test_typescript_handles_generic_extends() {
        let src = "class Box<T> extends Container<T> {}\n";
        let out = extract_ts(src, "m");
        assert!(out
            .type_relations
            .iter()
            .any(|t| t.relation == "extends" && t.target_raw_name == "Container"));
    }
}
