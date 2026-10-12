//! C tree-sitter extraction: `#include` imports, function / struct / enum
//! declarations, and call references.
//!
//! C has no methods or classes — every function is free and every call is a
//! `Free` reference (no container, no member/static shapes). Declarations
//! cover function definitions, structs / unions (`class`), enums (`enum`)
//! and their enumerators, struct and union fields, typedefs (`type`), and
//! file-scope variables and prototypes. Imports are `#include` paths.

use crate::extraction::header::Builder;
use crate::extraction::{Declaration, ExtractionResult, Import, RefShape, Reference, Visibility};
use std::ops::Range;
use tree_sitter::{Node, Parser};

fn empty() -> ExtractionResult {
    ExtractionResult {
        language: "c".into(),
        imports: vec![],
        declarations: vec![],
        references: vec![],
    }
}

pub fn extract(source: &[u8], path: &str) -> ExtractionResult {
    let lang: tree_sitter::Language = tree_sitter_c::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&lang).is_err() {
        return empty();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return empty(),
    };
    extract_from_tree(&tree, source, path)
}

/// Imports/declarations/references from an already-parsed C tree at `path`.
/// Caller guarantees `tree` came from the C grammar.
pub fn extract_from_tree(tree: &tree_sitter::Tree, source: &[u8], path: &str) -> ExtractionResult {
    let root = tree.root_node();
    let header_file = path.to_lowercase().ends_with(".h");
    ExtractionResult {
        language: "c".into(),
        imports: walk_imports(root, source),
        declarations: walk_declarations(root, source, header_file),
        references: walk_references(root, source),
    }
}

/// `static` keeps a declaration in its translation unit: file-local in a
/// `.c` file, and restricted in a header, whose every includer calls it.
/// Anything else links from every file.
fn linkage(node: Node, source: &[u8], header_file: bool) -> Visibility {
    let mut cursor = node.walk();
    let local = node
        .children(&mut cursor)
        .any(|child| child.kind() == "storage_class_specifier" && child.utf8_text(source) == Ok("static"));
    match local {
        true if header_file => Visibility::Restricted,
        true => Visibility::FileLocal,
        false => Visibility::Public,
    }
}

/// `#include <stdio.h>` / `#include "local.h"` — the path is the module; no
/// symbol.
fn walk_imports(root: Node, source: &[u8]) -> Vec<Import> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "preproc_include" {
            if let Some(path) = n.child_by_field_name("path") {
                if let Ok(raw) = path.utf8_text(source) {
                    let module = raw
                        .trim_matches(|c| c == '<' || c == '>' || c == '"')
                        .to_string();
                    out.push(Import {
                        module,
                        symbol: None,
                        locals: Vec::new(),
                        line: n.start_position().row as i64 + 1,
                        end_line: path.end_position().row as i64 + 1,
                        block: None,
                        visibility: None,
                    });
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

/// Every declaration the file makes outside a function body: a body
/// declares nothing other code reaches, so the walk never enters one. A
/// field or enumerator's container is the struct, union, enum, or typedef
/// that holds it.
fn walk_declarations(root: Node, source: &[u8], header_file: bool) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut ranges = Vec::new();
    let mut add = |declaration: Declaration, range: Range<usize>| {
        out.push(declaration);
        ranges.push(range);
    };
    let mut stack: Vec<(Node, Option<String>)> = vec![(root, None)];
    while let Some((n, container)) = stack.pop() {
        let mut members = container.clone();
        match n.kind() {
            "compound_statement" => continue,
            "function_definition" => {
                if let Some(name) = n.child_by_field_name("declarator").and_then(declared_name) {
                    let mut header = Builder::new(source);
                    match n.child_by_field_name("body") {
                        Some(body) => header.slice(n.start_byte(), body.start_byte()).block(body),
                        None => header.node(n),
                    };
                    add(declaration(n, name, source, "function", None, header.finish(), linkage(n, source, header_file)), n.byte_range());
                }
            }
            "struct_specifier" | "union_specifier" | "enum_specifier" => {
                // A specifier without a body names a type declared elsewhere.
                if let (Some(name), Some(body)) = (n.child_by_field_name("name"), n.child_by_field_name("body")) {
                    let kind = if n.kind() == "enum_specifier" { "enum" } else { "class" };
                    let mut header = Builder::new(source);
                    header.slice(n.start_byte(), body.start_byte()).block(body);
                    let specifier = declaration(n, name, source, kind, None, header.finish(), Visibility::Public);
                    members = Some(specifier.name.clone());
                    add(specifier, n.byte_range());
                }
            }
            "enumerator" => {
                if let Some(name) = n.child_by_field_name("name") {
                    let header = Builder::new(source).node(n).finish();
                    add(declaration(n, name, source, "property", container.clone(), header, Visibility::Public), n.byte_range());
                }
            }
            "type_definition" | "declaration" | "field_declaration" => {
                let kind = if n.kind() == "type_definition" { "type" } else { "property" };
                let visibility = linkage(n, source, header_file);
                let unnamed_body = n
                    .child_by_field_name("type")
                    .is_some_and(|kind| kind.child_by_field_name("body").is_some() && kind.child_by_field_name("name").is_none());
                let mut cursor = n.walk();
                let declarators: Vec<Node> = n.children_by_field_name("declarator", &mut cursor).collect();
                for (index, declarator) in declarators.iter().enumerate() {
                    let Some(name) = declared_name(*declarator) else { continue };
                    let declared = declaration(n, name, source, kind, container.clone(), statement_header(n, declarators[0], *declarator, source), visibility);
                    // The first declarator holds an unnamed body, so the
                    // fields of `typedef struct { … } A, *B;` nest under `A`.
                    let holds = index == 0 && unnamed_body;
                    if holds {
                        members = Some(declared.name.clone());
                    }
                    let start = if holds { n.start_byte() } else { declarator.start_byte() };
                    add(declared, start..declarator.end_byte());
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push((child, members.clone()));
        }
    }
    // Any declaration holds the members of the body it defines, so the
    // fields of `struct { int a; } config;` nest under `config`.
    crate::extraction::nest(out, ranges, |_| true)
}

/// One declaration named by `name`, spanning `node`.
fn declaration(node: Node, name: Node, source: &[u8], kind: &str, container: Option<String>, header: String, visibility: Visibility) -> Declaration {
    Declaration {
        name: name.utf8_text(source).unwrap_or_default().to_string(),
        kind: kind.into(),
        header_line: node.start_position().row as i64 + 1,
        line: name.start_position().row as i64 + 1,
        end_line: node.end_position().row as i64 + 1,
        container,
        parent: None,
        header,
        annotations: Vec::new(),
        self_type: None,
        module_file: None,
        supertypes: Vec::new(),
        visibility,
    }
}

/// The header of one declarator of `statement`, as if it were declared
/// alone: `struct Tree *left, *right;` gives `struct Tree *right;` for
/// `right`. A struct, union, or enum body the statement defines, and an
/// initializer list, read `{ … }`.
fn statement_header(statement: Node, first: Node, declarator: Node, source: &[u8]) -> String {
    let mut header = Builder::new(source);
    match statement.child_by_field_name("type").and_then(|kind| kind.child_by_field_name("body")) {
        Some(body) => header.slice(statement.start_byte(), body.start_byte()).block(body).slice(body.end_byte(), first.start_byte()),
        None => header.slice(statement.start_byte(), first.start_byte()),
    };
    header.declarator(declarator, declarator.child_by_field_name("value").filter(|value| value.kind() == "initializer_list"));
    header.finish()
}

/// The name a declarator declares, through every pointer, array, function,
/// parenthesized, attributed, and initialized wrapper: `int (*handler)(int)`
/// declares `handler`.
fn declared_name(declarator: Node) -> Option<Node> {
    match declarator.kind() {
        "identifier" | "field_identifier" | "type_identifier" => Some(declarator),
        _ => {
            let mut cursor = declarator.walk();
            let inner = declarator.child_by_field_name("declarator").or_else(|| {
                declarator.named_children(&mut cursor).find(|child| {
                    child.kind().ends_with("declarator") || matches!(child.kind(), "identifier" | "field_identifier" | "type_identifier")
                })
            });
            inner.and_then(declared_name)
        }
    }
}

/// Every `call_expression` — a bare `name(..)` is a `Free` call (C has no
/// member or static calls). A call through a function pointer field
/// (`obj->fn(..)`) resolves the field name as `Member`.
fn walk_references(root: Node, source: &[u8]) -> Vec<Reference> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" {
            if let Some(func) = n.child_by_field_name("function") {
                if let Some((name, line, shape)) = call_shape(func, source) {
                    out.push(Reference {
                        name,
                        line,
                        shape,
                        receiver: None,
                    });
                }
            }
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    out
}

/// A bare `identifier` callee is a `Free` call. A `field_expression`
/// `obj.fn` / `obj->fn` (function-pointer field) is a `Member` call.
fn call_shape(node: Node, source: &[u8]) -> Option<(String, i64, RefShape)> {
    match node.kind() {
        "identifier" => {
            let txt = node.utf8_text(source).ok()?;
            Some((
                txt.to_string(),
                node.start_position().row as i64 + 1,
                RefShape::Free,
            ))
        }
        "field_expression" => {
            let field = node.child_by_field_name("field")?;
            let txt = field.utf8_text(source).ok()?;
            Some((
                txt.to_string(),
                field.start_position().row as i64 + 1,
                RefShape::Member,
            ))
        }
        _ => None,
    }
}
