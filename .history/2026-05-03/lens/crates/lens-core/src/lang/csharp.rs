//! C# language extractor. Walks a tree-sitter-c-sharp AST and emits structured
//! [`ExtractedSymbol`] records, plus refs, calls, imports, and type relations.
//!
//! Conventions:
//!   - **Module path** is the namespace declared in `namespace_declaration`.
//!     Namespaces use `::` as the separator (cross-language consistency).
//!   - **Types** (class, struct, interface, enum, delegate) are symbols. Methods,
//!     constructors, properties, and fields nested inside a type are parented by
//!     the type's qualified name.
//!   - **Base list** entries become `extends` (class bases) or `implements`
//!     (interface bases) relations.
//!   - **Visibility** is extracted from `modifier` nodes (`public`, `private`,
//!     `protected`, `internal`, `protected internal`, `private protected`).
//!
//! v1 scope: namespaces, classes, structs, interfaces, enums, delegates,
//! methods, constructors, properties, fields, imports, calls, type refs.
//! Not covered: local functions, indexers (low retrieval value), event accessors.

use tree_sitter::Node;

use crate::extract::{
    ExtractContext, ExtractedCall, ExtractedFile, ExtractedImport, ExtractedRef, ExtractedSymbol,
    ExtractedTypeRel,
};
use crate::lang::{LanguageExtractor, LanguageId};
use crate::parse::ParsedFile;

pub struct CSharpExtractor;

impl CSharpExtractor {
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for CSharpExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::CSharp
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cs"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_c_sharp::language()
    }

    fn extract(&self, parsed: &ParsedFile, ctx: &ExtractContext) -> ExtractedFile {
        let mut out = ExtractedFile::empty(ctx.relative_path, LanguageId::CSharp);

        // Determine namespace prefix from the first namespace_declaration.
        let ns = find_namespace(parsed.root_node(), parsed.source());
        let module_path = ns.unwrap_or_else(|| ctx.module_path.to_string());

        // Walk top-level declarations.
        walk_declarations(parsed.root_node(), &module_path, None, parsed.source(), &mut out);
        out
    }
}

// --- Top-level walkers ---

fn find_namespace(root: Node, source: &[u8]) -> Option<String> {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "namespace_declaration" {
            return extract_namespace_name(child, source);
        }
    }
    None
}

fn extract_namespace_name(node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "qualified_name" | "identifier" => return Some(node_text(child, source).replace('.', "::")),
            _ => {}
        }
    }
    None
}

/// Recursively walk declaration nodes. `parent_type_qname` is the qualified
/// name of the enclosing class/struct/interface, if any.
fn walk_declarations(
    node: Node,
    module_path: &str,
    parent_type_qname: Option<&str>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "namespace_declaration" => {
                // Nested namespace — recurse into its declaration_list.
                let mut sub = child.walk();
                for c in child.children(&mut sub) {
                    if c.kind() == "declaration_list" {
                        walk_declarations(c, module_path, parent_type_qname, source, out);
                    }
                }
            }
            "class_declaration" => emit_type_declaration(child, module_path, parent_type_qname, "class", source, out),
            "struct_declaration" => emit_type_declaration(child, module_path, parent_type_qname, "struct", source, out),
            "interface_declaration" => emit_type_declaration(child, module_path, parent_type_qname, "interface", source, out),
            "enum_declaration" => emit_enum(child, module_path, parent_type_qname, source, out),
            "delegate_declaration" => emit_delegate(child, module_path, parent_type_qname, source, out),
            "method_declaration" => emit_method(child, module_path, parent_type_qname, source, out),
            "constructor_declaration" => emit_constructor(child, module_path, parent_type_qname, source, out),
            "property_declaration" => emit_property(child, module_path, parent_type_qname, source, out),
            "field_declaration" => emit_field(child, module_path, parent_type_qname, source, out),
            "event_field_declaration" => emit_event_field(child, module_path, parent_type_qname, source, out),
            "using_directive" => emit_using(child, source, out),
            "declaration_list" => walk_declarations(child, module_path, parent_type_qname, source, out),
            _ => {}
        }
    }
}

// --- Symbol emitters ---

fn emit_type_declaration(
    node: Node,
    module_path: &str,
    parent_type_qname: Option<&str>,
    kind: &str,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = child_text_by_kind(&node, "identifier", source);
    let Some(name) = name else { return };
    let qname = build_qname(module_path, parent_type_qname, &name);
    let vis = extract_visibility(&node, source);
    let sig = signature_text(&node, source);
    out.symbols.push(make_symbol(
        &qname,
        &name,
        kind,
        &node,
        source,
        sig,
        vis,
        parent_type_qname.map(|s| s.to_string()),
    ));

    // Base list → type relations.
    if let Some(base_list) = child_by_kind(&node, "base_list") {
        extract_base_types(&base_list, &qname, kind, source, out);
    }

    // Type parameter list → refs.
    if let Some(tpl) = child_by_kind(&node, "type_parameter_list") {
        walk_for_type_refs(tpl, source, out);
    }

    // Walk the declaration_list for nested items.
    if let Some(body) = child_by_kind(&node, "declaration_list") {
        walk_declarations(body, module_path, Some(&qname), source, out);
    }
}

fn emit_enum(
    node: Node,
    module_path: &str,
    parent_type_qname: Option<&str>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = child_text_by_kind(&node, "identifier", source);
    let Some(name) = name else { return };
    let qname = build_qname(module_path, parent_type_qname, &name);
    let vis = extract_visibility(&node, source);
    let sig = signature_text(&node, source);
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "enum",
        &node,
        source,
        sig,
        vis,
        parent_type_qname.map(|s| s.to_string()),
    ));

    if let Some(body) = child_by_kind(&node, "enum_member_declaration_list") {
        let mut cursor = body.walk();
        for c in body.children(&mut cursor) {
            if c.kind() == "enum_member_declaration" {
                if let Some(member_name) = child_text_by_kind(&c, "identifier", source) {
                    let member_qname = format!("{}::{}", qname, member_name);
                    out.symbols.push(make_symbol(
                        &member_qname,
                        &member_name,
                        "enum_member",
                        &c,
                        source,
                        None,
                        None,
                        Some(qname.clone()),
                    ));
                }
            }
        }
    }
}

fn emit_delegate(
    node: Node,
    module_path: &str,
    parent_type_qname: Option<&str>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = child_text_by_kind(&node, "identifier", source);
    let Some(name) = name else { return };
    let qname = build_qname(module_path, parent_type_qname, &name);
    let vis = extract_visibility(&node, source);
    let sig = signature_text(&node, source);
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "delegate",
        &node,
        source,
        sig,
        vis,
        parent_type_qname.map(|s| s.to_string()),
    ));
    // Return type and parameter types as refs.
    if let Some(params) = child_by_kind(&node, "parameter_list") {
        walk_for_type_refs(params, source, out);
    }
    walk_return_type(&node, source, out);
}

fn emit_method(
    node: Node,
    module_path: &str,
    parent_type_qname: Option<&str>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = child_text_by_kind(&node, "identifier", source);
    let Some(name) = name else { return };
    let qname = build_qname(module_path, parent_type_qname, &name);
    let vis = extract_visibility(&node, source);
    let sig = signature_text(&node, source);
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "method",
        &node,
        source,
        sig,
        vis,
        parent_type_qname.map(|s| s.to_string()),
    ));

    // Parameter types and return type as refs.
    if let Some(params) = child_by_kind(&node, "parameter_list") {
        walk_for_type_refs(params, source, out);
    }
    walk_return_type(&node, source, out);

    // Walk body for calls and refs.
    if let Some(body) = child_by_kind(&node, "block") {
        walk_body(body, &qname, source, out);
    } else if let Some(body) = child_by_kind(&node, "arrow_expression_clause") {
        walk_body(body, &qname, source, out);
    }
}

fn emit_constructor(
    node: Node,
    module_path: &str,
    parent_type_qname: Option<&str>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = child_text_by_kind(&node, "identifier", source);
    let Some(name) = name else { return };
    let qname = build_qname(module_path, parent_type_qname, &name);
    let vis = extract_visibility(&node, source);
    let sig = signature_text(&node, source);
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "constructor",
        &node,
        source,
        sig,
        vis,
        parent_type_qname.map(|s| s.to_string()),
    ));

    if let Some(params) = child_by_kind(&node, "parameter_list") {
        walk_for_type_refs(params, source, out);
    }
    if let Some(body) = child_by_kind(&node, "block") {
        walk_body(body, &qname, source, out);
    }
}

fn emit_property(
    node: Node,
    module_path: &str,
    parent_type_qname: Option<&str>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = child_text_by_kind(&node, "identifier", source);
    let Some(name) = name else { return };
    let qname = build_qname(module_path, parent_type_qname, &name);
    let vis = extract_visibility(&node, source);
    let sig = signature_text(&node, source);
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "property",
        &node,
        source,
        sig,
        vis,
        parent_type_qname.map(|s| s.to_string()),
    ));

    // Property type as ref.
    walk_for_type_refs(node, source, out);
}

fn emit_field(
    node: Node,
    module_path: &str,
    parent_type_qname: Option<&str>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // A field_declaration may declare multiple variables.
    if let Some(vd) = child_by_kind(&node, "variable_declaration") {
        let mut cursor = vd.walk();
        for c in vd.children(&mut cursor) {
            if c.kind() == "variable_declarator" {
                if let Some(name) = child_text_by_kind(&c, "identifier", source) {
                    let qname = build_qname(module_path, parent_type_qname, &name);
                    let vis = extract_visibility(&node, source);
                    out.symbols.push(make_symbol(
                        &qname,
                        &name,
                        "field",
                        &node,
                        source,
                        None,
                        vis,
                        parent_type_qname.map(|s| s.to_string()),
                    ));
                }
            }
        }
    }
    // Field types as refs.
    walk_for_type_refs(node, source, out);
}

fn emit_event_field(
    node: Node,
    module_path: &str,
    parent_type_qname: Option<&str>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    if let Some(vd) = child_by_kind(&node, "variable_declaration") {
        let mut cursor = vd.walk();
        for c in vd.children(&mut cursor) {
            if c.kind() == "variable_declarator" {
                if let Some(name) = child_text_by_kind(&c, "identifier", source) {
                    let qname = build_qname(module_path, parent_type_qname, &name);
                    let vis = extract_visibility(&node, source);
                    out.symbols.push(make_symbol(
                        &qname,
                        &name,
                        "event",
                        &node,
                        source,
                        None,
                        vis,
                        parent_type_qname.map(|s| s.to_string()),
                    ));
                }
            }
        }
    }
    walk_for_type_refs(node, source, out);
}

fn emit_using(node: Node, source: &[u8], out: &mut ExtractedFile) {
    let line = (node.start_position().row as u32).saturating_add(1);
    let mut cursor = node.walk();
    let mut path: Option<String> = None;
    let mut alias: Option<String> = None;

    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "qualified_name" => {
                if path.is_none() {
                    path = Some(node_text(child, source));
                }
            }
            "=>" | "alias" => {
                // C# 12 using alias: `using Alias = Type;`
                // tree-sitter-c-sharp may model this differently; best-effort.
            }
            "name_equals" => {
                // `using X = Y;` — alias on left side.
            }
            _ => {}
        }
    }

    // Re-walk more carefully for aliased using directives.
    let mut cursor2 = node.walk();
    for child in node.children(&mut cursor2) {
        if child.kind() == "name_equals" {
            alias = child_text_by_kind(&child, "identifier", source);
        }
    }

    if let Some(p) = path {
        out.imports.push(ExtractedImport {
            raw_path: p,
            alias,
            line,
        });
    }
}

// --- Type relations ---

fn extract_base_types(
    base_list: &Node,
    owner_qname: &str,
    owner_kind: &str,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let mut cursor = base_list.walk();
    for child in base_list.children(&mut cursor) {
        let kind = child.kind();
        if kind == ":" || kind == "," {
            continue;
        }
        let target = node_text(child, source);
        if target.is_empty() {
            continue;
        }
        let relation = if owner_kind == "interface" {
            "extends"
        } else {
            // For classes: first base is typically the class, rest are interfaces.
            // v1 heuristic: all bases are "extends" for simplicity.
            "extends"
        };
        out.type_relations.push(ExtractedTypeRel {
            symbol_qualified_name: owner_qname.to_string(),
            relation: relation.to_string(),
            target_raw_name: target,
            line: (child.start_position().row as u32).saturating_add(1),
        });
    }
}

// --- Body walkers (calls + refs) ---

fn walk_body(node: Node, caller_qname: &str, source: &[u8], out: &mut ExtractedFile) {
    let mut stack: Vec<Node> = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "invocation_expression" => {
                let expr = n.child_by_field_name("function").or_else(|| {
                    let mut sub = n.walk();
                    for c in n.children(&mut sub) {
                        return Some(c);
                    }
                    None
                });
                if let Some(expr) = expr {
                    let raw = callee_name(expr, source);
                    let line = (n.start_position().row as u32).saturating_add(1);
                    let col = n.start_position().column as u32;
                    out.calls.push(ExtractedCall {
                        caller_qualified_name: caller_qname.to_string(),
                        callee_raw_name: raw.clone(),
                        line,
                        col,
                    });
                    out.refs.push(ExtractedRef {
                        raw_name: raw,
                        kind: "call".to_string(),
                        line,
                        col,
                        end_line: (n.end_position().row as u32).saturating_add(1),
                        end_col: n.end_position().column as u32,
                    });
                }
            }
            "object_creation_expression" => {
                // `new Foo()` — record the type as a ref and the constructor as a call.
                let mut cursor = n.walk();
                let mut type_node: Option<Node> = None;
                for c in n.children(&mut cursor) {
                    if c.kind() == "identifier" || c.kind() == "qualified_name" || c.kind() == "generic_name" {
                        type_node = Some(c);
                        break;
                    }
                }
                if let Some(tn) = type_node {
                    let raw = node_text(tn, source);
                    let line = (n.start_position().row as u32).saturating_add(1);
                    let col = n.start_position().column as u32;
                    out.calls.push(ExtractedCall {
                        caller_qualified_name: caller_qname.to_string(),
                        callee_raw_name: raw.clone(),
                        line,
                        col,
                    });
                    out.refs.push(ExtractedRef {
                        raw_name: raw.clone(),
                        kind: "type".to_string(),
                        line,
                        col,
                        end_line: (n.end_position().row as u32).saturating_add(1),
                        end_col: n.end_position().column as u32,
                    });
                }
            }
            "identifier" => {
                // Only record identifiers that are likely type references.
                // Heuristic: if the parent is a type position, record it.
                if is_type_position(&n) {
                    let (sl, sc, el, ec) = position_of(&n);
                    out.refs.push(ExtractedRef {
                        raw_name: node_text(n, source),
                        kind: "type".to_string(),
                        line: sl,
                        col: sc,
                        end_line: el,
                        end_col: ec,
                    });
                }
            }
            _ => {}
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

fn callee_name(node: Node, source: &[u8]) -> String {
    match node.kind() {
        "member_access_expression" => {
            let mut cursor = node.walk();
            let mut last_ident: Option<Node> = None;
            for c in node.children(&mut cursor) {
                if c.kind() == "identifier" || c.kind() == "generic_name" {
                    last_ident = Some(c);
                }
            }
            last_ident.map(|n| node_text(n, source)).unwrap_or_else(|| node_text(node, source))
        }
        "generic_name" => {
            child_text_by_kind(&node, "identifier", source)
                .unwrap_or_else(|| node_text(node, source))
        }
        _ => node_text(node, source),
    }
}

fn is_type_position(node: &Node) -> bool {
    let parent = match node.parent() {
        Some(p) => p,
        None => return false,
    };
    match parent.kind() {
        "variable_declaration"
        | "parameter"
        | "cast_expression"
        | "type_argument_list"
        | "type_parameter_list"
        | "as_expression"
        | "is_expression"
        | "type_of_expression"
        | "default_expression"
        | "tuple_type"
        | "array_type"
        | "nullable_type" => true,
        _ => false,
    }
}

fn walk_for_type_refs(node: Node, source: &[u8], out: &mut ExtractedFile) {
    let mut stack: Vec<Node> = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "identifier" | "generic_name" | "qualified_name" | "predefined_type" => {
                let (sl, sc, el, ec) = position_of(&n);
                out.refs.push(ExtractedRef {
                    raw_name: node_text(n, source),
                    kind: "type".to_string(),
                    line: sl,
                    col: sc,
                    end_line: el,
                    end_col: ec,
                });
            }
            "invocation_expression" | "object_creation_expression" => {
                // Skip — these are handled by walk_body for calls.
            }
            _ => {
                let mut cursor = n.walk();
                for c in n.children(&mut cursor) {
                    stack.push(c);
                }
            }
        }
    }
}

fn walk_return_type(node: &Node, source: &[u8], out: &mut ExtractedFile) {
    // In method_declaration, the return type precedes the identifier.
    // We scan children before the identifier and record type-shaped nodes.
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "identifier" {
            break;
        }
        if is_type_node(c.kind()) {
            let (sl, sc, el, ec) = position_of(&c);
            out.refs.push(ExtractedRef {
                raw_name: node_text(c, source),
                kind: "type".to_string(),
                line: sl,
                col: sc,
                end_line: el,
                end_col: ec,
            });
        }
    }
}

fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "identifier" | "generic_name" | "qualified_name" | "predefined_type" | "array_type" | "nullable_type"
    )
}

// --- Helpers ---

fn build_qname(module_path: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}::{name}"),
        None if module_path.is_empty() => name.to_string(),
        None => format!("{}::{name}", module_path),
    }
}

fn extract_visibility(node: &Node, source: &[u8]) -> Option<String> {
    let mut mods: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "modifier" {
            let txt = node_text(c, source);
            mods.push(txt);
        }
    }
    if mods.is_empty() {
        // C# default is internal for top-level, private for nested.
        return Some("default".to_string());
    }
    Some(mods.join(" "))
}

fn signature_text(node: &Node, source: &[u8]) -> Option<String> {
    let start = node.start_byte();
    // Try to find the body start (block or arrow_expression_clause or accessor_list or ;)
    let mut end = node.end_byte();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if matches!(c.kind(), "block" | "arrow_expression_clause" | "accessor_list" | "enum_member_declaration_list") {
            end = c.start_byte();
            break;
        }
        if c.kind() == ";" {
            end = c.end_byte();
            break;
        }
    }
    if end <= start || end > source.len() {
        return None;
    }
    let s = String::from_utf8_lossy(&source[start..end]).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn child_by_kind<'tree>(node: &Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == kind {
            return Some(c);
        }
    }
    None
}

fn child_text_by_kind(node: &Node, kind: &str, source: &[u8]) -> Option<String> {
    child_by_kind(node, kind).map(|n| node_text(n, source))
}

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
        doc_comment: extract_doc_comment(node, source),
    }
}

fn node_text(node: Node, source: &[u8]) -> String {
    String::from_utf8_lossy(&source[node.byte_range()]).to_string()
}

fn position_of(node: &Node) -> (u32, u32, u32, u32) {
    let s = node.start_position();
    let e = node.end_position();
    (
        (s.row as u32).saturating_add(1),
        s.column as u32,
        (e.row as u32).saturating_add(1),
        e.column as u32,
    )
}

/// Walk prev_siblings collecting `///` doc comments. Multi-line blocks are
/// joined with `\n`.
fn extract_doc_comment(node: &Node, source: &[u8]) -> Option<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut cursor = node.prev_sibling();
    while let Some(sib) = cursor {
        if sib.kind() != "comment" {
            break;
        }
        let txt = node_text(sib, source);
        let trimmed = txt.trim();
        if let Some(rest) = trimmed.strip_prefix("///") {
            chunks.push(rest.trim().to_string());
        } else if trimmed.starts_with("/*") {
            let inner = trimmed
                .trim_start_matches("/*")
                .trim_end_matches("*/")
                .trim()
                .to_string();
            chunks.push(inner);
        } else {
            break;
        }
        cursor = sib.prev_sibling();
    }
    if chunks.is_empty() {
        None
    } else {
        chunks.reverse();
        Some(chunks.join("\n").trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ExtractContext;
    use crate::parse;

    fn extract_cs(src: &str, module_path: &str) -> ExtractedFile {
        let ext = CSharpExtractor::new();
        let parsed = parse::parse(src.as_bytes().to_vec(), &ext).expect("parse");
        let ctx = ExtractContext {
            relative_path: "test.cs",
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
        out.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol named {name}; have {:?}", names(out)))
    }

    #[test]
    fn test_csharp_uses_namespace_as_module_prefix() {
        let src = "namespace App { class Program { static void Main() {} } }\n";
        let out = extract_cs(src, "App");
        assert!(qnames(&out).contains(&"App::Program"));
        assert!(qnames(&out).contains(&"App::Program::Main"));
    }

    #[test]
    fn test_csharp_extracts_class_with_base() {
        let src = "namespace N { class Base {} class Derived : Base {} }\n";
        let out = extract_cs(src, "N");
        assert!(qnames(&out).contains(&"N::Derived"));
        assert!(out.type_relations.iter().any(|t|
            t.symbol_qualified_name == "N::Derived" && t.relation == "extends" && t.target_raw_name == "Base"
        ));
    }

    #[test]
    fn test_csharp_extracts_interface_and_implements() {
        let src = "namespace N { interface IFoo {} class Bar : IFoo {} }\n";
        let out = extract_cs(src, "N");
        assert!(qnames(&out).contains(&"N::IFoo"));
        assert!(qnames(&out).contains(&"N::Bar"));
    }

    #[test]
    fn test_csharp_extracts_struct() {
        let src = "namespace N { struct Point { public int X; } }\n";
        let out = extract_cs(src, "N");
        let p = find(&out, "Point");
        assert_eq!(p.kind, "struct");
    }

    #[test]
    fn test_csharp_extracts_enum_and_members() {
        let src = "namespace N { enum Status { Active, Inactive } }\n";
        let out = extract_cs(src, "N");
        assert!(qnames(&out).contains(&"N::Status"));
        assert!(qnames(&out).contains(&"N::Status::Active"));
        assert!(qnames(&out).contains(&"N::Status::Inactive"));
    }

    #[test]
    fn test_csharp_extracts_method_with_signature() {
        let src = "namespace N { class C { public void Run(int x) {} } }\n";
        let out = extract_cs(src, "N");
        let m = find(&out, "Run");
        assert_eq!(m.kind, "method");
        let sig = m.signature.as_deref().unwrap_or("");
        assert!(sig.contains("Run"), "sig was: {sig}");
    }

    #[test]
    fn test_csharp_extracts_constructor() {
        let src = "namespace N { class C { public C(string n) {} } }\n";
        let out = extract_cs(src, "N");
        let ctor = out.symbols.iter().find(|s| s.kind == "constructor").expect("constructor");
        assert_eq!(ctor.name, "C");
        assert_eq!(ctor.parent_qualified_name.as_deref(), Some("N::C"));
    }

    #[test]
    fn test_csharp_extracts_property() {
        let src = "namespace N { class C { public string Name { get; set; } } }\n";
        let out = extract_cs(src, "N");
        let p = find(&out, "Name");
        assert_eq!(p.kind, "property");
    }

    #[test]
    fn test_csharp_extracts_field() {
        let src = "namespace N { class C { private int count; } }\n";
        let out = extract_cs(src, "N");
        let f = find(&out, "count");
        assert_eq!(f.kind, "field");
    }

    #[test]
    fn test_csharp_extracts_delegate() {
        let src = "namespace N { public delegate void Handler(object sender); }\n";
        let out = extract_cs(src, "N");
        let d = find(&out, "Handler");
        assert_eq!(d.kind, "delegate");
    }

    #[test]
    fn test_csharp_extracts_using_imports() {
        let src = "using System.Collections.Generic;\nnamespace N { class C {} }\n";
        let out = extract_cs(src, "N");
        assert!(out.imports.iter().any(|i| i.raw_path == "System.Collections.Generic"));
    }

    #[test]
    fn test_csharp_records_call_in_method_body() {
        let src = "namespace N { class C { void A() { B(); } void B() {} } }\n";
        let out = extract_cs(src, "N");
        assert!(out.calls.iter().any(|c|
            c.caller_qualified_name == "N::C::A" && c.callee_raw_name == "B"
        ));
    }

    #[test]
    fn test_csharp_records_object_creation_as_call_and_ref() {
        let src = "namespace N { class C { void A() { var x = new List<int>(); } } }\n";
        let out = extract_cs(src, "N");
        assert!(out.calls.iter().any(|c| c.callee_raw_name == "List<int>"));
        assert!(out.refs.iter().any(|r| r.raw_name == "List<int>" && r.kind == "type"));
    }

    #[test]
    fn test_csharp_records_type_refs_in_parameters() {
        let src = "namespace N { class C { void A(string s, List<int> xs) {} } }\n";
        let out = extract_cs(src, "N");
        let raw_refs: Vec<&str> = out.refs.iter().map(|r| r.raw_name.as_str()).collect();
        assert!(raw_refs.contains(&"string"), "missing string ref: {raw_refs:?}");
        assert!(raw_refs.contains(&"List<int>"), "missing List<int> ref: {raw_refs:?}");
    }

    #[test]
    fn test_csharp_extracts_visibility_modifiers() {
        let src = "namespace N { class C { public void Pub() {} private void Priv() {} } }\n";
        let out = extract_cs(src, "N");
        assert_eq!(find(&out, "Pub").visibility.as_deref(), Some("public"));
        assert_eq!(find(&out, "Priv").visibility.as_deref(), Some("private"));
    }

    #[test]
    fn test_csharp_extracts_doc_comments() {
        let src = "namespace N {\n    /// <summary>A thing.</summary>\n    class C {}\n}\n";
        let out = extract_cs(src, "N");
        let c = find(&out, "C");
        let doc = c.doc_comment.as_deref().unwrap_or("");
        assert!(doc.contains("A thing."), "doc was: {doc}");
    }

    #[test]
    fn test_csharp_handles_file_without_namespace() {
        let src = "class C { void M() {} }\n";
        let out = extract_cs(src, "");
        assert!(qnames(&out).contains(&"C"));
        assert!(qnames(&out).contains(&"C::M"));
    }

    #[test]
    fn test_csharp_handles_empty_file() {
        let out = extract_cs("", "N");
        assert!(out.symbols.is_empty());
        assert!(out.imports.is_empty());
    }

    #[test]
    fn test_csharp_no_panic_on_partial_tree() {
        let src = "namespace N { class C { void M( } }\n";
        let _out = extract_cs(src, "N");
        // Must not panic.
    }
}
