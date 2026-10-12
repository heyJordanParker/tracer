//! Ruby tree-sitter extraction: `require` imports, method / class / module
//! declarations, and call references.
//!
//! Declarations cover methods (`method` / `singleton_method`, whose
//! enclosing class or module is the `container`), classes (`class`) and
//! modules (`module`, modelled as an interface — a mixin namespace).
//! References cover free calls (`name` with no receiver — `Free`) and
//! receiver calls (`obj.name` — `Member`, the receiver value's class is not
//! named at the site). Ruby constructs via `Foo.new`, a member call on the
//! constant, so construction is not a separate reference shape here.

use crate::extraction::{Declaration, ExtractionResult, Import, RefShape, Reference, Visibility};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Parser};

fn empty() -> ExtractionResult {
    ExtractionResult {
        language: "ruby".into(),
        imports: vec![],
        declarations: vec![],
        references: vec![],
    }
}

pub fn extract(source: &[u8]) -> ExtractionResult {
    let lang: tree_sitter::Language = tree_sitter_ruby::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&lang).is_err() {
        return empty();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return empty(),
    };
    extract_from_tree(&tree, source)
}

/// Imports/declarations/references from an already-parsed Ruby tree.
/// Caller guarantees `tree` came from the Ruby grammar.
pub fn extract_from_tree(tree: &tree_sitter::Tree, source: &[u8]) -> ExtractionResult {
    let root = tree.root_node();
    let imports = walk_imports(root, source);
    let mut declarations = walk_declarations(root, source);
    set_visibility(&mut declarations);
    let references = walk_references(root, source);
    ExtractionResult {
        language: "ruby".into(),
        imports,
        declarations,
        references,
    }
}

/// A method after a bare `private` or `protected` in its class, or one a
/// `private :name`, `protected :name`, or `private def` names, is reachable
/// but not public: a subclass in another file calls it. A `private` line
/// itself declares nothing, so it stays in its file. Everything else is
/// public. `declarations` are in line order with their parents set.
fn set_visibility(declarations: &mut [Declaration]) {
    let mut named: HashSet<(Option<u32>, String)> = HashSet::new();
    let mut inline: HashSet<i64> = HashSet::new();
    for statement in declarations.iter().filter(|row| row.kind == "statement" && row.name != "public") {
        for word in statement.header.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')')).skip(1) {
            match word.strip_prefix(':') {
                Some(name) => {
                    named.insert((statement.parent, name.to_string()));
                }
                None if word == "def" => {
                    inline.insert(statement.line);
                }
                None => {}
            }
        }
    }
    let mut section: HashMap<Option<u32>, bool> = HashMap::new();
    for row in declarations.iter_mut() {
        if row.kind == "statement" && row.header.trim() == row.name {
            section.insert(row.parent, row.name != "public");
        }
        let restricted = row.kind == "function"
            && (section.get(&row.parent).copied().unwrap_or(false)
                || inline.contains(&row.line)
                || named.contains(&(row.parent, row.name.clone())));
        row.visibility = match (row.kind.as_str(), restricted) {
            ("statement", _) => Visibility::FileLocal,
            (_, true) => Visibility::Restricted,
            (_, false) => Visibility::Public,
        };
    }
}

/// `require 'x'` / `require_relative 'x'` — the only Ruby import statement.
/// Surfaces as a `call` whose method is `require`/`require_relative` and
/// whose first argument is a string. The module is the string; no symbol.
fn walk_imports(root: Node, source: &[u8]) -> Vec<Import> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call" {
            let method_name = n
                .child_by_field_name("method")
                .and_then(|m| m.utf8_text(source).ok());
            if matches!(method_name, Some("require") | Some("require_relative")) {
                if let Some(args) = n.child_by_field_name("arguments") {
                    let mut c = args.walk();
                    for arg in args.children(&mut c) {
                        if arg.kind() == "string" {
                            let raw = arg.utf8_text(source).unwrap_or("");
                            let module = raw.trim_matches(|c| c == '\'' || c == '"').to_string();
                            out.push(Import {
                                module,
                                symbol: None,
                                locals: Vec::new(),
                                line: n.start_position().row as i64 + 1,
                                end_line: n.end_position().row as i64 + 1,
                                block: None,
                                visibility: None,
                            });
                        }
                    }
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    out
}

/// Methods (container = enclosing class/module), classes, and modules.
fn walk_declarations(root: Node, source: &[u8]) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut ranges = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<(Node, Option<String>)> = vec![(root, None)];
    while let Some((n, container)) = stack.pop() {
        let declared = out.len();
        let kind: Option<&str> = match n.kind() {
            "method" => Some("function"),
            "singleton_method" => Some("function"),
            "class" => Some("class"),
            "module" => Some("interface"),
            "call"
                if n.child_by_field_name("receiver").is_none()
                    && n.child_by_field_name("method").is_some_and(|method| {
                        matches!(
                            method.utf8_text(source).ok(),
                            Some(
                                "attr_reader"
                                    | "attr_writer"
                                    | "attr_accessor"
                                    | "private"
                                    | "protected"
                                    | "public"
                            )
                        )
                    }) =>
            {
                Some(
                    if n.child_by_field_name("method").is_some_and(|method| {
                        matches!(
                            method.utf8_text(source).ok(),
                            Some("private" | "protected" | "public")
                        )
                    }) {
                        "statement"
                    } else {
                        "property"
                    },
                )
            }
            // A bare `private` in a class or module body opens a section.
            "identifier"
                if n.parent().is_some_and(|body| {
                    body.kind() == "body_statement"
                        && body.parent().is_some_and(|holder| matches!(holder.kind(), "class" | "module" | "singleton_class"))
                }) && matches!(
                        n.utf8_text(source).ok(),
                        Some("private" | "protected" | "public")
                    ) =>
            {
                Some("statement")
            }
            _ => None,
        };
        let mut child_container = container.clone();
        if matches!(n.kind(), "class" | "module") {
            child_container = constant_name(n, source).or_else(|| container.clone());
        }
        if let Some(k) = kind {
            let name = match n.kind() {
                "class" | "module" => constant_name(n, source),
                "call" => n
                    .child_by_field_name("method")
                    .and_then(|method| method.utf8_text(source).ok())
                    .map(|name| name.to_string()),
                "identifier" => n.utf8_text(source).ok().map(str::to_string),
                _ => n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(source).ok())
                    .map(|s| s.to_string()),
            };
            if let Some(name) = name {
                let line = n
                    .child_by_field_name("name")
                    .or_else(|| n.child_by_field_name("method"))
                    .unwrap_or(n)
                    .start_position()
                    .row as i64
                    + 1;
                let decl_container = if matches!(n.kind(), "method" | "singleton_method" | "call") {
                    container.clone()
                } else {
                    None
                };
                if seen.insert((name.clone(), line)) {
                    out.push(Declaration {
                        name,
                        kind: k.to_string(),
                        header_line: line,
                        line,
                        end_line: n.end_position().row as i64 + 1,
                        container: decl_container,
                        parent: None,
                        header: {
                            let mut builder = crate::extraction::header::Builder::new(source);
                            let mut cursor = n.walk();
                            let body = n.child_by_field_name("body").or_else(|| {
                                n.named_children(&mut cursor).find(|child| child.kind() == "body_statement")
                            });
                            match body {
                                // `def size = @data.size` has an expression
                                // for its body.
                                Some(body) if n.kind() == "method" && body.kind() != "body_statement" => {
                                    builder.slice(n.start_byte(), body.start_byte()).expression(body)
                                }
                                Some(body) => builder.slice(n.start_byte(), body.start_byte()).block(body),
                                None => builder.node(n),
                            };
                            builder.finish()
                        },
                        annotations: Vec::new(),
                        self_type: None,
                        module_file: None,
                        supertypes: Vec::new(),
                        visibility: Visibility::Unknown,
                    });
                }
            }
        }
        if out.len() > declared {
            ranges.push(n.start_byte()..n.end_byte());
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push((child, child_container.clone()));
        }
    }
    crate::extraction::nest(out, ranges, |candidate| matches!(candidate.kind.as_str(), "class" | "interface"))
}

/// The constant name of a `class` / `module` node — its `name` field, last
/// `::` segment (`Foo::Bar` → `Bar`).
fn constant_name(n: Node, source: &[u8]) -> Option<String> {
    let name_node = n.child_by_field_name("name")?;
    let txt = name_node.utf8_text(source).ok()?;
    let seg = txt.rsplit("::").next().unwrap_or(txt).trim();
    if seg.is_empty() {
        None
    } else {
        Some(seg.to_string())
    }
}

/// Calls, stamped with shape. A `call` with no receiver is `Free`; with a
/// receiver it is `Member`, carrying the receiver text (the receiver value's
/// class is not named at the site).
fn walk_references(root: Node, source: &[u8]) -> Vec<Reference> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call" {
            if let Some(method) = n.child_by_field_name("method") {
                if let Ok(name) = method.utf8_text(source) {
                    // `require`/`require_relative` are imports, not calls.
                    if !matches!(name, "require" | "require_relative") {
                        let receiver = n.child_by_field_name("receiver");
                        out.push(Reference {
                            name: name.to_string(),
                            line: method.start_position().row as i64 + 1,
                            shape: if receiver.is_some() { RefShape::Member } else { RefShape::Free },
                            receiver: receiver
                                .and_then(|receiver| receiver.utf8_text(source).ok())
                                .map(str::to_string),
                        });
                    }
                }
            }
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    out
}
