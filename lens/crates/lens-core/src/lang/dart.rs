//! Dart language extractor. Walks a tree-sitter-dart AST and emits structured
//! [`ExtractedSymbol`] records, plus refs, imports, and type relations.
//!
//! Conventions:
//!   - **Module path** is the file path with `.dart` stripped and `/` replaced
//!     by `::` (cross-language consistency with Rust/TS/JS/Go).
//!   - **Qualified names** join module path, class/mixin/enum name, and member
//!     name with `::`. Top-level functions get the module path as their
//!     namespace — e.g. `lib::main::main`.
//!   - **Methods** use the enclosing class as parent: `pkg::ClassName::methodName`.
//!     Getters/setters/constructors/operators follow the same pattern.
//!   - **Doc comments** are `documentation_comment` nodes (`///` or `/** */`).
//!     They appear as siblings of `lambda_expression` at the top level and as
//!     siblings of `method_signature` / `declaration` inside class bodies.
//!
//! v1 scope: top-level functions, classes, methods, getters, setters,
//! constructors, mixins, enums (with constants), extensions, type aliases
//! (typedefs), imports/exports, plus type-ref collection in signatures.
//! Not covered: call-site extraction (complex expression AST), generic
//! constraint modeling, extension methods on third-party types, extension
//! types (tree-sitter-dart v0.0.4 does not parse them).

use tree_sitter::Node;

use crate::extract::{
    ExtractContext, ExtractedCall, ExtractedFile, ExtractedImport, ExtractedRef, ExtractedSymbol,
    ExtractedTypeRel,
};
use crate::lang::{LanguageExtractor, LanguageId};
use crate::parse::ParsedFile;

pub struct DartExtractor;

impl DartExtractor {
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for DartExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::Dart
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["dart"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_dart::language()
    }

    fn extract(&self, parsed: &ParsedFile, ctx: &ExtractContext) -> ExtractedFile {
        let mut out = ExtractedFile::empty(ctx.relative_path, LanguageId::Dart);

        // Dart files may declare a library name. If present, use it as the
        // namespace prefix; otherwise fall back to the path-based module_path.
        let prefix = find_library_name(parsed.root_node(), parsed.source())
            .unwrap_or_else(|| ctx.module_path.to_string());

        // Collect documentation_comment siblings so we can attach them to the
        // declaration that follows.
        let mut pending_doc: Option<String> = None;

        let mut cursor = parsed.root_node().walk();
        for child in parsed.root_node().children(&mut cursor) {
            match child.kind() {
                "lambda_expression" => {
                    let doc = pending_doc.take();
                    emit_lambda(child, &prefix, None, doc, parsed.source(), &mut out);
                }
                "class_definition" => {
                    let doc = pending_doc.take();
                    emit_class(child, &prefix, doc, parsed.source(), &mut out);
                }
                "mixin_declaration" => {
                    let doc = pending_doc.take();
                    emit_mixin(child, &prefix, doc, parsed.source(), &mut out);
                }
                "enum_declaration" => {
                    let doc = pending_doc.take();
                    emit_enum(child, &prefix, doc, parsed.source(), &mut out);
                }
                "extension_declaration" => {
                    let doc = pending_doc.take();
                    emit_extension(child, &prefix, doc, parsed.source(), &mut out);
                }
                "type_alias" => {
                    let doc = pending_doc.take();
                    emit_type_alias(child, &prefix, doc, parsed.source(), &mut out);
                }
                "import_or_export" => {
                    emit_import_or_export(child, parsed.source(), &mut out);
                }
                "local_variable_declaration" => {
                    let doc = pending_doc.take();
                    emit_top_level_variable(child, &prefix, doc, parsed.source(), &mut out);
                }
                "documentation_comment" => {
                    // Harvest and attach to the next declaration.
                    let txt = node_text(child, parsed.source()).trim().to_string();
                    let cleaned = clean_doc_comment(&txt);
                    // Coalesce consecutive doc comments into one block.
                    if let Some(ref existing) = pending_doc {
                        pending_doc = Some(format!("{existing}\n{cleaned}"));
                    } else {
                        pending_doc = Some(cleaned);
                    }
                }
                _ => {
                    // Non-declaration nodes (comments, stray tokens) clear the
                    // pending doc — they break the association chain.
                    if child.kind() != "comment" {
                        pending_doc = None;
                    }
                }
            }
        }

        out
    }
}

// --- Library name ---

fn find_library_name(root: Node, source: &[u8]) -> Option<String> {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "library_name" {
            return Some(node_text(child, source));
        }
    }
    None
}

// --- Lambda expression (top-level function / getter shorthand) ---

/// `lambda_expression` wraps `function_signature` + `function_body`. At the
/// top level it represents a function declaration; inside a class body it is
/// used for method bodies (already handled by the method-signature path).
fn emit_lambda(
    node: Node,
    prefix: &str,
    parent: Option<&str>,
    doc: Option<String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // Locate the function_signature child.
    let sig_node = first_child_kind(&node, &["function_signature"]);
    let Some(sig) = sig_node else {
        return;
    };

    let Some(name_node) = first_child_kind(&sig, &["identifier"]) else {
        return;
    };
    let name = node_text(name_node, source);
    let qname = qualify(prefix, parent, &name);

    let mut s = make_symbol(
        &qname,
        &name,
        "function",
        &sig,
        source,
        Some(node_text(sig, source)),
        None,
        parent.map(|p| qualify(prefix, None, p)),
    );
    if doc.is_some() {
        s.doc_comment = doc;
    }
    out.symbols.push(s);

    walk_for_type_refs_in_signature(&sig, source, out);

    if let Some(body) = first_child_kind(&node, &["function_body", "block"]) {
        walk_for_type_refs(body, source, out);
        walk_body_for_calls(body, &qname, source, out);
    }
}

// --- Classes ---

fn emit_class(
    node: Node,
    prefix: &str,
    doc: Option<String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(n, source));
    let Some(name) = name else {
        return;
    };
    let qname = qualify(prefix, None, &name);

    let mut s = make_symbol(
        &qname,
        &name,
        "class",
        &node,
        source,
        Some(node_text(node, source)),
        None,
        None,
    );
    if doc.is_some() {
        s.doc_comment = doc;
    }
    out.symbols.push(s);

    // Record superclass / mixins / interfaces as type relations.
    let mut sub = node.walk();
    for c in node.children(&mut sub) {
        match c.kind() {
            "superclass" => {
                if let Some(t) = first_child_kind(&c, &["identifier", "type_identifier"]) {
                    out.type_relations.push(type_rel(&qname, "extends", &node_text(t, source), &t));
                }
            }
            "mixins" | "interfaces" | "interface_type_list" => {
                for tc in children_of_kind(c, &["type_identifier"]) {
                    out.type_relations.push(type_rel(
                        &qname,
                        "implements",
                        &node_text(tc, source),
                        &tc,
                    ));
                }
            }
            _ => {}
        }
    }

    // Walk the class body for members.
    if let Some(body) = first_child_kind(&node, &["class_body"]) {
        walk_class_or_extension_body(body, &qname, prefix, source, out);
    }
}

// --- Class / extension body walker ---

fn walk_class_or_extension_body(
    body: Node,
    owner_qname: &str,
    prefix: &str,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let owner_name = owner_qname.rsplit("::").next().unwrap_or("");
    let mut pending_doc: Option<String> = None;

    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "class_member_definition" => {
                let doc = pending_doc.take();
                emit_class_member(child, owner_qname, owner_name, prefix, doc, source, out);
            }
            "documentation_comment" => {
                let txt = node_text(child, source).trim().to_string();
                let cleaned = clean_doc_comment(&txt);
                if let Some(ref existing) = pending_doc {
                    pending_doc = Some(format!("{existing}\n{cleaned}"));
                } else {
                    pending_doc = Some(cleaned);
                }
            }
            _ => {
                if child.kind() != "comment" {
                    pending_doc = None;
                }
            }
        }
    }
}

fn emit_class_member(
    node: Node,
    owner_qname: &str,
    owner_name: &str,
    prefix: &str,
    doc: Option<String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // class_member_definition wraps either:
    //   - method_signature → (function_signature | getter_signature |
    //     setter_signature | factory_constructor_signature | operator_signature)
    //   - declaration → (constructor_signature | variable_declaration | ...)
    //   - static_final_declaration / static_final_declaration_list
    //
    // The function_body / constructor_body appears as a sibling of the
    // signature inside class_member_definition.

    // Track the member qname so we can attribute call-sites in the body.
    let sym_count_before = out.symbols.len();
    let mut body: Option<Node> = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "method_signature" => {
                emit_method_sig(child, owner_qname, owner_name, prefix, doc.as_ref(), source, out);
            }
            "declaration" => {
                emit_declaration(child, owner_qname, owner_name, prefix, doc.as_ref(), source, out);
            }
            "function_body" | "constructor_body" => {
                body = Some(child);
            }
            _ => {
                walk_for_type_refs(child, source, out);
            }
        }
    }

    // Walk the body for call-sites, attributing calls to the emitted symbol.
    if let Some(b) = body {
        if out.symbols.len() > sym_count_before {
            let member_qname = out.symbols[out.symbols.len() - 1].qualified_name.clone();
            // Also check for getter/setter duplicates — take the last one with
            // a different qname if there are multiple symbols emitted.
            walk_body_for_calls(b, &member_qname, source, out);
        }
    }
}

fn emit_method_sig(
    node: Node,
    owner_qname: &str,
    owner_name: &str,
    prefix: &str,
    doc: Option<&String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // Locate the inner signature node (function_signature, getter_signature, etc.)
    let mut sub = node.walk();
    let inner = node
        .children(&mut sub)
        .find(|c| {
            matches!(
                c.kind(),
                "function_signature"
                    | "getter_signature"
                    | "setter_signature"
                    | "factory_constructor_signature"
                    | "constant_constructor_signature"
                    | "redirecting_factory_constructor_signature"
                    | "operator_signature"
            )
        });

    let Some(inner) = inner else {
        return;
    };

    match inner.kind() {
        "function_signature" => {
            emit_method_func(inner, owner_qname, owner_name, prefix, "method", doc, source, out);
        }
        "getter_signature" => {
            emit_getter_setter(inner, "getter", owner_qname, owner_name, prefix, doc, source, out);
        }
        "setter_signature" => {
            emit_getter_setter(inner, "setter", owner_qname, owner_name, prefix, doc, source, out);
        }
        "factory_constructor_signature"
        | "constant_constructor_signature"
        | "redirecting_factory_constructor_signature" => {
            emit_factory_constructor(inner, owner_qname, owner_name, prefix, doc, source, out);
        }
        "operator_signature" => {
            emit_method_func(inner, owner_qname, owner_name, prefix, "operator", doc, source, out);
        }
        _ => {}
    }
}

fn emit_method_func(
    sig: Node,
    owner_qname: &str,
    owner_name: &str,
    prefix: &str,
    kind: &str,
    doc: Option<&String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let Some(name_node) = first_child_kind(&sig, &["identifier"]) else {
        return;
    };
    let name = node_text(name_node, source);
    let qname = qualify(prefix, Some(owner_name), &name);
    let mut s = make_symbol(
        &qname,
        &name,
        kind,
        &sig,
        source,
        Some(node_text(sig, source)),
        None,
        Some(owner_qname.to_string()),
    );
    if let Some(d) = doc {
        s.doc_comment = Some(d.clone());
    }
    out.symbols.push(s);

    walk_for_type_refs_in_signature(&sig, source, out);
}

fn emit_getter_setter(
    sig: Node,
    kind: &str,
    owner_qname: &str,
    owner_name: &str,
    prefix: &str,
    doc: Option<&String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = sig
        .child_by_field_name("name")
        .map(|n| node_text(n, source));
    let Some(name) = name else {
        return;
    };
    let qname = qualify(prefix, Some(owner_name), &name);
    let mut s = make_symbol(
        &qname,
        &name,
        kind,
        &sig,
        source,
        Some(node_text(sig, source)),
        None,
        Some(owner_qname.to_string()),
    );
    if let Some(d) = doc {
        s.doc_comment = Some(d.clone());
    }
    out.symbols.push(s);

    walk_for_type_refs_in_signature(&sig, source, out);
}

fn emit_factory_constructor(
    sig: Node,
    owner_qname: &str,
    owner_name: &str,
    prefix: &str,
    doc: Option<&String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // factory_constructor_signature children: identifier (return type / class
    // name), identifier (named constructor part, e.g. "file" in
    // "Logger.file"), formal_parameter_list. Take the last identifier child
    // — it is the constructor name.
    let name = {
        let mut sub = sig.walk();
        sig.children(&mut sub)
            .filter(|c| c.kind() == "identifier")
            .last()
            .map(|n| node_text(n, source))
    };
    let Some(name) = name else {
        return;
    };
    let kind = match sig.kind() {
        "factory_constructor_signature" | "redirecting_factory_constructor_signature" => {
            "factory_constructor"
        }
        "constant_constructor_signature" => "const_constructor",
        _ => "constructor",
    };
    let qname = qualify(prefix, Some(owner_name), &name);
    let mut s = make_symbol(
        &qname,
        &name,
        kind,
        &sig,
        source,
        Some(node_text(sig, source)),
        None,
        Some(owner_qname.to_string()),
    );
    if let Some(d) = doc {
        s.doc_comment = Some(d.clone());
    }
    out.symbols.push(s);

    walk_for_type_refs_in_signature(&sig, source, out);
}

fn emit_declaration(
    node: Node,
    owner_qname: &str,
    owner_name: &str,
    prefix: &str,
    doc: Option<&String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // declaration wraps constructor_signature or fields
    // (type_identifier + initialized_identifier_list).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "constructor_signature" => {
                // Named constructors (e.g. `Point.origin()`) have two
                // `name:` fields — the first is the class name, the last
                // is the actual constructor name. Regular constructors
                // have a single `name:` field.
                let name = {
                    let mut csub = child.walk();
                    let names: Vec<String> = child
                        .children(&mut csub)
                        .filter(|c| c.kind() == "identifier")
                        .map(|c| node_text(c, source))
                        .collect();
                    names
                        .last()
                        .cloned()
                        .unwrap_or_else(|| owner_name.to_string())
                };
                let qname = qualify(prefix, Some(owner_name), &name);
                let mut s = make_symbol(
                    &qname,
                    &name,
                    "constructor",
                    &child,
                    source,
                    Some(node_text(child, source)),
                    None,
                    Some(owner_qname.to_string()),
                );
                if let Some(d) = doc {
                    s.doc_comment = Some(d.clone());
                }
                out.symbols.push(s);
                walk_for_type_refs_in_signature(&child, source, out);
            }
            "initialized_identifier_list" => {
                for sub in children_of_kind(child, &["initialized_identifier"]) {
                    if let Some(id) = first_child_kind(&sub, &["identifier"]) {
                        let fname = node_text(id, source);
                        let qname = qualify(prefix, Some(owner_name), &fname);
                        let mut s = make_symbol(
                            &qname,
                            &fname,
                            "field",
                            &sub,
                            source,
                            Some(node_text(sub, source)),
                            None,
                            Some(owner_qname.to_string()),
                        );
                        if let Some(d) = doc {
                            s.doc_comment = Some(d.clone());
                        }
                        out.symbols.push(s);
                    }
                }
            }
            "type_identifier" => {
                // Bare type ref in declaration — collect for impact analysis.
            }
            _ => {}
        }
    }
    walk_for_type_refs(node, source, out);
}

// --- Mixins ---

fn emit_mixin(
    node: Node,
    prefix: &str,
    doc: Option<String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = first_child_kind(&node, &["identifier"]).map(|n| node_text(n, source));
    let Some(name) = name else {
        return;
    };
    let qname = qualify(prefix, None, &name);
    let mut s = make_symbol(
        &qname,
        &name,
        "mixin",
        &node,
        source,
        Some(node_text(node, source)),
        None,
        None,
    );
    if doc.is_some() {
        s.doc_comment = doc;
    }
    out.symbols.push(s);

    // Walk class_body for mixin methods.
    if let Some(body) = first_child_kind(&node, &["class_body"]) {
        walk_class_or_extension_body(body, &qname, prefix, source, out);
    }
}

// --- Enums ---

fn emit_enum(
    node: Node,
    prefix: &str,
    doc: Option<String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(n, source));
    let Some(name) = name else {
        return;
    };
    let qname = qualify(prefix, None, &name);
    let mut s = make_symbol(
        &qname,
        &name,
        "enum",
        &node,
        source,
        Some(node_text(node, source)),
        None,
        None,
    );
    if doc.is_some() {
        s.doc_comment = doc;
    }
    out.symbols.push(s);

    // Walk enum_body for enum constants.
    if let Some(body) = first_child_kind(&node, &["enum_body"]) {
        let mut sub = body.walk();
        for c in body.children(&mut sub) {
            if c.kind() == "enum_constant" {
                let cname = c
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source));
                if let Some(cname) = cname {
                    let cqname = qualify(prefix, Some(&name), &cname);
                    out.symbols.push(make_symbol(
                        &cqname,
                        &cname,
                        "enum_constant",
                        &c,
                        source,
                        Some(node_text(c, source)),
                        None,
                        Some(qname.clone()),
                    ));
                }
            }
        }
    }
}

// --- Extensions ---

fn emit_extension(
    node: Node,
    prefix: &str,
    doc: Option<String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(n, source));
    let on_type = node
        .child_by_field_name("class")
        .map(|n| node_text(n, source));

    let display_name = name.unwrap_or_else(|| {
        on_type
            .as_deref()
            .map(|t| format!("extension_on_{t}"))
            .unwrap_or_else(|| "extension".to_string())
    });

    let qname = qualify(prefix, None, &display_name);
    let sig = match (&display_name, &on_type) {
        (dn, Some(ot)) => format!("extension {dn} on {ot}"),
        _ => format!("extension {display_name}"),
    };
    let mut s = make_symbol(
        &qname,
        &display_name,
        "extension",
        &node,
        source,
        Some(sig),
        None,
        None,
    );
    if doc.is_some() {
        s.doc_comment = doc;
    }
    out.symbols.push(s);

    // Walk extension_body for methods.
    let body_kind = if first_child_kind(&node, &["extension_body"]).is_some() {
        "extension_body"
    } else {
        "class_body"
    };
    if let Some(body) = first_child_kind(&node, &[body_kind]) {
        walk_class_or_extension_body(body, &qname, prefix, source, out);
    }
}

// --- Type aliases (typedef) ---

fn emit_type_alias(
    node: Node,
    prefix: &str,
    doc: Option<String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // type_alias children: type_identifier (name), then the type expression.
    let name = first_child_kind(&node, &["type_identifier"]).map(|n| node_text(n, source));
    let Some(name) = name else {
        return;
    };
    let qname = qualify(prefix, None, &name);
    let mut s = make_symbol(
        &qname,
        &name,
        "type_alias",
        &node,
        source,
        Some(node_text(node, source)),
        None,
        None,
    );
    if doc.is_some() {
        s.doc_comment = doc;
    }
    out.symbols.push(s);
}

// --- Top-level variables (constants) ---

fn emit_top_level_variable(
    node: Node,
    prefix: &str,
    doc: Option<String>,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    // local_variable_declaration → initialized_variable_definition
    //   → const_builtin / final_builtin, name: (identifier), value: (...)
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "initialized_variable_definition" {
            let vname = c
                .child_by_field_name("name")
                .map(|n| node_text(n, source));
            if let Some(vname) = vname {
                let qname = qualify(prefix, None, &vname);
                let mut s = make_symbol(
                    &qname,
                    &vname,
                    "variable",
                    &c,
                    source,
                    Some(node_text(c, source)),
                    None,
                    None,
                );
                if doc.is_some() {
                    s.doc_comment = doc.clone();
                }
                out.symbols.push(s);
            }
        }
    }
    walk_for_type_refs(node, source, out);
}

// --- Imports / Exports ---

fn emit_import_or_export(node: Node, source: &[u8], out: &mut ExtractedFile) {
    // import_or_export wraps library_import or library_export.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let is_export = child.kind() == "library_export";
        if !is_export && child.kind() != "library_import" {
            continue;
        }
        let kind = if is_export { "export" } else { "import" };

        // Walk into import_specification or directly to configurable_uri.
        let uri = find_descendant_text(&child, &["uri", "configurable_uri"], source);

        // Alias: look for an `identifier` under `import_specification` that is
        // not inside a combinator or uri.
        let alias = find_import_alias(&child, source);

        if let Some(u) = uri {
            out.imports.push(ExtractedImport {
                raw_path: format!("{kind}:{u}"),
                alias,
                line: (child.start_position().row as u32).saturating_add(1),
            });
        }
    }
}

fn find_import_alias(node: &Node, source: &[u8]) -> Option<String> {
    // In `import_specification`, the alias `identifier` appears as a direct
    // child alongside `configurable_uri` and `combinator`s.
    let spec = first_child_kind(node, &["import_specification"]);
    let spec_node = spec.unwrap_or(*node);

    let mut cursor = spec_node.walk();
    for c in spec_node.children(&mut cursor) {
        if c.kind() == "identifier" {
            // Check it's not inside a combinator or uri.
            if let Some(parent) = c.parent() {
                if parent.kind() == "import_specification" {
                    return Some(node_text(c, source));
                }
            }
        }
    }
    None
}

fn find_descendant_text(node: &Node, kinds: &[&str], source: &[u8]) -> Option<String> {
    let mut stack: Vec<Node> = vec![*node];
    while let Some(n) = stack.pop() {
        if kinds.contains(&n.kind()) {
            let inner = first_child_kind(&n, &["string_literal"])
                .map(|s| unquote(&node_text(s, source)))
                .unwrap_or_else(|| node_text(n, source));
            return Some(inner);
        }
        let mut sub = n.walk();
        for c in n.children(&mut sub) {
            stack.push(c);
        }
    }
    None
}

// --- Type-ref walkers ---

fn walk_for_type_refs_in_signature(decl: &Node, source: &[u8], out: &mut ExtractedFile) {
    let mut cursor = decl.walk();
    for child in decl.children(&mut cursor) {
        if matches!(
            child.kind(),
            "function_body" | "constructor_body" | "block"
        ) {
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

// --- Call-site extraction ---

/// Recursively walk a function/constructor body for call expressions.
fn walk_body_for_calls(node: Node, caller_qname: &str, source: &[u8], out: &mut ExtractedFile) {
    let mut stack: Vec<Node> = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "member_access" => {
                if has_descendant_kind(&n, "argument_part") {
                    let callee = callee_name_from_member_access(&n, source);
                    if !callee.is_empty() {
                        record_call(&n, caller_qname, &callee, source, out);
                    }
                }
            }
            "new_expression" => {
                if let Some(ti) = first_child_kind(&n, &["type_identifier"]) {
                    let name = node_text(ti, source);
                    record_call(&n, caller_qname, &name, source, out);
                }
            }
            "cascade_section" => {
                // cascade_section → cascade_selector → identifier
                if let Some(cs) = first_child_kind(&n, &["cascade_selector"]) {
                    if let Some(id) = first_child_kind(&cs, &["identifier"]) {
                        let name = node_text(id, source);
                        record_call(&n, caller_qname, &name, source, out);
                    }
                }
            }
            _ => {}
        }
        let mut sub = n.walk();
        for c in n.children(&mut sub) {
            stack.push(c);
        }
    }
}

/// Extracts the callee name from a `member_access` node that contains
/// `argument_part`. Collects identifiers from the receiver chain and
/// returns the last one (the actual called function/method).
fn callee_name_from_member_access(ma: &Node, source: &[u8]) -> String {
    let mut last_id = String::new();
    let mut cursor = ma.walk();
    for child in ma.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                last_id = node_text(child, source);
            }
            "selector" => {
                if has_descendant_kind(&child, "argument_part") {
                    break; // This selector is the call — our last_id is the callee
                }
                // Intermediate selector: walk for identifiers in
                // unconditional_assignable_selector children.
                let mut csub = child.walk();
                for sc in child.children(&mut csub) {
                    if sc.kind() == "unconditional_assignable_selector" {
                        let mut usub = sc.walk();
                        for uc in sc.children(&mut usub) {
                            if uc.kind() == "identifier" {
                                last_id = node_text(uc, source);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    last_id
}

fn has_descendant_kind(node: &Node, target: &str) -> bool {
    let mut stack: Vec<Node> = vec![*node];
    while let Some(n) = stack.pop() {
        if n.kind() == target {
            return true;
        }
        let mut sub = n.walk();
        for c in n.children(&mut sub) {
            stack.push(c);
        }
    }
    false
}

fn record_call(node: &Node, caller: &str, callee: &str, _source: &[u8], out: &mut ExtractedFile) {
    let line = (node.start_position().row as u32).saturating_add(1);
    let col = node.start_position().column as u32;
    out.calls.push(ExtractedCall {
        caller_qualified_name: caller.to_string(),
        callee_raw_name: callee.to_string(),
        line,
        col,
    });
    out.refs.push(ExtractedRef {
        raw_name: callee.to_string(),
        kind: "call".to_string(),
        line,
        col,
        end_line: (node.end_position().row as u32).saturating_add(1),
        end_col: node.end_position().column as u32,
    });
}

// --- Helpers ---

fn make_symbol(
    qname: &str,
    name: &str,
    kind: &str,
    node: &Node,
    _source: &[u8],
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
        doc_comment: None,
    }
}

fn clean_doc_comment(txt: &str) -> String {
    let trimmed = txt.trim();
    if let Some(rest) = trimmed.strip_prefix("///") {
        rest.trim().to_string()
    } else if let Some(inner) = trimmed.strip_prefix("/**").and_then(|t| t.strip_suffix("*/")) {
        inner.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn qualify(prefix: &str, parent: Option<&str>, name: &str) -> String {
    let base = match parent {
        Some(p) if !prefix.is_empty() => format!("{prefix}::{p}"),
        Some(p) => p.to_string(),
        None => prefix.to_string(),
    };
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{base}::{name}")
    }
}

fn first_child_kind<'tree>(node: &Node<'tree>, kinds: &[&str]) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if kinds.contains(&c.kind()) {
            return Some(c);
        }
    }
    None
}

fn children_of_kind<'tree>(node: Node<'tree>, kinds: &[&str]) -> Vec<Node<'tree>> {
    let mut result = Vec::new();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if kinds.contains(&c.kind()) {
            result.push(c);
        }
    }
    result
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

fn type_rel(symbol_qname: &str, relation: &str, target: &str, node: &Node) -> ExtractedTypeRel {
    ExtractedTypeRel {
        symbol_qualified_name: symbol_qname.to_string(),
        relation: relation.to_string(),
        target_raw_name: target.to_string(),
        line: (node.start_position().row as u32).saturating_add(1),
    }
}

fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        || (trimmed.starts_with('"') && trimmed.ends_with('"'))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ExtractContext;
    use crate::parse;

    fn extract_dart(src: &str) -> ExtractedFile {
        let ext = DartExtractor::new();
        let parsed = parse(src.as_bytes().to_vec(), &ext).expect("parse");
        let ctx = ExtractContext {
            relative_path: "lib/main.dart",
            module_path: "lib::main",
        };
        ext.extract(&parsed, &ctx)
    }

    fn names(out: &ExtractedFile) -> Vec<&str> {
        out.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn qname_set(out: &ExtractedFile) -> Vec<String> {
        out.symbols
            .iter()
            .map(|s| s.qualified_name.clone())
            .collect()
    }

    fn find<'a>(out: &'a ExtractedFile, name: &str) -> &'a ExtractedSymbol {
        out.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol named {name}; have {:?}", names(out)))
    }

    #[test]
    fn test_dart_extracts_top_level_function() {
        let src = "void main() { print('hello'); }";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(
            qns.contains(&"lib::main::main".to_string()),
            "qnames: {:?}",
            qns
        );
        let f = find(&out, "main");
        assert_eq!(f.kind, "function");
    }

    #[test]
    fn test_dart_extracts_class_with_methods() {
        let src = "
class User {
  String name;
  void save() {}
  String get displayName => name;
}
";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(qns.contains(&"lib::main::User".to_string()));
        assert!(qns.contains(&"lib::main::User::save".to_string()));
        assert!(qns.contains(&"lib::main::User::displayName".to_string()));
    }

    #[test]
    fn test_dart_extracts_constructor() {
        let src = "
class Point {
  final int x, y;
  Point(this.x, this.y);
}
";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(
            qns.contains(&"lib::main::Point::Point".to_string()),
            "qnames: {:?}",
            qns
        );
        let ctor = out
            .symbols
            .iter()
            .find(|s| s.name == "Point" && s.kind == "constructor");
        assert!(ctor.is_some(), "expected constructor in {:?}", names(&out));
    }

    #[test]
    fn test_dart_extracts_named_constructor() {
        let src = "
class Point {
  Point.origin();
}
";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(
            qns.contains(&"lib::main::Point::origin".to_string()),
            "qnames: {:?}",
            qns
        );
    }

    #[test]
    fn test_dart_extracts_enum_with_constants() {
        let src = "
enum Color { red, green, blue }
";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(qns.contains(&"lib::main::Color".to_string()));
        assert!(qns.contains(&"lib::main::Color::red".to_string()));
        assert!(qns.contains(&"lib::main::Color::green".to_string()));
        assert!(qns.contains(&"lib::main::Color::blue".to_string()));
    }

    #[test]
    fn test_dart_extracts_mixin() {
        let src = "
mixin Logger {
  void log(String msg) {}
}
";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(qns.contains(&"lib::main::Logger".to_string()));
        assert!(qns.contains(&"lib::main::Logger::log".to_string()));
    }

    #[test]
    fn test_dart_extracts_extension() {
        let src = "
extension StringExt on String {
  String get reversed => '';
}
";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(
            qns.contains(&"lib::main::StringExt::reversed".to_string()),
            "qnames: {:?}",
            qns
        );
    }

    #[test]
    fn test_dart_extracts_type_alias() {
        let src = "typedef IntList = List<int>;";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(
            qns.contains(&"lib::main::IntList".to_string()),
            "qnames: {:?}",
            qns
        );
        let t = find(&out, "IntList");
        assert_eq!(t.kind, "type_alias");
    }

    #[test]
    fn test_dart_extracts_import() {
        let src = "import 'package:meta/meta.dart';\nvoid main() {}";
        let out = extract_dart(src);
        assert!(out
            .imports
            .iter()
            .any(|i| i.raw_path.contains("package:meta/meta.dart")));
    }

    #[test]
    fn test_dart_extracts_doc_comment() {
        let src = "/// Returns the sum of a and b.\nint add(int a, int b) => a + b;";
        let out = extract_dart(src);
        let f = find(&out, "add");
        assert!(
            f.doc_comment.is_some(),
            "expected doc comment on add, got {:?}",
            f.doc_comment
        );
        assert!(f
            .doc_comment
            .as_deref()
            .unwrap()
            .contains("Returns the sum"));
    }

    #[test]
    fn test_dart_handles_empty_file() {
        let out = extract_dart("");
        assert!(out.symbols.is_empty());
    }

    #[test]
    fn test_dart_extracts_type_refs_in_signature() {
        let src = "void process(User u) {}";
        let out = extract_dart(src);
        assert!(
            out.refs.iter().any(|r| r.raw_name == "User"),
            "expected User in refs: {:?}",
            out.refs.iter().map(|r| &r.raw_name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dart_extracts_factory_constructor() {
        let src = "
class Logger {
  factory Logger.file(String path) => Logger._internal();
}
";
        let out = extract_dart(src);
        let ctor = out
            .symbols
            .iter()
            .find(|s| s.name == "file" && s.kind == "factory_constructor");
        assert!(
            ctor.is_some(),
            "expected factory constructor in {:?}",
            names(&out)
        );
    }

    #[test]
    fn test_dart_extracts_getter_and_setter() {
        let src = "
class Box {
  int get value => 1;
  set value(int v) {}
}
";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(
            qns.contains(&"lib::main::Box::value".to_string()),
            "qnames: {:?}",
            qns
        );
        assert!(
            out.symbols
                .iter()
                .any(|s| s.name == "value" && s.kind == "getter"),
            "expected getter in {:?}",
            names(&out)
        );
        assert!(
            out.symbols
                .iter()
                .any(|s| s.name == "value" && s.kind == "setter"),
            "expected setter in {:?}",
            names(&out)
        );
    }

    #[test]
    fn test_dart_records_superclass_as_extends_relation() {
        let src = "class Dog extends Animal {}";
        let out = extract_dart(src);
        assert!(
            out.type_relations.iter().any(|t| t.relation == "extends"
                && t.target_raw_name == "Animal"
                && t.symbol_qualified_name == "lib::main::Dog"),
            "type_relations: {:?}",
            out.type_relations
                .iter()
                .map(|t| format!("{} {} {}", t.relation, t.target_raw_name, t.symbol_qualified_name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dart_extracts_export() {
        let src = "export 'foo.dart';";
        let out = extract_dart(src);
        assert!(
            out.imports.iter().any(|i| i.raw_path.contains("foo.dart")),
            "imports: {:?}",
            out.imports.iter().map(|i| &i.raw_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dart_extracts_top_level_variable() {
        let src = "const apiUrl = 'https://api.example.com';";
        let out = extract_dart(src);
        let qns = qname_set(&out);
        assert!(
            qns.contains(&"lib::main::apiUrl".to_string()),
            "qnames: {:?}",
            qns
        );
    }

    // --- Call-site extraction tests ---

    #[test]
    fn test_dart_extracts_bare_function_call() {
        let src = "void main() { greet(); }";
        let out = extract_dart(src);
        assert!(
            out.calls
                .iter()
                .any(|c| c.caller_qualified_name == "lib::main::main"
                    && c.callee_raw_name == "greet"),
            "calls: {:?}",
            out.calls
                .iter()
                .map(|c| format!("{}→{}", c.caller_qualified_name, c.callee_raw_name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dart_extracts_method_call_on_object() {
        let src = "void main() { obj.method(); }";
        let out = extract_dart(src);
        assert!(
            out.calls
                .iter()
                .any(|c| c.caller_qualified_name == "lib::main::main"
                    && c.callee_raw_name == "method"),
            "calls: {:?}",
            out.calls
                .iter()
                .map(|c| format!("{}→{}", c.caller_qualified_name, c.callee_raw_name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dart_extracts_chained_method_call() {
        let src = "void main() { a.b.c(); }";
        let out = extract_dart(src);
        assert!(
            out.calls
                .iter()
                .any(|c| c.callee_raw_name == "c"),
            "calls: {:?}",
            out.calls
                .iter()
                .map(|c| format!("{}→{}", c.caller_qualified_name, c.callee_raw_name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dart_extracts_new_expression_call() {
        let src = "void main() { new Point(1, 2); }";
        let out = extract_dart(src);
        assert!(
            out.calls
                .iter()
                .any(|c| c.callee_raw_name == "Point"),
            "calls: {:?}",
            out.calls
                .iter()
                .map(|c| format!("{}→{}", c.caller_qualified_name, c.callee_raw_name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dart_extracts_cascade_call() {
        let src = "void main() { obj..a()..b(); }";
        let out = extract_dart(src);
        let callees: Vec<&str> = out.calls.iter().map(|c| c.callee_raw_name.as_str()).collect();
        assert!(callees.contains(&"a"), "callees: {:?}", callees);
        assert!(callees.contains(&"b"), "callees: {:?}", callees);
    }

    #[test]
    fn test_dart_extracts_call_inside_method() {
        let src = "
class Runner {
  void run() {
    doWork();
    helper.cleanup();
  }
}
";
        let out = extract_dart(src);
        let callees: Vec<&str> = out.calls.iter().map(|c| c.callee_raw_name.as_str()).collect();
        assert!(
            callees.contains(&"doWork"),
            "expected doWork in callees: {:?}",
            callees
        );
        assert!(
            callees.contains(&"cleanup"),
            "expected cleanup in callees: {:?}",
            callees
        );
    }

    #[test]
    fn test_dart_call_attributed_to_correct_enclosing_function() {
        let src = "
void helper() {}
void main() {
  helper();
}
";
        let out = extract_dart(src);
        let main_calls: Vec<&str> = out
            .calls
            .iter()
            .filter(|c| c.caller_qualified_name == "lib::main::main")
            .map(|c| c.callee_raw_name.as_str())
            .collect();
        assert_eq!(main_calls, vec!["helper"]);
    }

    #[test]
    fn test_dart_call_inside_nested_blocks() {
        let src = "
void main() {
  if (check()) {
    work();
  }
}
";
        let out = extract_dart(src);
        let callees: Vec<&str> = out.calls.iter().map(|c| c.callee_raw_name.as_str()).collect();
        assert!(callees.contains(&"check"));
        assert!(callees.contains(&"work"));
    }
}
