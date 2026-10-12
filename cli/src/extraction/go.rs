//! Go tree-sitter extraction: imports, top-level / method declarations, and
//! call / construction references.
//!
//! Declarations cover functions (`function_declaration`), methods
//! (`method_declaration`, whose receiver type is the `container`), and named
//! types (`type_spec` with a struct / interface / other underlying type).
//! References cover free calls (`Name(..)` — `Free`), selector calls
//! (`pkg.Name(..)` / `x.Name(..)` — `Free`, since Go's dominant cross-file
//! edge is the package-qualified function call and the site names no type)
//! and composite literals (`Type{..}` — `Static`).

use crate::extraction::{Declaration, ExtractionResult, Import, RefShape, Reference, Visibility};
use tree_sitter::{Node, Parser};

fn empty() -> ExtractionResult {
    ExtractionResult {
        language: "go".into(),
        imports: vec![],
        declarations: vec![],
        references: vec![],
    }
}

pub fn extract(source: &[u8]) -> ExtractionResult {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
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

/// Imports/declarations/references from an already-parsed Go tree.
/// Caller guarantees `tree` came from the Go grammar.
pub fn extract_from_tree(tree: &tree_sitter::Tree, source: &[u8]) -> ExtractionResult {
    let root = tree.root_node();
    let imports = walk_imports(root, source);
    let mut declarations = walk_declarations(root, source);
    for declaration in &mut declarations {
        declaration.visibility = match (declaration.kind.as_str(), &declaration.container) {
            // The package clause names what every importer writes.
            ("module", _) => Visibility::Public,
            ("function", Some(receiver)) => visibility(&declaration.name).min(visibility(receiver)),
            _ => visibility(&declaration.name),
        };
    }
    let references = walk_references(root, source);
    ExtractionResult {
        language: "go".into(),
        imports,
        declarations,
        references,
    }
}

/// A capital first letter exports a name from its package; any other name
/// is reachable from the package's other files. A method is never more
/// visible than its receiver type.
fn visibility(name: &str) -> Visibility {
    match name.starts_with(char::is_uppercase) {
        true => Visibility::Public,
        false => Visibility::Restricted,
    }
}

/// Every `import_spec` path. The module is the full quoted path; the symbol
/// is its last path segment (the package name the code uses unqualified).
fn walk_imports(root: Node, source: &[u8]) -> Vec<Import> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_spec" {
            if let Some(path) = n.child_by_field_name("path") {
                if let Ok(raw) = path.utf8_text(source) {
                    let module = raw.trim_matches('"').to_string();
                    let symbol = module.rsplit('/').next().map(|s| s.to_string());
                    out.push(Import {
                        module,
                        symbol,
                        locals: Vec::new(),
                        line: n.start_position().row as i64 + 1,
                        end_line: n.end_position().row as i64 + 1,
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

/// Functions, methods (receiver type as `container`), and named types.
fn walk_declarations(root: Node, source: &[u8]) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut ranges = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "package_clause" => {
                if let Some(name) = n.child_by_field_name("name").or_else(|| n.named_child(0)) {
                    if let Ok(name) = name.utf8_text(source) {
                        let line = n.start_position().row as i64 + 1;
                        if seen.insert((name.to_string(), line)) {
                            out.push(Declaration {
                                name: name.to_string(),
                                kind: "module".into(),
                                header_line: line,
                                line,
                                end_line: n.end_position().row as i64 + 1,
                                container: None,
                                parent: None,
                                header: crate::extraction::header::Builder::new(source).node(n).finish(),
                                annotations: Vec::new(),
                                self_type: None,
                                module_file: None,
                                supertypes: Vec::new(),
                                visibility: Visibility::Unknown,
                            });
                        }
                    }
                }
            }
            "function_declaration" => {
                push_named(&n, source, "function", None, &mut seen, &mut out);
            }
            "method_declaration" => {
                let container = method_receiver_type(n, source);
                push_named(&n, source, "function", container, &mut seen, &mut out);
            }
            "type_spec" => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(source) {
                        let kind = match n.child_by_field_name("type").map(|t| t.kind()) {
                            Some("interface_type") => "interface",
                            Some("struct_type") => "class",
                            _ => "class",
                        };
                        let line = name_node.start_position().row as i64 + 1;
                        if seen.insert((name.to_string(), line)) {
                            let start = n
                                .parent()
                                .filter(|parent| parent.kind() == "type_declaration")
                                .map(|parent| parent.start_byte())
                                .unwrap_or(n.start_byte());
                            let mut builder = crate::extraction::header::Builder::new(source);
                            if start != n.start_byte() {
                                builder.slice(start, start + 4);
                            }
                            if let Some(type_node) = n.child_by_field_name("type").filter(|body| {
                                matches!(body.kind(), "struct_type" | "interface_type")
                            }) {
                                let mut cursor = type_node.walk();
                                if let Some(body) =
                                    type_node.child_by_field_name("body").or_else(|| {
                                        type_node.named_children(&mut cursor).find(|child| {
                                            matches!(
                                                child.kind(),
                                                "field_declaration_list" | "method_elem_list"
                                            )
                                        })
                                    })
                                    .or_else(|| {
                                        let mut cursor = type_node.walk();
                                        let body = type_node
                                            .children(&mut cursor)
                                            .find(|child| child.kind() == "{");
                                        body
                                    })
                                {
                                    builder.slice(n.start_byte(), body.start_byte()).block(body);
                                } else {
                                    builder.node(n);
                                }
                            } else {
                                builder.node(n);
                            }
                            out.push(Declaration {
                                name: name.to_string(),
                                kind: kind.to_string(),
                                header_line: line,
                                line,
                                end_line: n.end_position().row as i64 + 1,
                                container: None,
                                parent: None,
                                header: builder.finish(),
                                annotations: Vec::new(),
                                self_type: None,
                                module_file: None,
                                supertypes: Vec::new(),
                                visibility: Visibility::Unknown,
                            });
                        }
                    }
                }
            }
            // A function body declares nothing other code reaches.
            "block" => continue,
            "field_declaration" | "const_spec" | "var_spec" => {
                let mut cursor = n.walk();
                // A `const_spec`'s `name` field holds its commas too.
                let names: Vec<Node> = n.children_by_field_name("name", &mut cursor).filter(Node::is_named).collect();
                // An embedded field is named by its type.
                let names = if names.is_empty() { n.named_child(0).into_iter().collect() } else { names };
                for (index, name_node) in names.iter().enumerate() {
                    let Ok(name) = name_node.utf8_text(source) else { continue };
                    let line = name_node.start_position().row as i64 + 1;
                    if seen.insert((name.to_string(), line)) {
                        out.push(Declaration {
                            name: name.to_string(),
                            kind: if n.kind() == "field_declaration" { "property" } else { "constant" }.into(),
                            header_line: n.start_position().row as i64 + 1,
                            line,
                            end_line: n.end_position().row as i64 + 1,
                            container: None,
                            parent: None,
                            header: spec_header(n, &names, index, source),
                            annotations: Vec::new(),
                            self_type: None,
                            module_file: None,
                            supertypes: Vec::new(),
                            visibility: Visibility::Unknown,
                        });
                    }
                }
            }
            "method_spec" | "method_elem" => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(source) {
                        let line = name_node.start_position().row as i64 + 1;
                        if seen.insert((name.to_string(), line)) {
                            out.push(Declaration {
                                name: name.to_string(),
                                kind: "function".into(),
                                header_line: n.start_position().row as i64 + 1,
                                line,
                                end_line: n.end_position().row as i64 + 1,
                                container: None,
                                parent: None,
                                header: crate::extraction::header::Builder::new(source).node(n).finish(),
                                annotations: Vec::new(),
                                self_type: None,
                                module_file: None,
                                supertypes: Vec::new(),
                                visibility: Visibility::Unknown,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
        ranges.resize(out.len(), n.start_byte()..n.end_byte());
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    crate::extraction::nest(out, ranges, |candidate| matches!(candidate.kind.as_str(), "class" | "interface"))
}

/// The header of `names[index]` of a field, `const`, or `var` spec, as if it
/// were declared alone: `X, Y int` gives `Y int`, and `const A, B = 1, 2`
/// gives `const B = 2`. A spec whose values do not pair with its names, such
/// as `a, b = f()`, reads whole.
fn spec_header(spec: Node, names: &[Node], index: usize, source: &[u8]) -> String {
    let values: Vec<Node> = spec.child_by_field_name("value").map_or_else(Vec::new, |list| {
        let mut cursor = list.walk();
        list.named_children(&mut cursor).filter(|value| value.kind() != "comment").collect()
    });
    let mut header = crate::extraction::header::Builder::new(source);
    if names.len() == 1 || (!values.is_empty() && values.len() != names.len()) {
        header.node(spec);
    } else {
        header.node(names[index]);
        if let Some(kind) = spec.child_by_field_name("type") {
            header.slice(kind.start_byte(), spec.child_by_field_name("tag").map_or(kind.end_byte(), |tag| tag.end_byte()));
        }
        let mut cursor = spec.walk();
        let equals = spec.children(&mut cursor).find(|token| token.kind() == "=");
        if let (Some(value), Some(equals)) = (values.get(index), equals) {
            header.node(equals).node(*value);
        }
    }
    match spec.kind() {
        "const_spec" => format!("const {}", header.finish()),
        "var_spec" => format!("var {}", header.finish()),
        _ => header.finish(),
    }
}

fn push_named(
    n: &Node,
    source: &[u8],
    kind: &str,
    container: Option<String>,
    seen: &mut std::collections::HashSet<(String, i64)>,
    out: &mut Vec<Declaration>,
) {
    if let Some(name_node) = n.child_by_field_name("name") {
        if let Ok(name) = name_node.utf8_text(source) {
            let line = name_node.start_position().row as i64 + 1;
            if seen.insert((name.to_string(), line)) {
                out.push(Declaration {
                    name: name.to_string(),
                    kind: kind.to_string(),
                    header_line: line,
                    line,
                    end_line: n.end_position().row as i64 + 1,
                    container,
                    parent: None,
                    header: {
                        let mut builder = crate::extraction::header::Builder::new(source);
                        if let Some(body) = n.child_by_field_name("body") {
                            builder.slice(n.start_byte(), body.start_byte()).block(body);
                        } else {
                            builder.node(*n);
                        }
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
}

/// The receiver type of a `method_declaration`: `func (r *Foo) Bar()` →
/// `Foo`. The receiver is a `parameter_list`; drill to the type identifier,
/// stripping a leading pointer.
fn method_receiver_type(n: Node, source: &[u8]) -> Option<String> {
    let recv = n.child_by_field_name("receiver")?;
    let mut stack = vec![recv];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "type_identifier" => {
                return node.utf8_text(source).ok().map(|s| s.to_string());
            }
            _ => {
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    stack.push(child);
                }
            }
        }
    }
    None
}

/// Calls and composite literals, stamped with shape.
fn walk_references(root: Node, source: &[u8]) -> Vec<Reference> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let line = n.start_position().row as i64 + 1;
        match n.kind() {
            "call_expression" => {
                if let Some(func) = n.child_by_field_name("function") {
                    if let Some((name, l, shape, receiver)) = call_shape(func, source) {
                        out.push(Reference {
                            name,
                            line: l,
                            shape,
                            receiver,
                        });
                    }
                }
            }
            // `Type{..}` — composite-literal construction. The named type is
            // a Static use. `&Type{..}` and bare `Type{..}` both carry a
            // `type` field.
            "composite_literal" => {
                if let Some(ty) = n.child_by_field_name("type") {
                    if matches!(ty.kind(), "type_identifier" | "qualified_type") {
                        if let Some(name) = last_type_segment(ty, source) {
                            out.push(Reference {
                                name: name.clone(),
                                line,
                                shape: RefShape::Static,
                                receiver: Some(name),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    out
}

/// A bare `identifier` callee is `Free`. A `selector_expression`
/// `operand.field` is `Member` — Go does not name the operand's type at the
/// site (it could be a package or a value), so it resolves to methods of
/// that name, matching the member-call model.
fn call_shape(node: Node, source: &[u8]) -> Option<(String, i64, RefShape, Option<String>)> {
    match node.kind() {
        "identifier" => {
            let txt = node.utf8_text(source).ok()?;
            Some((
                txt.to_string(),
                node.start_position().row as i64 + 1,
                RefShape::Free,
                None,
            ))
        }
        // `operand.field(..)` — in Go this is overwhelmingly a
        // package-qualified function call (`pkg.Func()`), the dominant
        // cross-file edge. It can also be a value method call (`v.Method()`),
        // but Go names no type at the site, so the two are indistinguishable
        // syntactically. Classifying it `Free` resolves the high-value
        // package-function case to the free function of that name; a
        // value-method call to a method whose receiver type is not named at
        // the site does not resolve, matching the free-call model. The bare
        // last segment (`field`) is the called name; the operand is a
        // package or value, never a type, so the receiver is None.
        "selector_expression" => {
            let field = node.child_by_field_name("field")?;
            let txt = field.utf8_text(source).ok()?;
            Some((
                txt.to_string(),
                field.start_position().row as i64 + 1,
                RefShape::Free,
                None,
            ))
        }
        _ => None,
    }
}

/// Last segment of a (possibly qualified) type node — `pkg.Foo` → `Foo`,
/// `Foo` → `Foo`.
fn last_type_segment(node: Node, source: &[u8]) -> Option<String> {
    let txt = node.utf8_text(source).ok()?;
    let seg = txt.rsplit('.').next().unwrap_or(txt).trim();
    if seg.is_empty() {
        None
    } else {
        Some(seg.to_string())
    }
}
