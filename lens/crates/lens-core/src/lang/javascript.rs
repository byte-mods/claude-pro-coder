//! JavaScript language extractor for `.js` and `.jsx`.
//!
//! Reuses the TypeScript walker — JS and TS share an AST shape for the
//! constructs lens cares about (function/class/import/call/method), and
//! TS-specific nodes (interface_declaration, type_alias_declaration) simply
//! don't appear in JS code so the corresponding emit paths sit idle.
//!
//! What differs from TypeScript:
//!   - tree-sitter grammar is `tree_sitter_javascript::language()` for `.js`
//!     and `language_jsx()` for `.jsx` (different from TS's grammars,
//!     produces compatible-shape AST for JS-only constructs).
//!   - emitted [`ExtractedFile::language`] is [`LanguageId::JavaScript`] not
//!     `TypeScript`, so storage segregates the two for `lens map` etc.
//!   - module-path convention identical to TS: strip `.js`/`.jsx`, join with
//!     `::`, collapse `index.js`/`index.jsx` to parent dir (Node.js
//!     resolution rule).
//!   - type-annotation refs and interface/type-alias symbols are simply
//!     never emitted because the AST nodes don't exist for JS.

use crate::extract::{ExtractContext, ExtractedFile};
use crate::lang::{LanguageExtractor, LanguageId};
use crate::parse::ParsedFile;

pub struct JavaScriptExtractor {
    is_jsx: bool,
}

impl JavaScriptExtractor {
    pub const fn js() -> Self {
        Self { is_jsx: false }
    }
    pub const fn jsx() -> Self {
        Self { is_jsx: true }
    }
}

impl LanguageExtractor for JavaScriptExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::JavaScript
    }

    fn extensions(&self) -> &'static [&'static str] {
        if self.is_jsx { &["jsx"] } else { &["js", "mjs", "cjs"] }
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        // tree-sitter-javascript exposes a single `language()` that accepts
        // both plain JS and JSX. Unlike TS where ts/tsx are separate grammars,
        // JS's grammar has a built-in JSX path. We expose two extractor
        // flavours anyway so the registry can map .js and .jsx to distinct
        // entries — keeps the option open for future grammar divergence.
        tree_sitter_javascript::language()
    }

    fn extract(&self, parsed: &ParsedFile, ctx: &ExtractContext) -> ExtractedFile {
        let mut out = ExtractedFile::empty(ctx.relative_path, LanguageId::JavaScript);
        let scope = super::typescript::Scope::file_root();
        super::typescript::walk(parsed.root_node(), &scope, ctx, parsed.source(), &mut out);
        out
    }

    /// JavaScript convention:
    ///   - strip `.js` / `.jsx` / `.mjs` / `.cjs`
    ///   - join with `::` (consistent with TS for cross-language symbol shape)
    ///   - collapse `index.js` / `index.jsx` to parent dir (Node resolution)
    fn module_path_from_relative_path(&self, relative_path: &str) -> String {
        let (parent, last) = match relative_path.rfind('/') {
            Some(i) => (&relative_path[..i], &relative_path[i + 1..]),
            None => ("", relative_path),
        };
        let parent_joined = parent.replace('/', "::");
        if matches!(last, "index.js" | "index.jsx" | "index.mjs" | "index.cjs") {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ExtractContext;
    use crate::parse;

    fn extract_js(src: &str, module_path: &str) -> ExtractedFile {
        let ext = JavaScriptExtractor::js();
        let parsed = parse(src.as_bytes().to_vec(), &ext).expect("parse");
        let ctx = ExtractContext { relative_path: "test.js", module_path };
        ext.extract(&parsed, &ctx)
    }

    fn names(out: &ExtractedFile) -> Vec<&str> {
        out.symbols.iter().map(|s| s.name.as_str()).collect()
    }
    fn qnames(out: &ExtractedFile) -> Vec<&str> {
        out.symbols.iter().map(|s| s.qualified_name.as_str()).collect()
    }
    fn find<'a>(out: &'a ExtractedFile, name: &str) -> &'a crate::extract::ExtractedSymbol {
        out.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol named {name}; have {:?}", names(out)))
    }

    #[test]
    fn test_javascript_extracts_top_level_function() {
        let src = "function add(a, b) { return a + b; }\n";
        let out = extract_js(src, "math");
        assert_eq!(names(&out), vec!["add"]);
        assert_eq!(find(&out, "add").kind, "function");
        assert_eq!(find(&out, "add").qualified_name, "math::add");
    }

    #[test]
    fn test_javascript_extracts_arrow_function_const() {
        let src = "const greet = (name) => `hi ${name}`;\n";
        let out = extract_js(src, "g");
        assert_eq!(names(&out), vec!["greet"]);
        assert_eq!(find(&out, "greet").kind, "function");
    }

    #[test]
    fn test_javascript_extracts_class_with_method() {
        let src = "class Greeter {\n    constructor(name) { this.name = name; }\n    greet() { return this.name; }\n}\n";
        let out = extract_js(src, "g");
        let qns = qnames(&out);
        assert!(qns.contains(&"g::Greeter"));
        assert!(qns.contains(&"g::Greeter::greet"));
    }

    #[test]
    fn test_javascript_extracts_default_import() {
        let src = "import React from 'react';\n";
        let out = extract_js(src, "app");
        assert_eq!(out.imports.len(), 1);
        assert_eq!(out.imports[0].alias.as_deref(), Some("React"));
        assert_eq!(out.imports[0].raw_path, "react");
    }

    #[test]
    fn test_javascript_extracts_named_imports() {
        let src = "import { foo, bar as baz } from './mod';\n";
        let out = extract_js(src, "u");
        let aliases: Vec<&str> = out.imports.iter().map(|i| i.alias.as_deref().unwrap_or("")).collect();
        assert!(aliases.contains(&"foo"));
        assert!(aliases.contains(&"baz"));
    }

    #[test]
    fn test_javascript_records_call_inside_function_body() {
        let src = "function helper() {}\nfunction main() { helper(); }\n";
        let out = extract_js(src, "m");
        assert!(out.calls.iter().any(|c| c.caller_qualified_name == "m::main" && c.callee_raw_name == "helper"));
    }

    #[test]
    fn test_javascript_records_method_call_unqualified_callee() {
        let src = "function main() { logger.info('hi'); }\n";
        let out = extract_js(src, "m");
        assert!(out.calls.iter().any(|c| c.callee_raw_name == "info"));
    }

    #[test]
    fn test_javascript_extracts_class_extends_relation() {
        let src = "class Cat extends Animal {}\n";
        let out = extract_js(src, "zoo");
        assert!(out.type_relations.iter().any(|t| t.relation == "extends" && t.target_raw_name == "Animal"));
    }

    #[test]
    fn test_javascript_skips_non_function_const_declarators() {
        let src = "const PI = 3.14;\nconst NAME = 'lens';\n";
        let out = extract_js(src, "c");
        assert!(out.symbols.is_empty(), "data const should not emit a symbol; got {:?}", names(&out));
    }

    #[test]
    fn test_javascript_emits_export_visibility() {
        let src = "export function pub() {}\nfunction priv() {}\n";
        let out = extract_js(src, "m");
        assert_eq!(find(&out, "pub").visibility.as_deref(), Some("export"));
        assert!(find(&out, "priv").visibility.is_none());
    }

    #[test]
    fn test_javascript_module_path_collapses_index_files() {
        let ext = JavaScriptExtractor::js();
        assert_eq!(ext.module_path_from_relative_path("src/auth/index.js"), "src::auth");
        assert_eq!(ext.module_path_from_relative_path("src/foo.mjs"), "src::foo");
        assert_eq!(ext.module_path_from_relative_path("a.cjs"), "a");
    }

    #[test]
    fn test_javascript_extracts_function_in_jsx_file() {
        // The .jsx flavour shares the JS grammar (which has built-in JSX).
        // Smoke that JSX inside a function body doesn't break extraction.
        let ext = JavaScriptExtractor::jsx();
        let parsed = parse(
            b"function App() { return <div>hi</div>; }\n".to_vec(),
            &ext,
        )
        .expect("parse");
        let ctx = ExtractContext { relative_path: "ui.jsx", module_path: "ui" };
        let out = ext.extract(&parsed, &ctx);
        assert_eq!(names(&out), vec!["App"]);
    }

    #[test]
    fn test_javascript_handles_empty_file() {
        let out = extract_js("", "");
        assert!(out.symbols.is_empty());
        assert!(out.imports.is_empty());
    }

    #[test]
    fn test_javascript_export_default_function() {
        let src = "export default function defaultFn() {}\n";
        let out = extract_js(src, "m");
        assert_eq!(names(&out), vec!["defaultFn"]);
        assert_eq!(find(&out, "defaultFn").visibility.as_deref(), Some("export"));
    }

    #[test]
    fn test_javascript_extracts_arrow_field_in_class_as_method() {
        let src = "class A { handle = (e) => { console.log(e); }; }\n";
        let out = extract_js(src, "m");
        assert!(qnames(&out).contains(&"m::A::handle"));
        assert_eq!(find(&out, "handle").kind, "method");
    }

    #[test]
    fn test_javascript_records_call_in_arrow_const() {
        let src = "const main = () => { helper(); };\nfunction helper() {}\n";
        let out = extract_js(src, "m");
        assert!(out.calls.iter().any(|c| c.caller_qualified_name == "m::main" && c.callee_raw_name == "helper"));
    }
}
