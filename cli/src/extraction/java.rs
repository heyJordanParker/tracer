//! Java tree-sitter extraction: imports, class / interface / enum / method
//! declarations, and call / construction / type references.
//!
//! Declarations cover classes, interfaces, enums, and their methods /
//! constructors (the enclosing type is the `container`). References cover
//! method invocations (`obj.name(..)` and bare `name(..)` — both `Member`,
//! since Java has no free functions and the receiver type is not named at
//! the site), object creation (`new Type(..)` — `Static`) and parameter /
//! return type hints (`Static`, the type names the class).

use crate::extraction::{Declaration, ExtractionResult, Import, RefShape, Reference, Visibility};
use tree_sitter::Node;

/// Imports/declarations/references from an already-parsed Java tree.
/// Caller guarantees `tree` came from the Java grammar.
pub fn extract_from_tree(tree: &tree_sitter::Tree, source: &[u8]) -> ExtractionResult {
    let root = tree.root_node();
    let imports = walk_imports(root, source);
    let declarations = walk_declarations(root, source);
    let references = walk_references(root, source);
    ExtractionResult {
        language: "java".into(),
        imports,
        declarations,
        references,
    }
}

/// `public` is public, `private` file-local, `protected` open to subclasses,
/// and no modifier internal to the package. A member of an interface or
/// annotation type is public unless it says `private`.
fn visibility(node: Node, source: &[u8], in_interface: bool) -> Visibility {
    let mut cursor = node.walk();
    let modifiers = node
        .children(&mut cursor)
        .find(|child| child.kind() == "modifiers")
        .and_then(|modifiers| modifiers.utf8_text(source).ok())
        .unwrap_or_default();
    let has = |word: &str| modifiers.split_whitespace().any(|modifier| modifier == word);
    if has("private") {
        Visibility::FileLocal
    } else if has("public") || in_interface {
        Visibility::Public
    } else if has("protected") {
        Visibility::Protected
    } else {
        Visibility::Internal
    }
}

/// `import a.b.C;` → module `a.b`, symbol `C`. `import a.b.*;` → module
/// `a.b`, no symbol.
fn walk_imports(root: Node, source: &[u8]) -> Vec<Import> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_declaration" {
            let line = n.start_position().row as i64 + 1;
            let end_line = n.end_position().row as i64 + 1;
            // The declaration holds a scoped_identifier (and optionally a `*`
            // for wildcard imports).
            let mut c = n.walk();
            let mut is_wildcard = false;
            let mut path: Option<String> = None;
            for child in n.children(&mut c) {
                match child.kind() {
                    "scoped_identifier" | "identifier" => {
                        path = child.utf8_text(source).ok().map(|s| s.to_string());
                    }
                    "asterisk" | "*" => is_wildcard = true,
                    _ => {}
                }
            }
            if let Some(p) = path {
                if is_wildcard {
                    out.push(Import {
                        module: p,
                        symbol: None,
                        locals: Vec::new(),
                        line,
                        end_line,
                        block: None,
                        visibility: None,
                    });
                } else {
                    let (module, symbol) = match p.rsplit_once('.') {
                        Some((m, s)) => (m.to_string(), Some(s.to_string())),
                        None => (p.clone(), None),
                    };
                    out.push(Import {
                        module,
                        symbol,
                        locals: Vec::new(),
                        line,
                        end_line,
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

/// Types and their methods / constructors. A method's container is the
/// enclosing class / interface / enum; a constructor takes the type name as
/// its declared name.
fn walk_declarations(root: Node, source: &[u8]) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut ranges = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<(Node, Option<String>, bool)> = vec![(root, None, false)];
    while let Some((n, container, in_interface)) = stack.pop() {
        let child_in_interface = match n.kind() {
            "interface_declaration" | "annotation_type_declaration" => true,
            "class_declaration" | "enum_declaration" | "record_declaration" => false,
            _ => in_interface,
        };
        let kind: Option<&str> = match n.kind() {
            "class_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "enum_declaration" => Some("enum"),
            "record_declaration" => Some("class"),
            "annotation_type_declaration" => Some("interface"),
            "method_declaration" => Some("function"),
            "constructor_declaration" => Some("function"),
            "field_declaration" => Some("property"),
            _ => None,
        };
        let mut child_container = container.clone();
        if matches!(
            n.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration"
        ) {
            child_container = n
                .child_by_field_name("name")
                .and_then(|nm| nm.utf8_text(source).ok())
                .map(|s| s.to_string())
                .or_else(|| container.clone());
        }
        if let Some(k) = kind {
            // A field declares one name per declarator, `int a = 1, b;`, each
            // with a header of its own.
            let mut cursor = n.walk();
            let declarators: Vec<Node> = n.children_by_field_name("declarator", &mut cursor).collect();
            let named: Vec<(Node, String)> = match n.child_by_field_name("name") {
                Some(name) => {
                    let mut header = crate::extraction::header::Builder::new(source);
                    match n.child_by_field_name("body") {
                        Some(body) => header.slice(n.start_byte(), body.start_byte()).block(body),
                        None => header.node(n),
                    };
                    vec![(name, header.finish())]
                }
                None => declarators
                    .iter()
                    .filter_map(|declarator| {
                        let mut header = crate::extraction::header::Builder::new(source);
                        header.slice(n.start_byte(), declarators[0].start_byte()).declarator(*declarator, None);
                        Some((declarator.child_by_field_name("name")?, header.finish()))
                    })
                    .collect(),
            };
            let decl_container = matches!(n.kind(), "method_declaration" | "constructor_declaration" | "field_declaration")
                .then(|| container.clone())
                .flatten();
            for (name_node, header) in named {
                let Ok(name) = name_node.utf8_text(source) else { continue };
                let line = name_node.start_position().row as i64 + 1;
                if seen.insert((name.to_string(), line)) {
                    ranges.push(n.start_byte()..n.end_byte());
                    out.push(Declaration {
                        name: name.to_string(),
                        kind: k.to_string(),
                        header_line: n.start_position().row as i64 + 1,
                        line,
                        end_line: n.end_position().row as i64 + 1,
                        container: decl_container.clone(),
                        parent: None,
                        header,
                        annotations: java_annotations(n, source),
                        self_type: None,
                        module_file: None,
                        supertypes: Vec::new(),
                        visibility: visibility(n, source, in_interface),
                    });
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push((child, child_container.clone(), child_in_interface));
        }
    }
    crate::extraction::nest(out, ranges, |candidate| matches!(candidate.kind.as_str(), "class" | "interface" | "enum"))
}

fn java_annotations(node: Node, source: &[u8]) -> Vec<String> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| child.kind() == "marker_annotation" || child.kind() == "annotation")
        .map(|annotation| {
            let source = annotation.utf8_text(source).unwrap_or("").to_string();
            let name = source
                .trim_start_matches('@')
                .split(['(', '.'])
                .next()
                .unwrap_or("")
                .to_string();
            name
        })
        .collect()
}

/// Method invocations, object creation, and parameter / return type hints,
/// stamped with shape.
fn walk_references(root: Node, source: &[u8]) -> Vec<Reference> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let line = n.start_position().row as i64 + 1;
        match n.kind() {
            // `obj.name(..)` or bare `name(..)` — Java has no free functions,
            // so every invocation is a method call. The receiver type is not
            // named at the site, so it is `Member`, carrying the object text.
            "method_invocation" => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(source) {
                        out.push(Reference {
                            name: name.to_string(),
                            line: name_node.start_position().row as i64 + 1,
                            shape: RefShape::Member,
                            receiver: n
                                .child_by_field_name("object")
                                .and_then(|object| object.utf8_text(source).ok())
                                .map(str::to_string),
                        });
                    }
                }
            }
            // `new Type(..)` — construction. The type names the class.
            "object_creation_expression" => {
                if let Some(ty) = n.child_by_field_name("type") {
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
            // A parameter's type hint names a class — a Static use.
            "formal_parameter" | "spread_parameter" => {
                if let Some(ty) = n.child_by_field_name("type") {
                    push_type(ty, source, &mut out);
                }
            }
            "method_declaration" => {
                if let Some(ty) = n.child_by_field_name("type") {
                    push_type(ty, source, &mut out);
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    out
}

/// Emit a Static reference for each named class inside a type node — handles
/// a bare `type_identifier`, a `generic_type` (`List<Foo>` references both
/// `List` and `Foo`), and a `scoped_type_identifier` (`a.b.C` → `C`).
fn push_type(node: Node, source: &[u8], out: &mut Vec<Reference>) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "type_identifier" => {
                if let Ok(name) = n.utf8_text(source) {
                    out.push(Reference {
                        name: name.to_string(),
                        line: n.start_position().row as i64 + 1,
                        shape: RefShape::Static,
                        receiver: Some(name.to_string()),
                    });
                }
            }
            "scoped_type_identifier" => {
                if let Some(name) = last_type_segment(n, source) {
                    out.push(Reference {
                        name: name.clone(),
                        line: n.start_position().row as i64 + 1,
                        shape: RefShape::Static,
                        receiver: Some(name),
                    });
                }
            }
            _ => {
                let mut c = n.walk();
                for child in n.children(&mut c) {
                    stack.push(child);
                }
            }
        }
    }
}

/// Last `.` segment of a (possibly scoped / generic) type — `a.b.C<T>` → `C`.
fn last_type_segment(node: Node, source: &[u8]) -> Option<String> {
    let txt = node.utf8_text(source).ok()?;
    let base = txt.split('<').next().unwrap_or(txt).trim();
    let seg = base.rsplit('.').next().unwrap_or(base).trim();
    if seg.is_empty() {
        None
    } else {
        Some(seg.to_string())
    }
}
