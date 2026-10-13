//! TypeScript/TSX/JS/JSX tree-sitter extraction: imports and exports.
//!
//! Note: tree-sitter 0.25's `QueryCursor::matches` does NOT auto-apply
//! `#eq?` text predicates, so the `(#eq? @_fn "require")` predicate is
//! enforced manually below.

use crate::extraction::{Declaration, ExtractionResult, Import, RefShape, Reference, Visibility};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

const QUERY_SRC: &str = r#"
        ; import statements — module path is always a string literal
        (import_statement source: (string (string_fragment) @import.module))

        ; named imports inside an import statement — captured separately,
        ; paired by byte position with the nearest preceding module
        (import_specifier name: (identifier) @import.symbol)

        ; require('m')
        (call_expression
          function: (identifier) @_fn
          arguments: (arguments (string (string_fragment) @import.module))
          (#eq? @_fn "require"))
"#;

fn empty() -> ExtractionResult {
    ExtractionResult {
        language: "typescript".into(),
        imports: vec![],
        declarations: vec![],
        references: vec![],
    }
}

/// Whether `path` is parsed with the TSX grammar, by its suffix.
fn is_tsx(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".tsx") || lower.ends_with(".jsx")
}

/// Imports/declarations/references from an already-parsed TS/TSX tree at
/// `path`. Decoupled from the parse for the single-parse optimization. The
/// caller parsed `tree` with the grammar `path`'s suffix picks, so the query
/// grammar matches the tree.
pub fn extract_from_tree(tree: &tree_sitter::Tree, source: &[u8], path: &str) -> ExtractionResult {
    let lang: tree_sitter::Language = if is_tsx(path) {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    };
    let query = match Query::new(&lang, QUERY_SRC) {
        Ok(q) => q,
        Err(_) => return empty(),
    };
    let capture_names = query.capture_names();

    #[derive(Clone)]
    struct Cap {
        name: String,
        text: String,
        /// The first and last lines of the `import` statement or `require`
        /// call that holds the capture.
        lines: (i64, i64),
        /// For an `import.symbol`: the module of its enclosing
        /// `import_statement`, resolved structurally (an `import { a } from
        /// 'm'` has the module string AFTER the specifiers, so a
        /// byte-distance heuristic resolves it to the wrong module).
        owner_module: Option<String>,
        locals: Vec<String>,
    }
    let mut caps: Vec<Cap> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, tree.root_node(), source);
    while let Some(m) = it.next() {
        // Manually enforce `(#eq? @_fn "require")`: if this match has an
        // `@_fn` capture, drop the whole match unless its text is "require".
        let mut fn_text: Option<String> = None;
        let mut has_fn = false;
        for cap in m.captures {
            if capture_names[cap.index as usize] == "_fn" {
                has_fn = true;
                fn_text = cap.node.utf8_text(source).ok().map(|s| s.to_string());
            }
        }
        if has_fn && fn_text.as_deref() != Some("require") {
            continue;
        }
        for cap in m.captures {
            let name = capture_names[cap.index as usize];
            if name == "_fn" {
                continue;
            }
            let node = cap.node;
            let owner_module = if name == "import.symbol" {
                enclosing_import_module(node, source)
            } else {
                None
            };
            let locals = match name {
                "import.module" => import_clause_bindings(node, source),
                "import.symbol" => import_specifier_binding(node, source).into_iter().collect(),
                _ => Vec::new(),
            };
            let statement = std::iter::successors(Some(node), Node::parent)
                .find(|holder| matches!(holder.kind(), "import_statement" | "call_expression"))
                .unwrap_or(node);
            caps.push(Cap {
                name: name.to_string(),
                text: node.utf8_text(source).unwrap_or("").to_string(),
                lines: (statement.start_position().row as i64 + 1, statement.end_position().row as i64 + 1),
                owner_module,
                locals,
            });
        }
    }

    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<Cap>> = HashMap::new();
    for c in caps {
        if !groups.contains_key(&c.name) {
            order.push(c.name.clone());
        }
        groups.entry(c.name.clone()).or_default().push(c);
    }

    let mut imports: Vec<Import> = Vec::new();

    for name in &order {
        for c in &groups[name] {
            match c.name.as_str() {
                "import.module" => {
                    imports.push(Import {
                        module: c.text.clone(),
                        symbol: None,
                        locals: c.locals.clone(),
                        line: c.lines.0,
                        end_line: c.lines.1,
                        block: None,
                        visibility: None,
                    });
                }
                "import.symbol" => {
                    let module = c
                        .owner_module
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    imports.push(Import {
                        module,
                        symbol: Some(c.text.clone()),
                        locals: c.locals.clone(),
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
    // A script's top level is global, and so is a declaration file's; a
    // module's top-level declaration is public when an export names it. The
    // query's imports are `import` statements and `require` calls.
    let global = path.to_lowercase().ends_with(".d.ts") || !is_module(tree.root_node(), source, !imports.is_empty());
    imports.extend(reexport_and_dynamic_imports(tree.root_node(), source));

    let mut declarations = walk_declarations(tree.root_node(), source);
    let exported = exported_names(tree.root_node(), source);
    for declaration in declarations.iter_mut().filter(|row| row.parent.is_none() && row.visibility == Visibility::FileLocal) {
        if global || exported.contains(&declaration.name) {
            declaration.visibility = Visibility::Public;
        }
    }
    let references = walk_references(tree.root_node(), source);

    ExtractionResult {
        language: "typescript".into(),
        imports,
        declarations,
        references,
    }
}

/// Whether the file is a module: it calls `require`, or a top-level
/// statement imports, exports, or assigns a CommonJS export.
fn is_module(root: Node, source: &[u8], requires: bool) -> bool {
    let mut cursor = root.walk();
    let statements = root.named_children(&mut cursor).any(|statement| match statement.kind() {
        "import_statement" | "export_statement" => true,
        _ => commonjs_export(statement, source).is_some(),
    });
    requires || statements
}

/// The assignment of a CommonJS export statement: `module.exports = …`,
/// `module.exports.a = …`, or `exports.a = …`.
fn commonjs_export<'a>(statement: Node<'a>, source: &[u8]) -> Option<Node<'a>> {
    let assignment = statement.named_child(0).filter(|node| statement.kind() == "expression_statement" && node.kind() == "assignment_expression")?;
    let left = assignment.child_by_field_name("left")?.utf8_text(source).ok()?;
    (left == "module.exports" || left.starts_with("exports.") || left.starts_with("module.exports.")).then_some(assignment)
}

/// The top-level names an export names apart from its own declaration:
/// `export { a, b as c }`, `export = a`, `export default a`,
/// `module.exports = a` or `{ a }`, `exports.c = a`, and each shorthand
/// property of an exported object literal, as in `export default { a }`.
fn exported_names(root: Node, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    let text = |node: Node| node.utf8_text(source).unwrap_or_default().to_string();
    let add_value = |value: Node, names: &mut HashSet<String>| match value.kind() {
        "identifier" => {
            names.insert(text(value));
        }
        "object" => {
            let mut cursor = value.walk();
            for property in value.named_children(&mut cursor) {
                if property.kind() == "shorthand_property_identifier" {
                    names.insert(text(property));
                }
            }
        }
        _ => {}
    };
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        match statement.kind() {
            "export_statement" if statement.child_by_field_name("source").is_none() => {
                let mut inner = statement.walk();
                for child in statement.named_children(&mut inner) {
                    match child.kind() {
                        "export_clause" => {
                            let mut specifiers = child.walk();
                            for specifier in child.named_children(&mut specifiers) {
                                if let Some(name) = specifier.child_by_field_name("name") {
                                    names.insert(text(name));
                                }
                            }
                        }
                        "lexical_declaration" | "variable_declaration" => {
                            let mut declarators = child.walk();
                            for declarator in child.named_children(&mut declarators) {
                                if let Some(value) = declarator.child_by_field_name("value") {
                                    add_value(value, &mut names);
                                }
                            }
                        }
                        _ => add_value(child, &mut names),
                    }
                }
            }
            _ => {
                if let Some(value) = commonjs_export(statement, source).and_then(|assignment| assignment.child_by_field_name("right")) {
                    add_value(value, &mut names);
                }
            }
        }
    }
    names
}

/// A top-level declaration an `export` holds directly, `export default`
/// included.
fn directly_exported(node: Node) -> bool {
    let mut up = node.parent();
    while let Some(parent) = up {
        match parent.kind() {
            "export_statement" => return true,
            "lexical_declaration" | "variable_declaration" | "ambient_declaration" => up = parent.parent(),
            _ => return false,
        }
    }
    false
}

/// A row's own visibility as the walk meets it. A class member is public
/// unless `protected` or `private` says otherwise, an interface member is
/// public, a top-level declaration is public when an export holds it, and
/// anything declared inside a function stays in its file. The file's other
/// exports raise the top level afterwards.
fn declared(node: Node, source: &[u8], top_level: bool) -> Visibility {
    match node.kind() {
        "method_definition" | "public_field_definition" | "abstract_method_signature" => {
            let mut cursor = node.walk();
            let modifier = node
                .children(&mut cursor)
                .find(|child| child.kind() == "accessibility_modifier")
                .and_then(|modifier| modifier.utf8_text(source).ok());
            match modifier {
                Some("private") => Visibility::FileLocal,
                Some("protected") => Visibility::Protected,
                _ => Visibility::Public,
            }
        }
        "method_signature" | "property_signature" => Visibility::Public,
        _ if top_level && directly_exported(node) => Visibility::Public,
        _ => Visibility::FileLocal,
    }
}

fn reexport_and_dynamic_imports(root: Node, source: &[u8]) -> Vec<Import> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let source_node = match node.kind() {
            "export_statement" => node.child_by_field_name("source"),
            "call_expression"
                if node
                    .child_by_field_name("function")
                    .is_some_and(|function| function.kind() == "import") =>
            {
                node.child_by_field_name("arguments")
                    .and_then(|arguments| string_fragment(arguments))
            }
            _ => None,
        };
        if let Some(source_node) = source_node.and_then(|node| string_fragment(node).or(Some(node)))
        {
            if let Ok(module) = source_node.utf8_text(source) {
                // `export … from` passes its names on as this file's API.
                let reexport = node.kind() == "export_statement";
                out.push(Import {
                    module: module.to_string(),
                    symbol: None,
                    locals: Vec::new(),
                    line: node.start_position().row as i64 + 1,
                    end_line: node.end_position().row as i64 + 1,
                    block: None,
                    visibility: reexport.then_some(Visibility::Public),
                });
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

fn string_fragment(node: Node) -> Option<Node> {
    if node.kind() == "string_fragment" {
        return Some(node);
    }
    let mut cursor = node.walk();
    let fragment = node.children(&mut cursor).find_map(string_fragment);
    fragment
}

/// Every named declaration in the tree — top-level, nested, and class
/// members. Covers function/class/interface/type/enum/method definitions
/// plus `const`/`let`/`var` declarators (with single-identifier names). A
/// `method_definition` carries the enclosing class name as its `container`;
/// every other declaration's container is `None`.
fn walk_declarations(root: Node, source: &[u8]) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut parent_keys = HashMap::new();
    let mut start_bytes = HashMap::new();
    // Stack carries (node, enclosing-class-name, lexical parent, inside a
    // function body) so a method picks up the class it is declared inside
    // without a second ancestor walk, and a function body's data variables
    // (and whatever they hold) never become rows: they are the body's
    // detail, not the file's shape. Nested functions — declared, or held by
    // a local — and classes still are.
    let mut stack: Vec<(Node, Option<String>, Option<(String, i64)>, bool)> =
        vec![(root, None, None, false)];
    while let Some((n, container, lexical_parent, in_body)) = stack.pop() {
        let holds_function = n.child_by_field_name("value").is_some_and(|value| {
            matches!(value.kind(), "arrow_function" | "function_expression" | "function")
        });
        if in_body && n.kind() == "variable_declarator" && !holds_function {
            continue;
        }
        let kind: Option<&str> = { match n.kind() {
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                Some("function")
            }
            "class_declaration" | "abstract_class_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "type_alias_declaration" => Some("type"),
            "enum_declaration" => Some("enum"),
            "method_definition" => Some("function"),
            "method_signature" => Some("function"),
            "abstract_method_signature" => Some("function"),
            "public_field_definition" => Some("property"),
            "property_signature" => Some("property"),
            "variable_declarator" => Some("constant"),
            _ => None,
        } };
        // The class name this node's children are declared inside. A class
        // node sets the container for everything below it.
        let mut child_container = container.clone();
        let mut child_parent = lexical_parent.clone();
        if let Some(k) = kind {
            let names = if n.kind() == "variable_declarator" {
                typescript_declaration_names(n)
            } else {
                n.child_by_field_name("name").into_iter().collect()
            };
            for name_node in names {
                let name_kind = name_node.kind();
                if name_kind != "identifier"
                    && name_kind != "property_identifier"
                    && name_kind != "type_identifier"
                    && name_kind != "shorthand_property_identifier_pattern"
                {
                    continue;
                }
                if let Ok(name) = name_node.utf8_text(source) {
                    let line = name_node.start_position().row as i64 + 1;
                    let key = (name.to_string(), line);
                    // A method's container is the enclosing class; every
                    // other declaration is a top-level / free symbol.
                    let decl_container = if k == "function" && n.kind() == "method_definition" {
                        container.clone()
                    } else {
                        None
                    };
                    if k == "class" {
                        child_container = Some(name.to_string());
                    }
                    if matches!(
                        n.kind(),
                        "class_declaration"
                            | "abstract_class_declaration"
                            | "interface_declaration"
                            | "enum_declaration"
                            | "function_declaration"
                            | "generator_function_declaration"
                            | "method_definition"
                            | "variable_declarator"
                            | "type_alias_declaration"
                            | "property_signature"
                            | "public_field_definition"
                    ) {
                        child_parent = Some(key.clone());
                    }
                    if seen.insert(key.clone()) {
                        parent_keys.insert(key, lexical_parent.clone());
                        start_bytes.insert((name.to_string(), line), name_node.start_byte());
                        out.push(Declaration {
                            name: name.to_string(),
                            kind: k.to_string(),
                            header_line: typescript_header_line(n),
                            line,
                            end_line: n.end_position().row as i64 + 1,
                            container: decl_container,
                            parent: None,
                            header: typescript_header(n, source),
                            annotations: typescript_annotations(n, source),
                            self_type: None,
                            module_file: None,
                            supertypes: Vec::new(),
                            visibility: declared(n, source, lexical_parent.is_none()),
                        });
                    }
                }
            }
        }
        let body = matches!(
            n.kind(),
            "function_declaration"
                | "generator_function_declaration"
                | "method_definition"
                | "arrow_function"
                | "function_expression"
                | "function"
        )
        .then(|| n.child_by_field_name("body"))
        .flatten();
        let mut c = n.walk();
        for child in n.children(&mut c) {
            let child_in_body = in_body || body.is_some_and(|body| body.id() == child.id());
            stack.push((child, child_container.clone(), child_parent.clone(), child_in_body));
        }
    }
    out.sort_by_key(|declaration| {
        (
            declaration.line,
            start_bytes
                .get(&(declaration.name.clone(), declaration.line))
                .copied()
                .unwrap_or_default(),
        )
    });
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

fn typescript_header(node: Node, source: &[u8]) -> String {
    let start = typescript_header_start(node);
    if node.kind() == "variable_declarator" {
        let declaration = node.parent().unwrap_or(node);
        let statement = declaration
            .parent()
            .filter(|parent| parent.kind() == "export_statement")
            .unwrap_or(declaration);
        let mut builder = crate::extraction::header::Builder::new(source);
        let first_declarator = declaration
            .named_children(&mut declaration.walk())
            .find(|child| child.kind() == "variable_declarator")
            .unwrap_or(node);
        builder.slice(start.start_byte(), first_declarator.start_byte());
        if let Some(value) = node.child_by_field_name("value") {
            let mut bodies = Vec::new();
            collect_typescript_function_bodies(value, &mut bodies);
            if !bodies.is_empty() {
                bodies.sort_by_key(Node::start_byte);
                let mut body_start = node.start_byte();
                for body in bodies {
                    slice_eliding_object_types(&mut builder, node, body_start, body.start_byte());
                    if body.kind() == "statement_block" {
                        builder.block(body);
                    } else {
                        builder.expression(body);
                    }
                    body_start = body.end_byte();
                }
                slice_eliding_object_types(&mut builder, node, body_start, node.end_byte());
                if source.get(statement.end_byte().saturating_sub(1)) == Some(&b';') {
                    builder.slice(statement.end_byte() - 1, statement.end_byte());
                }
                return builder.finish();
            }
        }
        slice_eliding_object_types(&mut builder, node, node.start_byte(), node.end_byte());
        if source.get(statement.end_byte().saturating_sub(1)) == Some(&b';') {
            builder.slice(statement.end_byte() - 1, statement.end_byte());
        }
        return builder.finish();
    }
    let body = node.children(&mut node.walk()).find(|child| {
        matches!(
            child.kind(),
            "class_body" | "interface_body" | "enum_body" | "statement_block"
        )
    });
    let mut builder = crate::extraction::header::Builder::new(source);
    if let Some(body) = body {
        slice_eliding_object_types(&mut builder, start, start.start_byte(), body.start_byte());
        builder.block(body);
    } else if node.kind() == "arrow_function" {
        builder.expression(node);
    } else {
        slice_eliding_object_types(&mut builder, start, start.start_byte(), node.end_byte());
        if source.get(node.end_byte()) == Some(&b';') {
            builder.slice(node.end_byte(), node.end_byte() + 1);
        }
    }
    builder.finish()
}

/// `start..end` into the header with every outermost inline object type cut
/// to `{ … }`: its members are rows of their own, so the header does not
/// print them a second time.
fn slice_eliding_object_types(
    builder: &mut crate::extraction::header::Builder,
    root: Node,
    start: usize,
    end: usize,
) {
    let mut object_types = Vec::new();
    collect_object_types(root, start, end, &mut object_types);
    let mut at = start;
    for object_type in object_types {
        builder.slice(at, object_type.start_byte());
        builder.block(object_type);
        at = object_type.end_byte();
    }
    builder.slice(at, end);
}

fn collect_object_types<'a>(node: Node<'a>, start: usize, end: usize, out: &mut Vec<Node<'a>>) {
    if node.end_byte() <= start || node.start_byte() >= end {
        return;
    }
    if node.kind() == "object_type" && node.start_byte() >= start && node.end_byte() <= end {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_object_types(child, start, end, out);
    }
}

fn typescript_declaration_names(node: Node) -> Vec<Node> {
    let Some(name) = node.child_by_field_name("name") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    collect_typescript_pattern_names(name, &mut names);
    names
}

fn collect_typescript_pattern_names<'a>(node: Node<'a>, names: &mut Vec<Node<'a>>) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => names.push(node),
        "pair_pattern" => {
            if let Some(value) = node.child_by_field_name("value") {
                collect_typescript_pattern_names(value, names);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_typescript_pattern_names(child, names);
            }
        }
    }
}

fn collect_typescript_function_bodies<'a>(node: Node<'a>, bodies: &mut Vec<Node<'a>>) {
    if matches!(
        node.kind(),
        "arrow_function" | "function_expression" | "method_definition"
    ) {
        if let Some(body) = node
            .child_by_field_name("body")
        {
            bodies.push(body);
            return;
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_typescript_function_bodies(child, bodies);
    }
}


fn typescript_header_start(mut node: Node) -> Node {
    if node.kind() == "variable_declarator" {
        node = node.parent().unwrap_or(node);
    }
    if let Some(parent) = node
        .parent()
        .filter(|parent| parent.kind() == "export_statement")
    {
        node = parent;
    }
    while let Some(previous) = node
        .prev_named_sibling()
        .filter(|sibling| sibling.kind() == "decorator")
    {
        node = previous;
    }
    node
}

fn typescript_header_line(node: Node) -> i64 {
    typescript_header_start(node).start_position().row as i64 + 1
}

fn typescript_annotations(node: Node, source: &[u8]) -> Vec<String> {
    let annotation = |decorator: Node| {
        let source = decorator.utf8_text(source).unwrap_or("").to_string();
        let name = source
            .trim_start_matches('@')
            .split(['(', '.'])
            .next()
            .unwrap_or("")
            .to_string();
        name
    };
    let mut cursor = node.walk();
    let mut decorators: Vec<String> = node
        .children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
        .map(annotation)
        .collect();
    if decorators.is_empty() {
        let mut current = node.prev_named_sibling();
        while let Some(decorator) = current.filter(|candidate| candidate.kind() == "decorator") {
            decorators.push(annotation(decorator));
            current = decorator.prev_named_sibling();
        }
        decorators.reverse();
    }
    decorators
}

/// Every `call_expression` and `new_expression` in the tree, stamped with
/// its call shape: a bare `foo()` is `Free`, an `o.foo()` is `Member` (the
/// object text becomes the receiver), and `new Foo()` is `Static` (the
/// class name is the receiver).
fn walk_references(root: Node, source: &[u8]) -> Vec<Reference> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "new_expression" {
            let ctor = n
                .child_by_field_name("constructor")
                .or_else(|| n.child_by_field_name("function"));
            if let Some(ctor) = ctor {
                if let Some((name, line)) = callee_name(ctor, source) {
                    out.push(Reference {
                        name: name.clone(),
                        line,
                        shape: RefShape::Static,
                        receiver: Some(name),
                    });
                }
            }
        } else if n.kind() == "call_expression" {
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

/// The callee name + line for a `new_expression` constructor node.
fn callee_name(node: Node, source: &[u8]) -> Option<(String, i64)> {
    match node.kind() {
        "identifier" | "type_identifier" => {
            let txt = node.utf8_text(source).ok()?;
            Some((txt.to_string(), node.start_position().row as i64 + 1))
        }
        "member_expression" => {
            let prop = node.child_by_field_name("property")?;
            let txt = prop.utf8_text(source).ok()?;
            Some((txt.to_string(), prop.start_position().row as i64 + 1))
        }
        _ => None,
    }
}

/// Name, line, shape, and receiver for a `call_expression`'s function node.
/// A bare identifier is a `Free` call; a `member_expression` is a `Member`
/// call whose receiver is the object text (`this`, a variable, or a longer
/// expression) — the receiver names a value, never a type, so it does not
/// disambiguate the method's class.
fn call_shape(node: Node, source: &[u8]) -> Option<(String, i64, RefShape, Option<String>)> {
    match node.kind() {
        "identifier" | "type_identifier" => {
            let txt = node.utf8_text(source).ok()?;
            Some((
                txt.to_string(),
                node.start_position().row as i64 + 1,
                RefShape::Free,
                None,
            ))
        }
        "member_expression" => {
            let prop = node.child_by_field_name("property")?;
            let txt = prop.utf8_text(source).ok()?;
            let receiver = node
                .child_by_field_name("object")
                .and_then(|o| o.utf8_text(source).ok())
                .map(|s| s.to_string());
            Some((
                txt.to_string(),
                prop.start_position().row as i64 + 1,
                RefShape::Member,
                receiver,
            ))
        }
        _ => None,
    }
}

/// The module string of the `import_statement` that structurally encloses
/// an `import_specifier`. Walks ancestors to the `import_statement`, then
/// reads its `source` string fragment. This is order-independent: in
/// `import { a } from 'm'` the module string follows the specifiers, so a
/// byte-position heuristic would mis-resolve it.
fn enclosing_import_module(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "import_statement" {
            let src = n.child_by_field_name("source")?;
            // `source` is a `string` node; its inner `string_fragment`
            // child holds the bare module path without quotes.
            let mut c = src.walk();
            for child in src.children(&mut c) {
                if child.kind() == "string_fragment" {
                    return child.utf8_text(source).ok().map(|s| s.to_string());
                }
            }
            return None;
        }
        cur = n.parent();
    }
    None
}

fn import_specifier_binding(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let specifier = node.parent()?;
    specifier
        .child_by_field_name("alias")
        .unwrap_or(node)
        .utf8_text(source)
        .ok()
        .map(str::to_string)
}

fn import_clause_bindings(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "import_statement" {
            let mut statement_cursor = parent.walk();
            let Some(clause) = parent
                .children(&mut statement_cursor)
                .find(|child| child.kind() == "import_clause")
            else {
                return Vec::new();
            };
            let mut bindings = Vec::new();
            let mut clause_cursor = clause.walk();
            for child in clause.children(&mut clause_cursor) {
                if child.kind() == "identifier" {
                    if let Ok(binding) = child.utf8_text(source) {
                        bindings.push(binding.to_string());
                    }
                }
                if child.kind() == "namespace_import" {
                    let mut namespace_cursor = child.walk();
                    let binding_node = child
                        .children(&mut namespace_cursor)
                        .find(|part| part.kind() == "identifier");
                    if let Some(binding) = binding_node.and_then(|part| part.utf8_text(source).ok())
                    {
                        bindings.push(binding.to_string());
                    }
                }
            }
            return bindings;
        }
        current = parent.parent();
    }
    Vec::new()
}
