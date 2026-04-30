//! Go language extractor. Walks a tree-sitter-go AST and emits structured
//! [`ExtractedSymbol`] records, plus refs, calls, imports, and type relations.
//!
//! Conventions:
//!   - **Module path** is the **package name** declared in `package_clause`,
//!     not the file path. Go's namespace is the package, and files in the
//!     same directory must share one package — so the package name is the
//!     canonical prefix. Qualified names join with `::` (cross-language
//!     consistency with Rust/TS/JS).
//!   - **Methods** are functions with a receiver. Their qualified name takes
//!     the receiver type as parent: `pkg::Receiver::MethodName`. Pointer
//!     receivers (`*Session`) and value receivers (`Session`) collapse to the
//!     same parent type.
//!   - **Type relations:** struct embedding becomes `embeds`, interface
//!     embedding becomes `extends`. There is no class hierarchy in Go.
//!
//! v1 scope: top-level functions / methods / structs / interfaces / type
//! aliases / type definitions / imports / call sites / type refs. Not
//! covered: package-level variables/constants (low retrieval value),
//! generics constraint analysis (records the type identifier as a ref but
//! doesn't model the constraint).

use tree_sitter::Node;

use crate::extract::{
    ExtractContext, ExtractedCall, ExtractedFile, ExtractedImport, ExtractedRef, ExtractedSymbol,
    ExtractedTypeRel,
};
use crate::lang::{LanguageExtractor, LanguageId};
use crate::parse::ParsedFile;

pub struct GoExtractor;

impl GoExtractor {
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for GoExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::Go
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_go::language()
    }

    fn extract(&self, parsed: &ParsedFile, ctx: &ExtractContext) -> ExtractedFile {
        let mut out = ExtractedFile::empty(ctx.relative_path, LanguageId::Go);

        // First pass: locate the package_clause and use its name as the
        // module-path prefix. If absent (rare — usually a parse error), fall
        // back to ctx.module_path.
        let pkg_name = find_package_name(parsed.root_node(), parsed.source())
            .unwrap_or_else(|| ctx.module_path.to_string());

        // Second pass: walk top-level declarations.
        let mut cursor = parsed.root_node().walk();
        for child in parsed.root_node().children(&mut cursor) {
            match child.kind() {
                "function_declaration" => emit_function(child, &pkg_name, parsed.source(), &mut out),
                "method_declaration" => emit_method(child, &pkg_name, parsed.source(), &mut out),
                "type_declaration" => emit_type_declaration(child, &pkg_name, parsed.source(), &mut out),
                "import_declaration" => emit_imports(child, parsed.source(), &mut out),
                _ => {}
            }
        }

        out
    }

    /// Go convention: package name is the canonical namespace. The path-based
    /// default is wrong for Go, but [`extract`] re-derives the prefix from
    /// the source's `package_clause` so this method's output is mostly
    /// vestigial. We override it to use the parent directory name as a
    /// reasonable fallback when ctx.module_path leaks through.
    fn module_path_from_relative_path(&self, relative_path: &str) -> String {
        // Take the parent directory's last segment if present; otherwise
        // strip the .go extension from the filename and use that.
        let stripped = relative_path.strip_suffix(".go").unwrap_or(relative_path);
        match stripped.rfind('/') {
            Some(i) => stripped[..i].rsplit('/').next().unwrap_or("").to_string(),
            None => stripped.to_string(),
        }
    }
}

// --- Top-level emitters ---

fn find_package_name(root: Node, source: &[u8]) -> Option<String> {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() != "package_clause" {
            continue;
        }
        let mut sub = child.walk();
        for c in child.children(&mut sub) {
            if c.kind() == "package_identifier" {
                return Some(node_text(c, source));
            }
        }
    }
    None
}

fn emit_function(node: Node, pkg: &str, source: &[u8], out: &mut ExtractedFile) {
    let Some(name_node) = first_child_kind(&node, &["identifier"]) else {
        return;
    };
    let name = node_text(name_node, source);
    let qname = qualify(pkg, None, &name);
    let sig = signature_until_body(&node, source);
    let visibility = if name.chars().next().is_some_and(|c| c.is_uppercase()) {
        Some("exported".to_string())
    } else {
        Some("package-private".to_string())
    };
    out.symbols.push(make_symbol(&qname, &name, "function", &node, source, sig, visibility, None));

    // Collect type refs from parameter and return type lists.
    walk_for_type_refs_in_signature(&node, source, out);

    if let Some(body) = node.child_by_field_name("body").or_else(|| first_child_kind(&node, &["block"])) {
        walk_within_function(body, &qname, source, out);
    }
}

fn emit_method(node: Node, pkg: &str, source: &[u8], out: &mut ExtractedFile) {
    // method_declaration:
    //   func  (first child)
    //   parameter_list (receiver — extract the type identifier)
    //   field_identifier (method name)
    //   parameter_list (params)
    //   <return type>
    //   block
    let receiver_type = extract_receiver_type(&node, source);
    let Some(receiver) = receiver_type else {
        return;
    };
    // Method name is the field_identifier child (not the param list one).
    let mut name_node: Option<Node> = None;
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "field_identifier" {
            name_node = Some(c);
            break;
        }
    }
    let Some(nn) = name_node else {
        return;
    };
    let name = node_text(nn, source);
    let parent_qname = qualify(pkg, None, &receiver);
    let qname = qualify(pkg, Some(&receiver), &name);
    let sig = signature_until_body(&node, source);
    let visibility = if name.chars().next().is_some_and(|c| c.is_uppercase()) {
        Some("exported".to_string())
    } else {
        Some("package-private".to_string())
    };
    out.symbols.push(make_symbol(
        &qname,
        &name,
        "method",
        &node,
        source,
        sig,
        visibility,
        Some(parent_qname),
    ));

    walk_for_type_refs_in_signature(&node, source, out);

    if let Some(body) = node.child_by_field_name("body").or_else(|| first_child_kind(&node, &["block"])) {
        walk_within_function(body, &qname, source, out);
    }
}

/// Pull the receiver type out of the first `parameter_list` child of a
/// method_declaration. Strips one level of `pointer_type`. Returns the bare
/// type identifier (e.g. `Session` for `(s *Session)`).
fn extract_receiver_type(method_node: &Node, source: &[u8]) -> Option<String> {
    // Locate the receiver parameter_list (always first param-list child).
    let mut cursor = method_node.walk();
    let mut pl: Option<Node> = None;
    for c in method_node.children(&mut cursor) {
        if c.kind() == "parameter_list" {
            pl = Some(c);
            break;
        }
    }
    let pl = pl?;

    let mut sub = pl.walk();
    let mut pd: Option<Node> = None;
    for c in pl.children(&mut sub) {
        if c.kind() == "parameter_declaration" {
            pd = Some(c);
            break;
        }
    }
    let pd = pd?;

    // The parameter_declaration's last type-shaped child is the receiver type.
    let mut last_type: Option<Node> = None;
    let mut sub2 = pd.walk();
    for c in pd.children(&mut sub2) {
        match c.kind() {
            "type_identifier" | "qualified_type" | "generic_type" | "pointer_type" => {
                last_type = Some(c);
            }
            _ => {}
        }
    }
    let t = last_type?;

    let inner = if t.kind() == "pointer_type" {
        // Skip the `*` to get the underlying type.
        let mut sub3 = t.walk();
        let mut found: Option<Node> = None;
        for c in t.children(&mut sub3) {
            if c.kind() != "*" {
                found = Some(c);
                break;
            }
        }
        found.unwrap_or(t)
    } else {
        t
    };
    Some(strip_generics(&node_text(inner, source)))
}

fn emit_type_declaration(node: Node, pkg: &str, source: &[u8], out: &mut ExtractedFile) {
    // type_declaration → type_spec (regular) or type_alias (with `=` token).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_spec" => emit_type_spec(child, pkg, source, out),
            "type_alias" => emit_type_alias(child, pkg, source, out),
            _ => {}
        }
    }
}

fn emit_type_spec(node: Node, pkg: &str, source: &[u8], out: &mut ExtractedFile) {
    // type_spec children: type_identifier (the new type's name) followed by
    // the type body (struct_type / interface_type / type_identifier / etc.)
    let mut cursor = node.walk();
    let mut name: Option<String> = None;
    let mut body: Option<Node> = None;
    for c in node.children(&mut cursor) {
        if c.kind() == "type_identifier" && name.is_none() {
            name = Some(node_text(c, source));
        } else if !matches!(c.kind(), "type_identifier" | "type" | "=") && body.is_none() {
            body = Some(c);
        }
    }
    let Some(name) = name else {
        return;
    };
    let qname = qualify(pkg, None, &name);

    let kind = match body.as_ref().map(|b| b.kind()).unwrap_or("") {
        "struct_type" => "struct",
        "interface_type" => "interface",
        _ => "type",
    };
    let sig = node_text(node, source);
    let visibility = if name.chars().next().is_some_and(|c| c.is_uppercase()) {
        Some("exported".to_string())
    } else {
        Some("package-private".to_string())
    };
    out.symbols.push(make_symbol(
        &qname,
        &name,
        kind,
        &node,
        source,
        Some(sig),
        visibility,
        None,
    ));

    if let Some(b) = body {
        if b.kind() == "struct_type" {
            extract_struct_fields(&b, &qname, source, out);
        } else if b.kind() == "interface_type" {
            extract_interface_methods(&b, &qname, pkg, source, out);
        }
    }
}

fn emit_type_alias(node: Node, pkg: &str, source: &[u8], out: &mut ExtractedFile) {
    // type_alias: type_identifier `=` type_identifier
    let mut cursor = node.walk();
    let names: Vec<String> = node
        .children(&mut cursor)
        .filter(|c| c.kind() == "type_identifier")
        .map(|c| node_text(c, source))
        .collect();
    if names.is_empty() {
        return;
    }
    let name = &names[0];
    let qname = qualify(pkg, None, name);
    let sig = node_text(node, source);
    let visibility = if name.chars().next().is_some_and(|c| c.is_uppercase()) {
        Some("exported".to_string())
    } else {
        Some("package-private".to_string())
    };
    out.symbols.push(make_symbol(
        &qname,
        name,
        "type_alias",
        &node,
        source,
        Some(sig),
        visibility,
        None,
    ));
    // Record the alias target as a type-rel.
    if let Some(target) = names.get(1) {
        out.type_relations.push(ExtractedTypeRel {
            symbol_qualified_name: qname,
            relation: "alias_of".to_string(),
            target_raw_name: target.clone(),
            line: (node.start_position().row as u32).saturating_add(1),
        });
    }
}

/// Struct embedding: a `field_declaration` that has only a type_identifier
/// (no field name) is an embedded type. Record it as `embeds`. Named fields
/// just contribute their type identifier to the refs table.
fn extract_struct_fields(node: &Node, owner_qname: &str, source: &[u8], out: &mut ExtractedFile) {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() != "field_declaration_list" {
            continue;
        }
        let mut sub = c.walk();
        for fd in c.children(&mut sub) {
            if fd.kind() != "field_declaration" {
                continue;
            }
            // Count field_identifier (named field) and type_identifier
            // children; structurally an embedded type is just a type_identifier
            // with no preceding field_identifier.
            let mut has_field_ident = false;
            let mut last_type_ident: Option<Node> = None;
            let mut sub2 = fd.walk();
            for fc in fd.children(&mut sub2) {
                match fc.kind() {
                    "field_identifier" => has_field_ident = true,
                    "type_identifier" => last_type_ident = Some(fc),
                    _ => {}
                }
            }
            if !has_field_ident {
                if let Some(t) = last_type_ident {
                    out.type_relations.push(ExtractedTypeRel {
                        symbol_qualified_name: owner_qname.to_string(),
                        relation: "embeds".to_string(),
                        target_raw_name: node_text(t, source),
                        line: (t.start_position().row as u32).saturating_add(1),
                    });
                }
            }
            // Also record any type identifiers as refs for impact analysis.
            walk_for_type_refs(fd, source, out);
        }
    }
}

/// Interface methods: `method_elem` children inside `interface_type`. Each
/// becomes a "method" symbol parented by the interface qname. Embedded
/// interfaces (`type_identifier` direct children) become `extends` relations.
fn extract_interface_methods(
    node: &Node,
    owner_qname: &str,
    pkg: &str,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let receiver_short = owner_qname.rsplit("::").next().unwrap_or("").to_string();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        match c.kind() {
            "method_elem" | "method_spec" => {
                let mut sub = c.walk();
                let mut name_node: Option<Node> = None;
                for n in c.children(&mut sub) {
                    if n.kind() == "field_identifier" {
                        name_node = Some(n);
                        break;
                    }
                }
                if let Some(nn) = name_node {
                    let name = node_text(nn, source);
                    let qname = qualify(pkg, Some(&receiver_short), &name);
                    let sig = node_text(c, source);
                    out.symbols.push(make_symbol(
                        &qname,
                        &name,
                        "method",
                        &c,
                        source,
                        Some(sig),
                        None,
                        Some(owner_qname.to_string()),
                    ));
                }
                walk_for_type_refs(c, source, out);
            }
            // Embedded interface. tree-sitter-go wraps the embedded name in a
            // `type_elem` node; older grammars surfaced it as a direct
            // `type_identifier`/`qualified_type`. Handle both.
            "type_elem" => {
                let mut sub = c.walk();
                let mut target_node: Option<Node> = None;
                for n in c.children(&mut sub) {
                    if matches!(n.kind(), "type_identifier" | "qualified_type") {
                        target_node = Some(n);
                        break;
                    }
                }
                if let Some(t) = target_node {
                    out.type_relations.push(ExtractedTypeRel {
                        symbol_qualified_name: owner_qname.to_string(),
                        relation: "extends".to_string(),
                        target_raw_name: node_text(t, source),
                        line: (t.start_position().row as u32).saturating_add(1),
                    });
                }
            }
            "type_identifier" | "qualified_type" => {
                out.type_relations.push(ExtractedTypeRel {
                    symbol_qualified_name: owner_qname.to_string(),
                    relation: "extends".to_string(),
                    target_raw_name: node_text(c, source),
                    line: (c.start_position().row as u32).saturating_add(1),
                });
            }
            _ => {}
        }
    }
}

fn emit_imports(node: Node, source: &[u8], out: &mut ExtractedFile) {
    // Handle both single-line `import "x"` and grouped `import (...)`. We
    // recurse to find import_spec nodes.
    let mut stack: Vec<Node> = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_spec" {
            let mut alias: Option<String> = None;
            let mut path: Option<String> = None;
            let mut sub = n.walk();
            for c in n.children(&mut sub) {
                match c.kind() {
                    "package_identifier" | "blank_identifier" | "dot" => {
                        alias = Some(node_text(c, source));
                    }
                    "interpreted_string_literal" | "raw_string_literal" => {
                        path = Some(unquote(&node_text(c, source)));
                    }
                    _ => {}
                }
            }
            if let Some(p) = path {
                out.imports.push(ExtractedImport {
                    raw_path: p,
                    alias,
                    line: (n.start_position().row as u32).saturating_add(1),
                });
            }
            continue;
        }
        let mut sub = n.walk();
        for c in n.children(&mut sub) {
            stack.push(c);
        }
    }
}

// --- Body walkers ---

fn walk_within_function(node: Node, caller_qname: &str, source: &[u8], out: &mut ExtractedFile) {
    let mut stack: Vec<Node> = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                let callee_opt = match n.child_by_field_name("function") {
                    Some(f) => Some(f),
                    None => {
                        // Field-name lookup failed; first child is the callee.
                        let mut sub = n.walk();
                        let mut first: Option<Node> = None;
                        for c in n.children(&mut sub) {
                            first = Some(c);
                            break;
                        }
                        first
                    }
                };
                if let Some(callee) = callee_opt {
                    // For `pkg.Func(...)` and `obj.Method(...)`, the
                    // selector_expression's last identifier-like child is
                    // the callee name. For bare `Func(...)`, the identifier
                    // itself is the name.
                    let raw_name = match callee.kind() {
                        "selector_expression" => {
                            // The right side is a field_identifier.
                            let mut sub = callee.walk();
                            callee
                                .children(&mut sub)
                                .filter(|c| matches!(c.kind(), "field_identifier" | "identifier"))
                                .last()
                                .map(|c| node_text(c, source))
                                .unwrap_or_else(|| node_text(callee, source))
                        }
                        _ => node_text(callee, source),
                    };
                    let line = (n.start_position().row as u32).saturating_add(1);
                    let col = n.start_position().column as u32;
                    out.calls.push(ExtractedCall {
                        caller_qualified_name: caller_qname.to_string(),
                        callee_raw_name: raw_name.clone(),
                        line,
                        col,
                    });
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
            "type_identifier" => {
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
        let mut sub = n.walk();
        for c in n.children(&mut sub) {
            stack.push(c);
        }
    }
}

/// Scan a function/method declaration's signature region (parameters +
/// return type) for type identifiers, recording each as a ref. Skips the
/// body block — that's handled by [`walk_within_function`].
fn walk_for_type_refs_in_signature(decl: &Node, source: &[u8], out: &mut ExtractedFile) {
    let mut cursor = decl.walk();
    for child in decl.children(&mut cursor) {
        if child.kind() == "block" {
            continue;
        }
        walk_for_type_refs(child, source, out);
    }
}

fn walk_for_type_refs(node: Node, source: &[u8], out: &mut ExtractedFile) {
    let mut stack: Vec<Node> = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" {
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
        let mut sub = n.walk();
        for c in n.children(&mut sub) {
            stack.push(c);
        }
    }
}

// --- Helpers ---

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

/// Walk the prev_sibling chain harvesting Go doc comments. Go convention is
/// `// ...` lines (or block `/* */`) immediately preceding the declaration;
/// godoc treats these as documentation. Returns `None` when nothing was
/// attached. Multi-line `//` blocks are joined with `\n`.
fn extract_doc_comment(node: &Node, source: &[u8]) -> Option<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut cursor = node.prev_sibling();
    while let Some(sib) = cursor {
        if sib.kind() != "comment" {
            break;
        }
        let txt = node_text(sib, source);
        let trimmed = txt.trim();
        if let Some(rest) = trimmed.strip_prefix("//") {
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

fn qualify(pkg: &str, parent: Option<&str>, name: &str) -> String {
    let prefix = match parent {
        Some(p) if !pkg.is_empty() => format!("{pkg}::{p}"),
        Some(p) => p.to_string(),
        None => pkg.to_string(),
    };
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}

fn first_child_kind<'tree>(node: &Node<'tree>, kinds: &[&str]) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    let mut found: Option<Node<'tree>> = None;
    for c in node.children(&mut cursor) {
        if kinds.contains(&c.kind()) {
            found = Some(c);
            break;
        }
    }
    found
}

fn node_text(node: Node, source: &[u8]) -> String {
    String::from_utf8_lossy(&source[node.byte_range()]).to_string()
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

fn signature_until_body(node: &Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let mut body: Option<Node> = None;
    for c in node.children(&mut cursor) {
        if c.kind() == "block" {
            body = Some(c);
            break;
        }
    }
    let body = body?;
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
        || (trimmed.starts_with('`') && trimmed.ends_with('`'))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Strip generic type parameter brackets — `Box[T]` → `Box`. Keeps qualified
/// names stable when generics are present.
fn strip_generics(s: &str) -> String {
    match s.find('[') {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ExtractContext;
    use crate::parse;

    fn extract_go(src: &str) -> ExtractedFile {
        let ext = GoExtractor::new();
        let parsed = parse(src.as_bytes().to_vec(), &ext).expect("parse");
        let ctx = ExtractContext { relative_path: "test.go", module_path: "test" };
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
    fn test_go_uses_package_clause_as_module_prefix() {
        let src = "package auth\n\nfunc Login() {}\n";
        let out = extract_go(src);
        assert_eq!(qnames(&out), vec!["auth::Login"]);
    }

    #[test]
    fn test_go_extracts_top_level_function_with_signature() {
        let src = "package m\n\nfunc Hello(name string) string { return name }\n";
        let out = extract_go(src);
        let f = find(&out, "Hello");
        assert_eq!(f.kind, "function");
        assert!(f.signature.as_deref().unwrap_or("").contains("func Hello"));
    }

    #[test]
    fn test_go_classifies_exported_vs_package_private_visibility() {
        let src = "package m\n\nfunc Public() {}\nfunc internal() {}\n";
        let out = extract_go(src);
        assert_eq!(find(&out, "Public").visibility.as_deref(), Some("exported"));
        assert_eq!(find(&out, "internal").visibility.as_deref(), Some("package-private"));
    }

    #[test]
    fn test_go_extracts_struct_with_fields_recorded_as_type_refs() {
        let src = "package store\n\ntype Session struct {\n    UserID  string\n    Expires int64\n}\n";
        let out = extract_go(src);
        let s = find(&out, "Session");
        assert_eq!(s.kind, "struct");
        assert_eq!(s.qualified_name, "store::Session");
        // Field types should appear as type refs.
        let raw_refs: Vec<&str> = out.refs.iter().map(|r| r.raw_name.as_str()).collect();
        assert!(raw_refs.contains(&"string"));
        assert!(raw_refs.contains(&"int64"));
    }

    #[test]
    fn test_go_method_uses_receiver_type_as_parent_qname() {
        let src = "package m\n\ntype S struct{}\n\nfunc (s *S) Run() {}\n";
        let out = extract_go(src);
        let qns = qnames(&out);
        assert!(qns.contains(&"m::S"));
        assert!(qns.contains(&"m::S::Run"));
        let run = find(&out, "Run");
        assert_eq!(run.kind, "method");
        assert_eq!(run.parent_qualified_name.as_deref(), Some("m::S"));
    }

    #[test]
    fn test_go_method_pointer_and_value_receivers_collapse_to_same_parent() {
        let src = "package m\n\ntype S struct{}\nfunc (s *S) A() {}\nfunc (s S) B() {}\n";
        let out = extract_go(src);
        let qns = qnames(&out);
        assert!(qns.contains(&"m::S::A"));
        assert!(qns.contains(&"m::S::B"));
    }

    #[test]
    fn test_go_extracts_interface_with_method_signatures() {
        let src = "package iface\n\ntype Logger interface {\n    Info(msg string)\n    Error(msg string) error\n}\n";
        let out = extract_go(src);
        let qns = qnames(&out);
        assert!(qns.contains(&"iface::Logger"));
        assert!(qns.contains(&"iface::Logger::Info"));
        assert!(qns.contains(&"iface::Logger::Error"));
    }

    #[test]
    fn test_go_extracts_type_alias_and_records_target() {
        let src = "package t\n\ntype UserID = string\n";
        let out = extract_go(src);
        let s = find(&out, "UserID");
        assert_eq!(s.kind, "type_alias");
        assert!(out
            .type_relations
            .iter()
            .any(|t| t.relation == "alias_of"
                && t.target_raw_name == "string"
                && t.symbol_qualified_name == "t::UserID"));
    }

    #[test]
    fn test_go_extracts_named_type_definition() {
        let src = "package t\n\ntype Count int\n";
        let out = extract_go(src);
        let s = find(&out, "Count");
        assert_eq!(s.kind, "type");
    }

    #[test]
    fn test_go_records_struct_embedding_as_embeds_relation() {
        let src = "package m\n\ntype Base struct{}\ntype Derived struct {\n    Base\n    name string\n}\n";
        let out = extract_go(src);
        assert!(out.type_relations.iter().any(|t|
            t.relation == "embeds"
            && t.target_raw_name == "Base"
            && t.symbol_qualified_name == "m::Derived"));
    }

    #[test]
    fn test_go_records_interface_embedding_as_extends_relation() {
        let src = "package m\n\ntype A interface { Foo() }\ntype B interface {\n    A\n    Bar() error\n}\n";
        let out = extract_go(src);
        assert!(out.type_relations.iter().any(|t|
            t.relation == "extends"
            && t.target_raw_name == "A"
            && t.symbol_qualified_name == "m::B"));
    }

    #[test]
    fn test_go_extracts_grouped_imports() {
        let src = "package m\n\nimport (\n    \"crypto/sha256\"\n    log \"log/slog\"\n)\n";
        let out = extract_go(src);
        let raw_paths: Vec<&str> = out.imports.iter().map(|i| i.raw_path.as_str()).collect();
        assert!(raw_paths.contains(&"crypto/sha256"));
        assert!(raw_paths.contains(&"log/slog"));
        assert!(out
            .imports
            .iter()
            .any(|i| i.raw_path == "log/slog" && i.alias.as_deref() == Some("log")));
    }

    #[test]
    fn test_go_extracts_single_import_without_alias() {
        let src = "package m\n\nimport \"fmt\"\n";
        let out = extract_go(src);
        assert_eq!(out.imports.len(), 1);
        assert_eq!(out.imports[0].raw_path, "fmt");
        assert!(out.imports[0].alias.is_none());
    }

    #[test]
    fn test_go_extracts_blank_and_dot_imports() {
        let src = "package m\n\nimport (\n    _ \"pgx/stdlib\"\n    . \"strings\"\n)\n";
        let out = extract_go(src);
        let aliases: Vec<&str> = out.imports.iter().filter_map(|i| i.alias.as_deref()).collect();
        assert!(aliases.contains(&"_"));
        assert!(aliases.contains(&"."));
    }

    #[test]
    fn test_go_records_call_inside_function_body() {
        let src = "package m\n\nfunc helper() {}\nfunc main() { helper() }\n";
        let out = extract_go(src);
        assert!(out
            .calls
            .iter()
            .any(|c| c.caller_qualified_name == "m::main" && c.callee_raw_name == "helper"));
    }

    #[test]
    fn test_go_records_method_call_with_unqualified_callee_name() {
        let src = "package m\n\nfunc main() { logger.Info(\"hi\") }\n";
        let out = extract_go(src);
        assert!(out
            .calls
            .iter()
            .any(|c| c.callee_raw_name == "Info"));
    }

    #[test]
    fn test_go_records_call_in_method_body_with_method_qname_as_caller() {
        let src = "package m\n\ntype S struct{}\n\nfunc (s *S) Run() { helper() }\n\nfunc helper() {}\n";
        let out = extract_go(src);
        assert!(out
            .calls
            .iter()
            .any(|c| c.caller_qualified_name == "m::S::Run" && c.callee_raw_name == "helper"));
    }

    #[test]
    fn test_go_strips_generics_from_method_receiver_type() {
        let src = "package m\n\ntype Box[T any] struct { v T }\n\nfunc (b *Box[T]) Get() T { return b.v }\n";
        let out = extract_go(src);
        // The method should be parented by `m::Box`, not `m::Box[T]`.
        assert!(qnames(&out).contains(&"m::Box::Get"));
    }

    #[test]
    fn test_go_handles_empty_file_without_panicking() {
        let out = extract_go("package empty\n");
        assert!(out.symbols.is_empty());
        assert!(out.calls.is_empty());
    }

    #[test]
    fn test_go_records_parameter_type_as_ref() {
        let src = "package m\n\nfunc f(u User) {}\n";
        let out = extract_go(src);
        assert!(out.refs.iter().any(|r| r.raw_name == "User" && r.kind == "type"));
    }

    #[test]
    fn test_go_module_path_uses_parent_dir_name_default() {
        let ext = GoExtractor::new();
        assert_eq!(ext.module_path_from_relative_path("internal/auth/session.go"), "auth");
        assert_eq!(ext.module_path_from_relative_path("main.go"), "main");
    }
}
