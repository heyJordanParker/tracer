//! Python tree-sitter extraction: module-level imports and definitions.

use crate::extraction::{Declaration, ExtractionResult, Import, RefShape, Reference, Visibility};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

const QUERY_SRC: &str = r#"
        ; import_statement covers `import a`, `import a.b`, `import a as c`
        (import_statement
          name: (dotted_name) @import.module)
        (import_statement
          name: (aliased_import
                  name: (dotted_name) @import.module))

        ; import_from_statement covers `from x import y`, `from x import y as z`,
        ; and the relative `from .x import y` and `from . import y`
        (import_from_statement
          module_name: [(dotted_name) (relative_import)] @import_from.module
          name: (dotted_name) @import_from.symbol)
        (import_from_statement
          module_name: [(dotted_name) (relative_import)] @import_from.module
          name: (aliased_import
                  name: (dotted_name) @import_from.symbol))
"#;

fn empty() -> ExtractionResult {
    ExtractionResult {
        language: "python".into(),
        imports: vec![],
        declarations: vec![],
        references: vec![],
    }
}

/// Each function's and class's docstring, by its name's line: the string
/// statement that opens its body.
pub fn docstrings(tree: &tree_sitter::Tree, source: &[u8]) -> HashMap<i64, String> {
    let mut found = HashMap::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "function_definition" | "class_definition") {
            let opening = node.child_by_field_name("body").and_then(|body| body.named_child(0));
            let string = opening
                .filter(|statement| statement.kind() == "expression_statement")
                .and_then(|statement| statement.named_child(0))
                .filter(|child| child.kind() == "string");
            if let (Some(name), Some(string)) = (node.child_by_field_name("name"), string) {
                found.insert(name.start_position().row as i64 + 1, string.utf8_text(source).unwrap_or_default().to_string());
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    found
}

/// Imports/declarations/references from an already-parsed Python tree at
/// `path`. Decoupled from the parse so `file_facts` shares one tree with
/// `ccn` (single-parse). Caller guarantees `tree` came from the Python
/// grammar.
pub fn extract_from_tree(tree: &tree_sitter::Tree, source: &[u8], path: &str) -> ExtractionResult {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let query = match Query::new(&lang, QUERY_SRC) {
        Ok(q) => q,
        Err(_) => return empty(),
    };
    let capture_names = query.capture_names();

    #[derive(Clone)]
    struct Cap {
        name: String,
        text: String,
        /// The first and last lines of the statement that holds the capture.
        lines: (i64, i64),
        start_byte: usize,
    }
    let mut caps: Vec<Cap> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, tree.root_node(), source);
    while let Some(m) = it.next() {
        for cap in m.captures {
            let node = cap.node;
            let statement = std::iter::successors(Some(node), Node::parent)
                .find(|holder| matches!(holder.kind(), "import_statement" | "import_from_statement"))
                .unwrap_or(node);
            caps.push(Cap {
                name: capture_names[cap.index as usize].to_string(),
                text: node.utf8_text(source).unwrap_or("").to_string(),
                lines: (statement.start_position().row as i64 + 1, statement.end_position().row as i64 + 1),
                start_byte: node.start_byte(),
            });
        }
    }

    // Group captures by name in first-seen order, then iterate the nodes
    // within each group.
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<Cap>> = HashMap::new();
    for c in caps {
        if !groups.contains_key(&c.name) {
            order.push(c.name.clone());
        }
        groups.entry(c.name.clone()).or_default().push(c);
    }

    let mut imports: Vec<Import> = Vec::new();

    // Symbol resolution depends only on byte position, not on the order
    // captures are emitted. Collect every pending module first so a
    // `from X import a, b` resolves its symbols correctly regardless of
    // capture-emission order.
    let mut pending_module: Vec<(usize, String)> = Vec::new();
    if let Some(g) = groups.get("import_from.module") {
        for c in g {
            pending_module.push((c.start_byte, c.text.clone()));
        }
    }

    for name in &order {
        for c in &groups[name] {
            match c.name.as_str() {
                "import.module" => imports.push(Import {
                    module: c.text.clone(),
                    symbol: None,
                    locals: Vec::new(),
                    line: c.lines.0,
                    end_line: c.lines.1,
                    block: None,
                    visibility: None,
                }),
                "import_from.module" => {}
                "import_from.symbol" => {
                    let module = nearest_pending(&pending_module, c.start_byte);
                    imports.push(Import {
                        module,
                        symbol: Some(c.text.clone()),
                        locals: Vec::new(),
                        line: c.lines.0,
                        end_line: c.lines.1,
                        block: None,
                        visibility: None,
                    });
                }
                _ => {}
            }
        }
    }

    let all = dunder_all(tree.root_node(), source);
    let mut declarations = walk_declarations(tree.root_node(), source);
    for declaration in &mut declarations {
        declaration.visibility = visibility(&declaration.name, declaration.parent.is_none(), all.as_ref());
    }
    // A package's `__init__.py` re-exports what it imports from inside the
    // package, and the imported names its `__all__` lists.
    if path.ends_with("__init__.py") {
        let listed = |name: &str| all.as_ref().is_some_and(|all| all.contains(name));
        for import in imports
            .iter_mut()
            .filter(|import| import.module.starts_with('.') || import.binding().is_some_and(listed))
        {
            import.visibility = Some(Visibility::Public);
        }
    }
    let references = walk_references(tree.root_node(), source);

    ExtractionResult {
        language: "python".into(),
        imports,
        declarations,
        references,
    }
}

/// With `__all__`, the top-level names it lists are the module's API and the
/// rest are internal. Without it, a leading `_` marks a name internal, as
/// PEP 8 reads it, except a dunder, which Python itself calls.
fn visibility(name: &str, top_level: bool, all: Option<&HashSet<String>>) -> Visibility {
    if let (true, Some(all)) = (top_level, all) {
        return match all.contains(name) {
            true => Visibility::Public,
            false => Visibility::Internal,
        };
    }
    match name.starts_with('_') && !(name.starts_with("__") && name.ends_with("__")) {
        true => Visibility::Internal,
        false => Visibility::Public,
    }
}

/// The names a top-level `__all__ = [...]` lists, `None` when there is none.
fn dunder_all(root: Node, source: &[u8]) -> Option<HashSet<String>> {
    let mut cursor = root.walk();
    let assignment = root
        .named_children(&mut cursor)
        .filter(|statement| statement.kind() == "expression_statement")
        .filter_map(|statement| statement.named_child(0))
        .find(|assignment| {
            assignment.kind() == "assignment"
                && assignment.child_by_field_name("left").and_then(|left| left.utf8_text(source).ok()) == Some("__all__")
        })?;
    let names = assignment.child_by_field_name("right")?;
    let mut cursor = names.walk();
    let listed = names
        .named_children(&mut cursor)
        .filter(|item| item.kind() == "string")
        .filter_map(|item| item.utf8_text(source).ok())
        .map(|text| text.trim_matches(['"', '\'']).to_string())
        .collect();
    Some(listed)
}

/// Every `function_definition` and `class_definition` in the tree —
/// top-level, methods on classes, and definitions nested inside other
/// functions. Source-order, deduped (one entry per (name, line) pair). A
/// function defined directly inside a class body is a method and carries
/// that class as its `container`; a top-level function or one nested inside
/// another function has none.
fn walk_declarations(root: Node, source: &[u8]) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Carries the enclosing class name only when the parent is a class body
    // — a function nested inside another function is not a method, so the
    // container resets to None when descending into a function.
    let mut parent_keys = HashMap::new();
    let mut stack: Vec<(Node, Option<String>, Option<(String, i64)>)> =
        vec![(root, None, None)];
    while let Some((n, container, lexical_parent)) = stack.pop() {
        let kind = match n.kind() {
            "function_definition" => Some("function"),
            "class_definition" => Some("class"),
            "assignment" | "expression_statement" if python_is_member_assignment(n) => {
                Some("property")
            }
            _ => None,
        };
        let mut child_container = container.clone();
        let mut child_parent = lexical_parent.clone();
        if let Some(k) = kind {
            if let Some(name_node) = python_name_node(n) {
                if let Ok(name) = name_node.utf8_text(source) {
                    let line = name_node.start_position().row as i64 + 1;
                    let decl_container = if matches!(n.kind(), "function_definition" | "assignment")
                    {
                        container.clone()
                    } else {
                        None
                    };
                    // A class scopes its direct method children; descending
                    // into a function clears the class scope (inner defs are
                    // free functions, not methods).
                    child_container = match n.kind() {
                        "class_definition" => Some(name.to_string()),
                        "function_definition" => None,
                        _ => container.clone(),
                    };
                    let key = (name.to_string(), line);
                    if matches!(n.kind(), "function_definition" | "class_definition") {
                        child_parent = Some(key.clone());
                    }
                    if seen.insert(key.clone()) {
                        parent_keys.insert(key, lexical_parent.clone());
                        out.push(Declaration {
                            name: name.to_string(),
                            kind: k.to_string(),
                            header_line: python_header_line(n),
                            line,
                            end_line: n.end_position().row as i64 + 1,
                            container: decl_container,
                            parent: None,
                            header: python_header(n, source),
                            annotations: python_annotations(n, source),
                            self_type: None,
                            module_file: None,
                            supertypes: Vec::new(),
                            visibility: Visibility::Unknown,
                        });
                    }
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push((child, child_container.clone(), child_parent.clone()));
        }
    }
    out.sort_by_key(|d| d.line);
    let parents: HashMap<(String, i64), u32> = out
        .iter()
        .enumerate()
        .map(|(index, declaration)| ((declaration.name.clone(), declaration.line), index as u32))
        .collect();
    for declaration in &mut out {
        declaration.parent = parent_keys
            .get(&(declaration.name.clone(), declaration.line))
            .and_then(|parent| parent.as_ref())
            .and_then(|parent| parents.get(parent).copied());
    }
    out
}

fn python_is_member_assignment(node: Node) -> bool {
    if node.kind() == "expression_statement" {
        let mut cursor = node.walk();
        return node
            .named_children(&mut cursor)
            .any(|child| child.kind() == "assignment")
            && node.parent().is_some_and(|parent| {
                parent.kind() == "module"
                    || parent.kind() == "block"
                        && parent
                            .parent()
                            .is_some_and(|node| node.kind() == "class_definition")
            });
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "module"
        || parent.kind() == "block"
            && parent
                .parent()
                .is_some_and(|node| node.kind() == "class_definition")
}

fn python_name_node(node: Node) -> Option<Node> {
    if node.kind() == "expression_statement" {
        let mut cursor = node.walk();
        return node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "assignment")
            .and_then(python_name_node);
    }
    if node.kind() != "assignment" {
        return node.child_by_field_name("name");
    }
    node.child_by_field_name("left").or_else(|| {
        let mut cursor = node.walk();
        let name = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "identifier");
        name
    })
}

fn python_header_line(node: Node) -> i64 {
    node.parent()
        .filter(|parent| parent.kind() == "decorated_definition")
        .map(|parent| parent.start_position().row as i64 + 1)
        .unwrap_or_else(|| node.start_position().row as i64 + 1)
}

fn python_header(node: Node, source: &[u8]) -> String {
    let mut builder = crate::extraction::header::Builder::new(source);
    let start = node
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")
        .map(|parent| parent.start_byte())
        .unwrap_or_else(|| node.start_byte());
    if start != node.start_byte() {
        builder.slice(start, node.start_byte());
    }
    if let Some(body) = node.child_by_field_name("body") {
        // The header stops at the definition's own colon: comments that open
        // the body sit before the body node and belong to it.
        let mut cursor = node.walk();
        let colon_end = node
            .children(&mut cursor)
            .filter(|child| child.kind() == ":" && child.end_byte() <= body.start_byte())
            .last()
            .map_or(body.start_byte(), |colon| colon.end_byte());
        builder.slice(node.start_byte(), colon_end).expression(body);
    } else {
        builder.node(node);
    }
    builder.finish()
}

fn python_annotations(node: Node, source: &[u8]) -> Vec<String> {
    let Some(parent) = node
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")
    else {
        return Vec::new();
    };
    let mut cursor = parent.walk();
    parent
        .children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
        .map(|decorator| {
            let source = decorator.utf8_text(source).unwrap_or("").to_string();
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

/// Every `call` expression in the tree. Resolves the callee identifier:
///   - bare `foo()`            → name = "foo"
///   - attribute `o.foo()`     → name = "foo"  (last segment)
///   - qualified `a.b.foo()`   → name = "foo"
/// Subscripts and complex callees are skipped — only the last identifier
/// matters for reference resolution against the declaration index.
fn walk_references(root: Node, source: &[u8]) -> Vec<Reference> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call" {
            if let Some(func) = n.child_by_field_name("function") {
                if let Some((name, line, shape, receiver)) = call_shape(func, source) {
                    out.push(Reference {
                        name,
                        line,
                        shape,
                        receiver,
                    });
                }
            }
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    out
}

/// Name, line, shape, and receiver for a call's function node. A bare
/// `foo()` is `Free`; an `obj.foo()` is `Member` with the object text
/// (`self`, a variable, a longer expression) as the receiver. Python names
/// no class at a member call site, so the receiver names a value, not a
/// type, and does not disambiguate the method's class.
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
        "attribute" => {
            let attr = node.child_by_field_name("attribute")?;
            let txt = attr.utf8_text(source).ok()?;
            let receiver = node
                .child_by_field_name("object")
                .and_then(|o| o.utf8_text(source).ok())
                .map(|s| s.to_string());
            Some((
                txt.to_string(),
                attr.start_position().row as i64 + 1,
                RefShape::Member,
                receiver,
            ))
        }
        _ => None,
    }
}

/// The pending `from`-module whose byte offset most closely precedes a
/// symbol — the module that symbol was imported from.
fn nearest_pending(pending: &[(usize, String)], symbol_byte: usize) -> String {
    pending
        .iter()
        .filter(|(b, _)| *b < symbol_byte)
        .max_by_key(|(b, _)| *b)
        .map(|(_, m)| m.clone())
        .unwrap_or_else(|| "unknown".to_string())
}
