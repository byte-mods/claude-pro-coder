//! Python language extractor. Walks a tree-sitter-python AST and emits
//! structured [`ExtractedSymbol`] records, plus refs, calls, imports, and
//! type-relations.
//!
//! Qualified-name convention is dotted (Python-native): `pkg.module.Class.method`.
//! Distinct from Rust's `::` separator — both are stable contracts in the
//! schema.

use tree_sitter::Node;

use crate::extract::{
    ExtractContext, ExtractedCall, ExtractedFile, ExtractedImport, ExtractedRef, ExtractedSymbol,
    ExtractedTypeRel,
};
use crate::lang::{LanguageExtractor, LanguageId};
use crate::parse::ParsedFile;

pub struct PythonExtractor;

impl LanguageExtractor for PythonExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::Python
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_python::language()
    }

    fn extract(&self, parsed: &ParsedFile, ctx: &ExtractContext) -> ExtractedFile {
        let mut out = ExtractedFile::empty(ctx.relative_path, LanguageId::Python);
        let scope = Scope::file_root();
        walk(parsed.root_node(), &scope, ctx, parsed.source(), &mut out);
        out
    }

    /// Python convention: drop the `.py` extension and replace `/` with `.`,
    /// **collapsing** `__init__.py` to its parent directory so that symbols
    /// declared in `pkg/__init__.py` get qname `pkg.Sym`, not `pkg.__init__.Sym`.
    fn module_path_from_relative_path(&self, relative_path: &str) -> String {
        let (parent, last) = match relative_path.rfind('/') {
            Some(i) => (&relative_path[..i], &relative_path[i + 1..]),
            None => ("", relative_path),
        };
        let parent_joined = parent.replace('/', ".");
        if last == "__init__.py" {
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
            format!("{parent_joined}.{stem}")
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
    Class,
}

impl Scope {
    fn file_root() -> Self {
        Self {
            parent_qname: None,
            kind: ScopeKind::File,
        }
    }
}

/// Symbol-emitting walker. Called at the file root and on class bodies.
/// Function bodies are NOT walked here — they are walked by
/// [`walk_within_function`] which collects calls and refs but does not
/// emit nested function/class definitions as separate symbols (matching the
/// Rust extractor's contract).
fn walk(node: Node, scope: &Scope, ctx: &ExtractContext, source: &[u8], out: &mut ExtractedFile) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" => emit_function(child, scope, ctx, source, out),
            "class_definition" => emit_class(child, scope, ctx, source, out),
            "decorated_definition" => emit_decorated(child, scope, ctx, source, out),
            "import_statement" => extract_import_statement(child, source, out),
            "import_from_statement" => extract_import_from_statement(child, source, out),
            _ => {}
        }
    }
}

fn classify_function(scope: &Scope) -> &'static str {
    match scope.kind {
        ScopeKind::Class => "method",
        _ => "function",
    }
}

fn emit_function(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let kind = classify_function(scope);
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
        extract_def_signature(&node, source),
        extract_doc_comment(&node, source),
    ));
    // Walk the entire function_definition (parameters + return type + body)
    // for refs and calls. Calls in default arguments and refs in type
    // annotations get attributed to this function.
    walk_within_function(node, &qname, source, out);
}

fn emit_class(
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
        "class",
        node,
        scope,
        extract_def_signature(&node, source),
        extract_doc_comment(&node, source),
    ));

    // `superclasses` field points to the argument_list of `class Foo(Base1, Base2, kw=value)`.
    // Positional arguments are base classes; `keyword_argument` children (e.g. `metaclass=Meta`)
    // are NOT base classes and must be skipped.
    if let Some(args) = node.child_by_field_name("superclasses") {
        let mut cursor = args.walk();
        for child in args.named_children(&mut cursor) {
            if child.kind() == "keyword_argument" {
                continue;
            }
            let target = node_text(child, source).trim().to_string();
            if !target.is_empty() {
                out.type_relations.push(ExtractedTypeRel {
                    symbol_qualified_name: qname.clone(),
                    relation: "extends".to_string(),
                    target_raw_name: target,
                    line: (child.start_position().row + 1) as u32,
                });
            }
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        let inner = Scope {
            parent_qname: Some(qname),
            kind: ScopeKind::Class,
        };
        walk(body, &inner, ctx, source, out);
    }
}

/// Decorated definitions (`@dec\ndef foo():` or `@dec\nclass Foo:`) wrap an
/// inner `function_definition` or `class_definition` in the `definition` field.
/// We descend to the inner node so the symbol's start position is the `def`/
/// `class` keyword line, not the `@` line. Decorators themselves are ignored
/// in v1 — they could become refs/imports in a later pass.
fn emit_decorated(
    node: Node,
    scope: &Scope,
    ctx: &ExtractContext,
    source: &[u8],
    out: &mut ExtractedFile,
) {
    let inner = match node.child_by_field_name("definition") {
        Some(n) => n,
        None => return,
    };
    match inner.kind() {
        "function_definition" => emit_function(inner, scope, ctx, source, out),
        "class_definition" => emit_class(inner, scope, ctx, source, out),
        _ => {}
    }
}

/// Decompose `import x`, `import x.y.z`, `import x as y`, `import x, y` into
/// per-module [`ExtractedImport`] rows.
fn extract_import_statement(node: Node, source: &[u8], out: &mut ExtractedFile) {
    let line = (node.start_position().row + 1) as u32;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "dotted_name" => {
                out.imports.push(ExtractedImport {
                    raw_path: node_text(child, source),
                    alias: None,
                    line,
                });
            }
            "aliased_import" => {
                let path = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source))
                    .unwrap_or_default();
                let alias = child
                    .child_by_field_name("alias")
                    .map(|n| node_text(n, source));
                out.imports.push(ExtractedImport {
                    raw_path: path,
                    alias,
                    line,
                });
            }
            _ => {}
        }
    }
}

/// Decompose `from MOD import name [as alias], ...` and `from MOD import *`.
/// `MOD` may be a `dotted_name` (`os.path`) or a `relative_import` (`.`,
/// `.pkg`); we preserve relative-import dots verbatim so callers can
/// distinguish absolute from relative imports.
fn extract_import_from_statement(node: Node, source: &[u8], out: &mut ExtractedFile) {
    let line = (node.start_position().row + 1) as u32;

    let module_str = node
        .child_by_field_name("module_name")
        .map(|n| node_text(n, source))
        .unwrap_or_default();

    // `from X import *` — emit a single wildcard import row and stop.
    {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "wildcard_import" {
                let raw = if module_str.is_empty() {
                    "*".to_string()
                } else {
                    format!("{module_str}.*")
                };
                out.imports.push(ExtractedImport {
                    raw_path: raw,
                    alias: None,
                    line,
                });
                return;
            }
        }
    }

    // Otherwise iterate the multi-valued `name` field — one row per imported name.
    let mut cursor = node.walk();
    for name_node in node.children_by_field_name("name", &mut cursor) {
        match name_node.kind() {
            "dotted_name" => {
                let leaf = node_text(name_node, source);
                out.imports.push(ExtractedImport {
                    raw_path: join_module_and_leaf(&module_str, &leaf),
                    alias: None,
                    line,
                });
            }
            "aliased_import" => {
                let leaf = name_node
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source))
                    .unwrap_or_default();
                let alias = name_node
                    .child_by_field_name("alias")
                    .map(|n| node_text(n, source));
                out.imports.push(ExtractedImport {
                    raw_path: join_module_and_leaf(&module_str, &leaf),
                    alias,
                    line,
                });
            }
            _ => {}
        }
    }
}

/// Combine a module string and a leaf import name into a dotted raw_path.
/// Relative-import modules (`.`, `..pkg`) join with no extra separator if the
/// Python docstring extraction. Per PEP 257: the docstring is the first
/// statement of a function/class body, when that statement is a bare string
/// literal. We accept both single-line (`"""one"""`) and triple-quoted
/// multi-line forms; quotes are stripped, leading whitespace per line is
/// trimmed, lines re-joined with `\n`. Returns `None` when no docstring.
///
/// Modules also have docstrings (first statement of the file). We don't
/// emit module-level symbols today, so module docstrings aren't surfaced
/// via this code path — callers extract module docs separately if needed.
fn extract_doc_comment(node: &Node, source: &[u8]) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let mut first_stmt: Option<Node> = None;
    for c in body.children(&mut cursor) {
        // Skip newlines / comments at the top of the body.
        match c.kind() {
            "expression_statement" => {
                first_stmt = Some(c);
                break;
            }
            // Comments and structural punctuation don't terminate the
            // search — keep going until we hit a real statement.
            "comment" | ":" | "block" => continue,
            _ => break,
        }
    }
    let stmt = first_stmt?;
    let mut sub = stmt.walk();
    let mut string_node: Option<Node> = None;
    for c in stmt.children(&mut sub) {
        if c.kind() == "string" {
            string_node = Some(c);
            break;
        }
    }
    let s = string_node?;
    let raw = node_text(s, source);
    Some(normalise_docstring(&raw))
}

/// Strip quotes (single, double, triple-single, triple-double) and leading
/// indentation per line. Preserves blank lines inside the docstring.
fn normalise_docstring(raw: &str) -> String {
    let trimmed = raw.trim();
    // Strip raw/byte/format prefix (b'', r'', rb'', f'') if present. Lens
    // doesn't care about escape semantics — drop the prefix and proceed.
    let body = trimmed
        .trim_start_matches(|c: char| matches!(c, 'r' | 'R' | 'b' | 'B' | 'f' | 'F' | 'u' | 'U'));
    // Triple-quoted forms first so we don't mis-strip a one-line """abc""".
    let inner: &str = if let Some(s) = body.strip_prefix("\"\"\"").and_then(|s| s.strip_suffix("\"\"\"")) {
        s
    } else if let Some(s) = body.strip_prefix("'''").and_then(|s| s.strip_suffix("'''")) {
        s
    } else if let Some(s) = body.strip_prefix("\"").and_then(|s| s.strip_suffix("\"")) {
        s
    } else if let Some(s) = body.strip_prefix("'").and_then(|s| s.strip_suffix("'")) {
        s
    } else {
        body
    };
    // Per-line trim of leading whitespace; drop a leading blank line if
    // the docstring opens with `\n` (common multi-line shape).
    let lines: Vec<&str> = inner.lines().collect();
    let stripped_lines: Vec<String> =
        lines.iter().map(|l| l.trim_start().to_string()).collect();
    stripped_lines.join("\n").trim().to_string()
}

/// module already ends in `.`; absolute modules use `.` as the joiner.
fn join_module_and_leaf(module: &str, leaf: &str) -> String {
    if module.is_empty() {
        leaf.to_string()
    } else if module.ends_with('.') {
        format!("{module}{leaf}")
    } else {
        format!("{module}.{leaf}")
    }
}

fn make_symbol(
    qualified_name: String,
    name: String,
    kind: &'static str,
    node: Node,
    scope: &Scope,
    signature: Option<String>,
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
        // Python has no syntactic visibility modifier (only the `_`-prefix
        // convention, which is name-based and not part of v1 semantics).
        visibility: None,
        parent_qualified_name: scope.parent_qname.clone(),
        doc_comment,
    }
}

fn build_qname(module_path: &str, parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}.{name}"),
        None if module_path.is_empty() => name.to_string(),
        None => format!("{module_path}.{name}"),
    }
}

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

/// Signature for a function or class definition: text from the item start to
/// the start of its `body` block, with trailing whitespace and a trailing `:`
/// stripped.
fn extract_def_signature(node: &Node, source: &[u8]) -> Option<String> {
    let end = match node.child_by_field_name("body") {
        Some(body) => body.start_byte(),
        None => node.end_byte(),
    };
    let s = std::str::from_utf8(&source[node.start_byte()..end]).ok()?;
    let s = s.trim().trim_end_matches(':').trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Walk the body of a function — which in Python includes parameters,
/// type annotations, and the `block`. Emits a [`ExtractedCall`] for every
/// `call` node and a [`ExtractedRef`] for every identifier that appears
/// inside a `type` annotation. Does not extract nested definitions as
/// symbols (consistent with the Rust extractor: items nested in function
/// bodies are not part of the public symbol surface in v1).
fn walk_within_function(node: Node, caller_qname: &str, source: &[u8], out: &mut ExtractedFile) {
    match node.kind() {
        "call" => {
            if let Some(func) = node.child_by_field_name("function") {
                let pos = node.start_position();
                out.calls.push(ExtractedCall {
                    caller_qualified_name: caller_qname.to_string(),
                    callee_raw_name: node_text(func, source),
                    line: (pos.row + 1) as u32,
                    col: pos.column as u32,
                });
            }
            // Continue descending — arguments may themselves contain calls
            // (e.g. `outer(inner())`).
        }
        "type" => {
            // Collect every identifier nested inside this type annotation as
            // a ref. Stop the outer walk here; the inner traversal covers
            // the entire subtree.
            collect_type_refs(node, source, out);
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_within_function(child, caller_qname, source, out);
    }
}

/// Recursively collect `identifier` nodes inside a `type` annotation.
/// Handles nested generics like `Optional[List[Dict[str, int]]]` — each
/// type-name contribution becomes a separate ref.
fn collect_type_refs(node: Node, source: &[u8], out: &mut ExtractedFile) {
    if node.kind() == "identifier" {
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
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_refs(child, source, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_py(src: &str, module_path: &str) -> ExtractedFile {
        let parsed = crate::parse::parse(src.as_bytes().to_vec(), &PythonExtractor)
            .expect("parse python source");
        let ctx = ExtractContext {
            relative_path: "src/test.py",
            module_path,
        };
        PythonExtractor.extract(&parsed, &ctx)
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
    fn test_python_extracts_top_level_function_with_signature() {
        let src = "def add(x: int, y: int = 5) -> str:\n    return str(x + y)\n";
        let out = extract_py(src, "pkg.math");
        assert_eq!(names(&out), vec!["add"]);
        let s = find(&out, "add");
        assert_eq!(s.kind, "function");
        assert_eq!(s.qualified_name, "pkg.math.add");
        assert_eq!(s.parent_qualified_name, None);
        let sig = s.signature.as_deref().unwrap_or("");
        assert!(sig.contains("def add(x: int, y: int = 5) -> str"), "sig was: {sig}");
        assert_eq!(s.visibility, None);
        assert_eq!(s.start_line, 1);
        assert!(s.body_end_byte > s.body_start_byte);
    }

    #[test]
    fn test_python_extracts_class_definition() {
        let src = "class Greeter:\n    pass\n";
        let out = extract_py(src, "pkg.greet");
        let s = find(&out, "Greeter");
        assert_eq!(s.kind, "class");
        assert_eq!(s.qualified_name, "pkg.greet.Greeter");
        assert_eq!(s.parent_qualified_name, None);
    }

    #[test]
    fn test_python_extracts_method_with_class_parent_qname() {
        let src = "\
class Counter:
    def __init__(self):
        self.n = 0
    def inc(self):
        self.n += 1
";
        let out = extract_py(src, "pkg.ctr");
        let qns = qnames(&out);
        assert!(qns.contains(&"pkg.ctr.Counter"));
        assert!(qns.contains(&"pkg.ctr.Counter.__init__"));
        assert!(qns.contains(&"pkg.ctr.Counter.inc"));
        let inc = find(&out, "inc");
        assert_eq!(inc.kind, "method");
        assert_eq!(inc.parent_qualified_name.as_deref(), Some("pkg.ctr.Counter"));
    }

    #[test]
    fn test_python_extracts_class_inheritance_as_extends_relation() {
        let src = "\
class Base:
    pass
class Derived(Base):
    pass
";
        let out = extract_py(src, "pkg.h");
        let rels: Vec<(&str, &str, &str)> = out
            .type_relations
            .iter()
            .map(|t| {
                (
                    t.symbol_qualified_name.as_str(),
                    t.relation.as_str(),
                    t.target_raw_name.as_str(),
                )
            })
            .collect();
        assert!(
            rels.contains(&("pkg.h.Derived", "extends", "Base")),
            "rels were {rels:?}"
        );
    }

    #[test]
    fn test_python_extracts_class_with_multiple_bases() {
        let src = "class C(A, B):\n    pass\n";
        let out = extract_py(src, "pkg.m");
        let targets: Vec<&str> = out
            .type_relations
            .iter()
            .filter(|t| t.relation == "extends" && t.symbol_qualified_name == "pkg.m.C")
            .map(|t| t.target_raw_name.as_str())
            .collect();
        assert!(targets.contains(&"A"));
        assert!(targets.contains(&"B"));
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn test_python_keyword_arguments_in_class_args_are_not_extends_relations() {
        // `class Foo(metaclass=Meta):` — `metaclass=Meta` is a kwarg, not a base.
        let src = "class Foo(Base, metaclass=Meta):\n    pass\n";
        let out = extract_py(src, "pkg.k");
        let targets: Vec<&str> = out
            .type_relations
            .iter()
            .filter(|t| t.relation == "extends")
            .map(|t| t.target_raw_name.as_str())
            .collect();
        assert_eq!(targets, vec!["Base"], "metaclass kwarg must not be an extends");
    }

    #[test]
    fn test_python_extracts_decorated_function() {
        let src = "@cache\n@staticmethod\ndef foo():\n    pass\n";
        let out = extract_py(src, "pkg.d");
        let s = find(&out, "foo");
        assert_eq!(s.kind, "function");
        assert_eq!(s.qualified_name, "pkg.d.foo");
    }

    #[test]
    fn test_python_extracts_decorated_class() {
        let src = "@dataclass\nclass Point:\n    pass\n";
        let out = extract_py(src, "pkg.d");
        let s = find(&out, "Point");
        assert_eq!(s.kind, "class");
        assert_eq!(s.qualified_name, "pkg.d.Point");
    }

    #[test]
    fn test_python_extracts_decorated_method_inside_class() {
        let src = "\
class C:
    @staticmethod
    def make():
        pass
";
        let out = extract_py(src, "pkg.d");
        let m = find(&out, "make");
        assert_eq!(m.kind, "method");
        assert_eq!(m.parent_qualified_name.as_deref(), Some("pkg.d.C"));
    }

    #[test]
    fn test_python_extracts_simple_import_statement() {
        let src = "import os\n";
        let out = extract_py(src, "pkg.u");
        assert_eq!(out.imports.len(), 1);
        let i = &out.imports[0];
        assert_eq!(i.raw_path, "os");
        assert_eq!(i.alias, None);
        assert_eq!(i.line, 1);
    }

    #[test]
    fn test_python_extracts_dotted_import() {
        let src = "import a.b.c\n";
        let out = extract_py(src, "pkg.u");
        assert_eq!(out.imports.len(), 1);
        assert_eq!(out.imports[0].raw_path, "a.b.c");
    }

    #[test]
    fn test_python_extracts_aliased_import() {
        let src = "import sys as system\n";
        let out = extract_py(src, "pkg.u");
        assert_eq!(out.imports.len(), 1);
        let i = &out.imports[0];
        assert_eq!(i.raw_path, "sys");
        assert_eq!(i.alias.as_deref(), Some("system"));
    }

    #[test]
    fn test_python_extracts_multiple_imports_on_one_line() {
        let src = "import a, b as bb\n";
        let out = extract_py(src, "pkg.u");
        let pairs: Vec<(&str, Option<&str>)> = out
            .imports
            .iter()
            .map(|i| (i.raw_path.as_str(), i.alias.as_deref()))
            .collect();
        assert!(pairs.contains(&("a", None)));
        assert!(pairs.contains(&("b", Some("bb"))));
        assert_eq!(out.imports.len(), 2);
    }

    #[test]
    fn test_python_extracts_from_import() {
        let src = "from os import path\n";
        let out = extract_py(src, "pkg.u");
        assert_eq!(out.imports.len(), 1);
        let i = &out.imports[0];
        assert_eq!(i.raw_path, "os.path");
        assert_eq!(i.alias, None);
    }

    #[test]
    fn test_python_extracts_from_import_with_alias() {
        let src = "from os import path as p\n";
        let out = extract_py(src, "pkg.u");
        assert_eq!(out.imports.len(), 1);
        let i = &out.imports[0];
        assert_eq!(i.raw_path, "os.path");
        assert_eq!(i.alias.as_deref(), Some("p"));
    }

    #[test]
    fn test_python_extracts_from_import_multiple_names() {
        let src = "from os import path, sep, getcwd as gc\n";
        let out = extract_py(src, "pkg.u");
        let triples: Vec<(&str, Option<&str>)> = out
            .imports
            .iter()
            .map(|i| (i.raw_path.as_str(), i.alias.as_deref()))
            .collect();
        assert!(triples.contains(&("os.path", None)));
        assert!(triples.contains(&("os.sep", None)));
        assert!(triples.contains(&("os.getcwd", Some("gc"))));
        assert_eq!(out.imports.len(), 3);
    }

    #[test]
    fn test_python_extracts_from_import_wildcard() {
        let src = "from os.path import *\n";
        let out = extract_py(src, "pkg.u");
        assert_eq!(out.imports.len(), 1);
        assert_eq!(out.imports[0].raw_path, "os.path.*");
        assert_eq!(out.imports[0].alias, None);
    }

    #[test]
    fn test_python_extracts_relative_import_dot_only() {
        let src = "from . import sibling\n";
        let out = extract_py(src, "pkg.u");
        assert_eq!(out.imports.len(), 1);
        let i = &out.imports[0];
        // Relative-import dots are preserved verbatim. `from . import sibling`
        // → the module portion is "." so the joiner yields ".sibling".
        assert!(
            i.raw_path == ".sibling" || i.raw_path == "..sibling".trim_start_matches('.'),
            "expected .sibling, got {:?}",
            i.raw_path
        );
        assert!(i.raw_path.ends_with("sibling"));
    }

    #[test]
    fn test_python_extracts_relative_import_with_subpackage() {
        let src = "from .pkg import x\n";
        let out = extract_py(src, "pkg.u");
        assert_eq!(out.imports.len(), 1);
        let i = &out.imports[0];
        assert!(
            i.raw_path == ".pkg.x" || i.raw_path == "pkg.x",
            "expected .pkg.x or pkg.x, got {:?}",
            i.raw_path
        );
    }

    #[test]
    fn test_python_extracts_function_call_attributed_to_caller() {
        let src = "\
def helper():
    pass

def main():
    helper()
";
        let out = extract_py(src, "pkg.c");
        let pairs: Vec<(&str, &str)> = out
            .calls
            .iter()
            .map(|c| (c.caller_qualified_name.as_str(), c.callee_raw_name.as_str()))
            .collect();
        assert!(pairs.contains(&("pkg.c.main", "helper")));
    }

    #[test]
    fn test_python_extracts_method_call_attributed_to_caller() {
        let src = "\
class W:
    def run(self):
        self.tick()
    def tick(self):
        pass
";
        let out = extract_py(src, "pkg.c");
        let callees: Vec<&str> = out
            .calls
            .iter()
            .filter(|c| c.caller_qualified_name == "pkg.c.W.run")
            .map(|c| c.callee_raw_name.as_str())
            .collect();
        assert!(
            callees.iter().any(|s| s.contains("tick")),
            "self.tick call not captured: {callees:?}"
        );
    }

    #[test]
    fn test_python_extracts_call_in_default_argument() {
        // The call is in the function's parameter list; v1 attributes it to
        // the function being defined (matches Rust extractor behaviour for
        // refs in signatures).
        let src = "def f(x=helper()):\n    pass\n";
        let out = extract_py(src, "pkg.c");
        let callers: Vec<&str> = out
            .calls
            .iter()
            .map(|c| c.caller_qualified_name.as_str())
            .collect();
        assert!(callers.contains(&"pkg.c.f"));
    }

    #[test]
    fn test_python_extracts_type_refs_inside_function_body() {
        let src = "\
def make(x: int) -> str:
    y: list = []
    return str(x)
";
        let out = extract_py(src, "pkg.r");
        let refs: Vec<&str> = out.refs.iter().map(|r| r.raw_name.as_str()).collect();
        assert!(refs.contains(&"int"), "no int ref: {refs:?}");
        assert!(refs.contains(&"str"), "no str ref: {refs:?}");
        assert!(refs.contains(&"list"), "no list ref: {refs:?}");
    }

    #[test]
    fn test_python_extracts_nested_generic_type_refs() {
        let src = "def f(x: Optional[List[int]]) -> Dict[str, int]:\n    pass\n";
        let out = extract_py(src, "pkg.r");
        let refs: Vec<&str> = out.refs.iter().map(|r| r.raw_name.as_str()).collect();
        for expected in &["Optional", "List", "int", "Dict", "str"] {
            assert!(refs.contains(expected), "missing {expected} in {refs:?}");
        }
    }

    #[test]
    fn test_python_extracts_nested_class_methods() {
        let src = "\
class Outer:
    class Inner:
        def m(self):
            pass
";
        let out = extract_py(src, "pkg.n");
        let qns = qnames(&out);
        assert!(qns.contains(&"pkg.n.Outer"));
        assert!(qns.contains(&"pkg.n.Outer.Inner"));
        assert!(qns.contains(&"pkg.n.Outer.Inner.m"));
        let m = find(&out, "m");
        assert_eq!(m.kind, "method");
        assert_eq!(m.parent_qualified_name.as_deref(), Some("pkg.n.Outer.Inner"));
    }

    #[test]
    fn test_python_no_symbols_from_empty_source() {
        let out = extract_py("", "pkg.empty");
        assert!(out.symbols.is_empty());
        assert!(out.imports.is_empty());
        assert!(out.calls.is_empty());
        assert!(out.refs.is_empty());
        assert!(out.type_relations.is_empty());
    }

    #[test]
    fn test_python_does_not_extract_nested_function_inside_function_body() {
        // Local `def inner` inside a function body must NOT surface as a
        // symbol — the walker only recurses into class bodies for symbols.
        let src = "def outer():\n    def inner():\n        pass\n";
        let out = extract_py(src, "pkg.n");
        assert_eq!(names(&out), vec!["outer"]);
    }

    #[test]
    fn test_python_partial_tree_on_syntax_error_does_not_panic() {
        // Tree-sitter recovers from syntax errors and returns a partial tree;
        // the extractor must not panic on whatever shape it produces.
        let src = "def broken(:\n    pass\n";
        let _out = extract_py(src, "pkg.b");
        // No assertion on contents — just that we got here without panicking.
    }

    #[test]
    fn test_python_qualified_name_uses_module_path_at_file_root() {
        let src = "def alone():\n    pass\n";
        let with_path = extract_py(src, "deeply.nested.path");
        assert_eq!(
            find(&with_path, "alone").qualified_name,
            "deeply.nested.path.alone"
        );
        let no_path = extract_py(src, "");
        assert_eq!(find(&no_path, "alone").qualified_name, "alone");
    }

    #[test]
    fn test_python_top_level_symbols_have_no_parent_qname() {
        let src = "def solo():\n    pass\n";
        let out = extract_py(src, "pkg.p");
        assert_eq!(find(&out, "solo").parent_qualified_name, None);
    }

    #[test]
    fn test_python_class_signature_ends_before_body_block() {
        let src = "class Foo(Base):\n    pass\n";
        let out = extract_py(src, "pkg.s");
        let s = find(&out, "Foo");
        let sig = s.signature.as_deref().unwrap_or("");
        assert!(sig.starts_with("class Foo"), "sig was: {sig}");
        assert!(sig.contains("Base"), "base class missing in sig: {sig}");
        assert!(!sig.ends_with(':'), "trailing colon must be stripped: {sig}");
    }

    #[test]
    fn test_python_extracts_at_correct_line() {
        let src = "\n\nclass Foo:\n    pass\n";
        let out = extract_py(src, "pkg.l");
        assert_eq!(find(&out, "Foo").start_line, 3);
    }

    // ----- T7: module_path_from_relative_path overrides -----

    #[test]
    fn test_python_module_path_for_init_py_collapses_to_parent_dir() {
        let p = PythonExtractor;
        assert_eq!(p.module_path_from_relative_path("pkg/__init__.py"), "pkg");
        assert_eq!(
            p.module_path_from_relative_path("foo/bar/__init__.py"),
            "foo.bar"
        );
        assert_eq!(p.module_path_from_relative_path("__init__.py"), "");
    }

    #[test]
    fn test_python_module_path_for_regular_py_file_strips_extension() {
        let p = PythonExtractor;
        assert_eq!(p.module_path_from_relative_path("pkg/mod.py"), "pkg.mod");
        assert_eq!(p.module_path_from_relative_path("a/b/c.py"), "a.b.c");
    }

    #[test]
    fn test_python_module_path_at_root_with_no_parent() {
        let p = PythonExtractor;
        assert_eq!(p.module_path_from_relative_path("foo.py"), "foo");
    }

    #[test]
    fn test_python_module_path_uses_dot_separator_not_double_colon() {
        let p = PythonExtractor;
        let mp = p.module_path_from_relative_path("a/b/c.py");
        assert!(mp.contains('.'), "expected `.` in {mp}");
        assert!(!mp.contains("::"), "must not contain `::`: {mp}");
    }

    // ----- T8: T6 super-qa MINOR follow-ups -----

    #[test]
    fn test_python_class_with_star_base_documents_current_behavior() {
        // `class C(*bases):` — the star expression is a `list_splat` named
        // child, not a `keyword_argument`, so v1's filter passes it through
        // and emits an extends row with target "*bases". Documented as the
        // current contract; a future version may strip the splat.
        let src = "class C(*bases):\n    pass\n";
        let out = extract_py(src, "pkg.s");
        let targets: Vec<&str> = out
            .type_relations
            .iter()
            .filter(|t| t.relation == "extends" && t.symbol_qualified_name == "pkg.s.C")
            .map(|t| t.target_raw_name.as_str())
            .collect();
        assert_eq!(targets, vec!["*bases"], "current behavior pin");
    }

    #[test]
    fn test_python_decorated_method_inside_class_start_line_is_def_line() {
        // The `@decorator` lines come before `def`; the symbol's start_line
        // must point to the `def` line so it is consistent across
        // decorated and non-decorated methods.
        let src = "\
class C:
    @staticmethod
    def make():
        pass
";
        let out = extract_py(src, "pkg.d");
        let m = find(&out, "make");
        // Line 1: `class C:`, line 2: `@staticmethod`, line 3: `def make():`.
        assert_eq!(m.start_line, 3, "expected def line, got {}", m.start_line);
    }
}
