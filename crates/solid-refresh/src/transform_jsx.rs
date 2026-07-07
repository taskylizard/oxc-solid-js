//! JSX expression extraction for granular HMR.
//!
//! Upstream: `solid-refresh/src/babel/core/transform-jsx.ts`
//!
//! For each JSX element/fragment in the program, extracts dynamic expressions
//! (attribute values, expression containers, spread children, component names)
//! into a generated "template component", replacing the original JSX with
//! `<TemplateName v0={expr0} v1={expr1} ... />`.

use std::collections::HashSet;

use oxc_allocator::{Allocator, CloneIn};
use oxc_ast::ast::*;
use oxc_ast::{AstBuilder, NONE};
use oxc_span::SPAN;

use crate::checks::is_component_ish_name;
use crate::unique_name::generate_unique_name;

/// Mutable state accumulated while extracting expressions from a single JSX tree.
struct JsxState<'a> {
    /// Name of the props parameter (e.g. `"_props"`).
    props_name: &'a str,
    /// Collected `vN={expr}` attributes for the replacement element.
    attributes: Vec<JSXAttributeItem<'a>>,
    /// `use:` directive variable declarators that go into the template body block.
    vars: Vec<VariableDeclarator<'a>>,
    /// Running counter for the next `vN` key.
    var_count: usize,
}

// ---------------------------------------------------------------------------
// Helper: push attribute / build _props.vN
// ---------------------------------------------------------------------------

/// Pushes `vN={replacement}` onto `state.attributes`, returns the key name (e.g. `"v0"`).
fn push_attribute<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    replacement: Expression<'a>,
) -> &'a str {
    let key_string = format!("v{}", state.var_count);
    state.var_count += 1;
    let key: &'a str = ast.allocator.alloc_str(&key_string);

    let jsx_attr = ast.jsx_attribute(
        SPAN,
        JSXAttributeName::Identifier(ast.alloc(ast.jsx_identifier(SPAN, key))),
        Some(JSXAttributeValue::ExpressionContainer(ast.alloc(
            ast.jsx_expression_container(SPAN, JSXExpression::from(replacement)),
        ))),
    );
    state
        .attributes
        .push(JSXAttributeItem::Attribute(ast.alloc(jsx_attr)));
    key
}

/// Builds the expression `<props_name>.<key>` (a static member expression).
fn build_props_member<'a>(
    ast: AstBuilder<'a>,
    props_name: &'a str,
    key: &'a str,
) -> Expression<'a> {
    Expression::StaticMemberExpression(ast.alloc_static_member_expression(
        SPAN,
        ast.expression_identifier(SPAN, props_name),
        ast.identifier_name(SPAN, key),
        false,
    ))
}

/// Pushes the replacement expression as a `vN` attribute and returns the
/// `_props.vN` expression that should replace the original.
fn push_attribute_and_get_replacement<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    replacement: Expression<'a>,
) -> Expression<'a> {
    let key = push_attribute(state, ast, replacement);
    build_props_member(ast, state.props_name, key)
}

// ---------------------------------------------------------------------------
// Collect top-level binding names (lightweight substitute for scope analysis)
// ---------------------------------------------------------------------------

/// Collects all names bound at the top level of the program — imports, `const`/
/// `let`/`var` declarations, function declarations, and class declarations.
/// Used to decide whether a component name reference is "top-level" (and thus
/// should NOT be extracted into template props).
fn collect_top_level_names<'a>(program: &Program<'a>) -> HashSet<&'a str> {
    let mut names = HashSet::new();
    for stmt in &program.body {
        collect_names_from_statement(&mut names, stmt);
    }
    names
}

fn collect_names_from_statement<'a>(names: &mut HashSet<&'a str>, stmt: &Statement<'a>) {
    match stmt {
        Statement::ImportDeclaration(import) => {
            if let Some(specifiers) = &import.specifiers {
                for spec in specifiers {
                    match spec {
                        ImportDeclarationSpecifier::ImportSpecifier(s) => {
                            names.insert(s.local.name.as_str());
                        }
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                            names.insert(s.local.name.as_str());
                        }
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                            names.insert(s.local.name.as_str());
                        }
                    }
                }
            }
        }
        Statement::VariableDeclaration(var_decl) => {
            for decl in &var_decl.declarations {
                if let BindingPattern::BindingIdentifier(id) = &decl.id {
                    names.insert(id.name.as_str());
                }
            }
        }
        Statement::FunctionDeclaration(func) => {
            if let Some(id) = &func.id {
                names.insert(id.name.as_str());
            }
        }
        Statement::ClassDeclaration(cls) => {
            if let Some(id) = &cls.id {
                names.insert(id.name.as_str());
            }
        }
        Statement::ExportNamedDeclaration(export) => {
            if let Some(decl) = &export.declaration {
                match decl {
                    Declaration::VariableDeclaration(var_decl) => {
                        for d in &var_decl.declarations {
                            if let BindingPattern::BindingIdentifier(id) = &d.id {
                                names.insert(id.name.as_str());
                            }
                        }
                    }
                    Declaration::FunctionDeclaration(func) => {
                        if let Some(id) = &func.id {
                            names.insert(id.name.as_str());
                        }
                    }
                    Declaration::ClassDeclaration(cls) => {
                        if let Some(id) = &cls.id {
                            names.insert(id.name.as_str());
                        }
                    }
                    _ => {}
                }
            }
        }
        Statement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                if let Some(id) = &func.id {
                    names.insert(id.name.as_str());
                }
            }
            ExportDefaultDeclarationKind::ClassDeclaration(cls) => {
                if let Some(id) = &cls.id {
                    names.insert(id.name.as_str());
                }
            }
            _ => {}
        },
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// shouldSkipJSX — check @refresh jsx-skip comment
// ---------------------------------------------------------------------------

/// Returns `true` if the JSX node has a leading `@refresh jsx-skip` comment.
///
/// In OXC, comments are stored separately in `program.comments`. We check if
/// any comment whose `attached_to` position falls on the node's span start
/// contains the skip directive.
fn should_skip_jsx(span_start: u32, comments: &[Comment], source_text: &str) -> bool {
    for comment in comments {
        if comment.attached_to == span_start {
            let content_span = comment.content_span();
            let start = content_span.start as usize;
            let end = content_span.end as usize;
            if let Some(text) = source_text.get(start..end) {
                if text.trim() == "@refresh jsx-skip" {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// JSX name → Expression conversion
// ---------------------------------------------------------------------------

/// Converts a `JSXElementName` to an `Expression` for pushing as an attribute value.
///
/// Only handles `IdentifierReference` (uppercase component) and `MemberExpression`
/// (dotted component like `Foo.Bar`). Other variants (lowercase identifiers,
/// namespaced names, `this`) are not extracted.
fn jsx_element_name_to_expression<'a>(
    ast: AstBuilder<'a>,
    name: &JSXElementName<'a>,
) -> Option<Expression<'a>> {
    match name {
        JSXElementName::IdentifierReference(id) => {
            Some(ast.expression_identifier(SPAN, ast.allocator.alloc_str(id.name.as_str())))
        }
        JSXElementName::MemberExpression(member) => Some(jsx_member_to_expression(ast, member)),
        _ => None,
    }
}

/// Recursively converts a `JSXMemberExpression` to a standard `MemberExpression`.
fn jsx_member_to_expression<'a>(
    ast: AstBuilder<'a>,
    member: &JSXMemberExpression<'a>,
) -> Expression<'a> {
    let object = match &member.object {
        JSXMemberExpressionObject::IdentifierReference(id) => {
            ast.expression_identifier(SPAN, ast.allocator.alloc_str(id.name.as_str()))
        }
        JSXMemberExpressionObject::MemberExpression(inner) => jsx_member_to_expression(ast, inner),
        JSXMemberExpressionObject::ThisExpression(_) => ast.expression_this(SPAN),
    };
    let property =
        ast.identifier_name(SPAN, ast.allocator.alloc_str(member.property.name.as_str()));
    Expression::StaticMemberExpression(
        ast.alloc_static_member_expression(SPAN, object, property, false),
    )
}

// ---------------------------------------------------------------------------
// Expression → JSXMemberExpression (for replacing element names)
// ---------------------------------------------------------------------------

/// Builds `<_props.vN>` as a `JSXMemberExpression` for replacing opening/closing
/// element names.
fn build_jsx_member_name<'a>(
    ast: AstBuilder<'a>,
    props_name: &'a str,
    key: &'a str,
) -> JSXElementName<'a> {
    let object = JSXMemberExpressionObject::IdentifierReference(
        ast.alloc(ast.identifier_reference(SPAN, props_name)),
    );
    let property = ast.jsx_identifier(SPAN, key);
    JSXElementName::MemberExpression(ast.alloc(ast.jsx_member_expression(SPAN, object, property)))
}

// ---------------------------------------------------------------------------
// Descriptive name from statement (no TraverseCtx)
// ---------------------------------------------------------------------------

/// Extracts a descriptive name from the program statement that contains the JSX.
///
/// Checks for function declaration names, variable declarator names, and export
/// wrappers. Falls back to `"template"`.
fn get_descriptive_name_from_statement<'a>(stmt: &'a Statement<'a>) -> &'a str {
    match stmt {
        Statement::FunctionDeclaration(func) => {
            if let Some(id) = &func.id {
                return id.name.as_str();
            }
            "template"
        }
        Statement::VariableDeclaration(var_decl) => {
            for decl in &var_decl.declarations {
                if let BindingPattern::BindingIdentifier(id) = &decl.id {
                    return id.name.as_str();
                }
            }
            "template"
        }
        Statement::ExportNamedDeclaration(export) => {
            if let Some(decl) = &export.declaration {
                match decl {
                    Declaration::FunctionDeclaration(func) => {
                        if let Some(id) = &func.id {
                            return id.name.as_str();
                        }
                    }
                    Declaration::VariableDeclaration(var_decl) => {
                        for d in &var_decl.declarations {
                            if let BindingPattern::BindingIdentifier(id) = &d.id {
                                return id.name.as_str();
                            }
                        }
                    }
                    _ => {}
                }
            }
            "template"
        }
        Statement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                if let Some(id) = &func.id {
                    return id.name.as_str();
                }
                "template"
            }
            _ => "template",
        },
        _ => "template",
    }
}

// ---------------------------------------------------------------------------
// Expression extraction functions (mirror upstream)
// ---------------------------------------------------------------------------

/// Component name pattern: starts with [A-Z_].
fn is_component_name_pattern(name: &str) -> bool {
    name.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_uppercase() || *b == b'_')
}

/// Entry point: extract expressions from a JSXElement or JSXFragment.
fn extract_jsx_expressions<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    top_level_names: &HashSet<&str>,
    node: &mut JSXElementOrFragment<'a, '_>,
) {
    match node {
        JSXElementOrFragment::Element(element) => {
            extract_from_jsx_element(state, ast, top_level_names, element);
            extract_from_attributes(state, ast, element);
        }
        JSXElementOrFragment::Fragment(_) => {}
    }
    let children = match node {
        JSXElementOrFragment::Element(el) => &mut el.children,
        JSXElementOrFragment::Fragment(frag) => &mut frag.children,
    };
    let len = children.len();
    for i in 0..len {
        match &mut children[i] {
            JSXChild::Element(child_el) => {
                let mut wrapper = JSXElementOrFragment::Element(child_el);
                extract_jsx_expressions(state, ast, top_level_names, &mut wrapper);
            }
            JSXChild::Fragment(child_frag) => {
                let mut wrapper = JSXElementOrFragment::Fragment(child_frag);
                extract_jsx_expressions(state, ast, top_level_names, &mut wrapper);
            }
            JSXChild::ExpressionContainer(container) => {
                extract_from_expression_container(state, ast, container);
            }
            JSXChild::Spread(spread) => {
                extract_from_spread_child(state, ast, spread);
            }
            JSXChild::Text(_) => {}
        }
    }
}

/// Wrapper enum to handle both JSXElement and JSXFragment uniformly.
enum JSXElementOrFragment<'a, 'b>
where
    'a: 'b,
{
    Element(&'b mut JSXElement<'a>),
    Fragment(&'b mut JSXFragment<'a>),
}

/// Extract component name if it's dynamic (uppercase identifier not at top level,
/// or member expression).
fn extract_from_jsx_element<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    top_level_names: &HashSet<&str>,
    element: &mut JSXElement<'a>,
) {
    let should_extract = match &element.opening_element.name {
        JSXElementName::IdentifierReference(id) => {
            // Uppercase or underscore-prefixed identifier
            if is_component_name_pattern(id.name.as_str()) {
                // Only extract if NOT a top-level binding
                !top_level_names.contains(id.name.as_str())
            } else {
                false
            }
        }
        JSXElementName::MemberExpression(_) => true,
        _ => false,
    };

    if !should_extract {
        return;
    }

    // Convert JSX element name → expression for the attribute value
    let expr = match jsx_element_name_to_expression(ast, &element.opening_element.name) {
        Some(e) => e,
        None => return,
    };

    let key = push_attribute(state, ast, expr);

    // Replace opening element name with _props.vN
    let replacement_name = build_jsx_member_name(ast, state.props_name, key);
    element.opening_element.name = replacement_name;

    // Replace closing element name too
    if let Some(ref mut closing) = element.closing_element {
        closing.name = build_jsx_member_name(ast, state.props_name, key);
    }
}

/// Extract expressions from element attributes.
fn extract_from_attributes<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    element: &mut JSXElement<'a>,
) {
    let attrs_len = element.opening_element.attributes.len();
    for i in 0..attrs_len {
        match &element.opening_element.attributes[i] {
            JSXAttributeItem::Attribute(attr) => {
                // Check attribute name type
                match &attr.name {
                    JSXAttributeName::Identifier(id) if id.name.as_str() == "ref" => {
                        extract_from_ref(state, ast, element, i);
                    }
                    JSXAttributeName::NamespacedName(ns) if ns.namespace.name.as_str() == "use" => {
                        extract_from_use_directive(state, ast, element, i);
                    }
                    _ => {
                        extract_from_normal_attribute(state, ast, element, i);
                    }
                }
            }
            JSXAttributeItem::SpreadAttribute(_) => {
                extract_from_spread_attribute(state, ast, element, i);
            }
        }
    }
}

/// Extract from a normal attribute value.
fn extract_from_normal_attribute<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    element: &mut JSXElement<'a>,
    attr_idx: usize,
) {
    let JSXAttributeItem::Attribute(attr) = &mut element.opening_element.attributes[attr_idx]
    else {
        return;
    };

    // If value is JSXElement or JSXFragment, wrap in expression container first
    let needs_wrap = matches!(
        &attr.value,
        Some(JSXAttributeValue::Element(_)) | Some(JSXAttributeValue::Fragment(_))
    );

    if needs_wrap {
        let old_value = attr.value.take();
        if let Some(value) = old_value {
            let expr = match value {
                JSXAttributeValue::Element(el) => Expression::JSXElement(el),
                JSXAttributeValue::Fragment(frag) => Expression::JSXFragment(frag),
                other => {
                    attr.value = Some(other);
                    return;
                }
            };
            attr.value = Some(JSXAttributeValue::ExpressionContainer(
                ast.alloc(ast.jsx_expression_container(SPAN, JSXExpression::from(expr))),
            ));
        }
    }

    // If value is expression container, extract the expression
    let is_expression_container =
        matches!(&attr.value, Some(JSXAttributeValue::ExpressionContainer(_)));

    if is_expression_container {
        let JSXAttributeItem::Attribute(attr) = &mut element.opening_element.attributes[attr_idx]
        else {
            return;
        };
        if let Some(JSXAttributeValue::ExpressionContainer(container)) = &mut attr.value {
            // Extract expression and replace with _props.vN
            let old_expr = std::mem::replace(
                &mut container.expression,
                ast.jsx_expression_empty_expression(SPAN),
            );
            if let Some(expr) = jsx_expression_to_expression(old_expr) {
                let replacement = push_attribute_and_get_replacement(state, ast, expr);
                container.expression = JSXExpression::from(replacement);
            }
        }
    }
}

/// Convert JSXExpression to Option<Expression> (filtering out EmptyExpression).
fn jsx_expression_to_expression<'a>(expr: JSXExpression<'a>) -> Option<Expression<'a>> {
    match expr {
        JSXExpression::EmptyExpression(_) => None,
        _ => Some(expr.into_expression()),
    }
}

/// Extract from a `ref` attribute: wraps identifier refs in
/// `(el) => typeof x === 'function' ? x(el) : x = el`.
fn extract_from_ref<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    element: &mut JSXElement<'a>,
    attr_idx: usize,
) {
    let JSXAttributeItem::Attribute(attr) = &mut element.opening_element.attributes[attr_idx]
    else {
        return;
    };

    let is_expression_container =
        matches!(&attr.value, Some(JSXAttributeValue::ExpressionContainer(_)));
    if !is_expression_container {
        return;
    }

    let Some(JSXAttributeValue::ExpressionContainer(container)) = &mut attr.value else {
        return;
    };

    // Take the expression out
    let old_expr = std::mem::replace(
        &mut container.expression,
        ast.jsx_expression_empty_expression(SPAN),
    );

    let Some(expr) = jsx_expression_to_expression(old_expr) else {
        return;
    };

    // Check if it's an identifier (possibly wrapped in TS casts / parens)
    let unwrapped = crate::unwrap::unwrap_expression(&expr);
    let is_identifier = matches!(unwrapped, Expression::Identifier(_));

    let replacement = if is_identifier {
        // Build: (arg) => { if (typeof x === 'function') { x(arg); } else { x = arg; } }
        // Extract the identifier name
        let ident_name = match unwrapped {
            Expression::Identifier(id) => id.name.as_str(),
            _ => "",
        };
        let ident_str: &'a str = ast.allocator.alloc_str(ident_name);
        let arg_name: &'a str = "_arg";

        // typeof x
        let typeof_expr = ast.expression_unary(
            SPAN,
            UnaryOperator::Typeof,
            ast.expression_identifier(SPAN, ident_str),
        );
        // typeof x === 'function'
        let test = ast.expression_binary(
            SPAN,
            typeof_expr,
            BinaryOperator::StrictEquality,
            ast.expression_string_literal(SPAN, "function", None),
        );
        // x(arg)
        let call_expr = ast.expression_call(
            SPAN,
            ast.expression_identifier(SPAN, ident_str),
            NONE,
            ast.vec1(Argument::from(ast.expression_identifier(SPAN, arg_name))),
            false,
        );
        let consequent = Statement::BlockStatement(
            ast.alloc_block_statement(SPAN, ast.vec1(ast.statement_expression(SPAN, call_expr))),
        );
        // x = arg (safe default — dead code for const/signal refs)
        let assign_expr = ast.expression_assignment(
            SPAN,
            AssignmentOperator::Assign,
            ast.simple_assignment_target_assignment_target_identifier(SPAN, ident_str)
                .into(),
            ast.expression_identifier(SPAN, arg_name),
        );
        let alternate = Statement::BlockStatement(
            ast.alloc_block_statement(SPAN, ast.vec1(ast.statement_expression(SPAN, assign_expr))),
        );
        let if_stmt = ast.statement_if(SPAN, test, consequent, Some(alternate));

        let body = ast.alloc_function_body(SPAN, ast.vec(), ast.vec1(if_stmt));
        let param = ast
            .plain_formal_parameter(SPAN, ast.binding_pattern_binding_identifier(SPAN, arg_name));
        let params = ast.alloc_formal_parameters(
            SPAN,
            FormalParameterKind::ArrowFormalParameters,
            ast.vec1(param),
            NONE,
        );

        ast.expression_arrow_function(SPAN, true, false, NONE, params, NONE, body)
    } else {
        // Not an identifier — just use the expression directly
        expr
    };

    // Push as attribute and replace the expression
    let props_member = push_attribute_and_get_replacement(state, ast, replacement);
    container.expression = JSXExpression::from(props_member);
}

/// Extract from a `use:directive` attribute.
fn extract_from_use_directive<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    element: &mut JSXElement<'a>,
    attr_idx: usize,
) {
    let JSXAttributeItem::Attribute(attr) = &mut element.opening_element.attributes[attr_idx]
    else {
        return;
    };

    // Get the directive name (the part after `use:`)
    let directive_name: &'a str = match &attr.name {
        JSXAttributeName::NamespacedName(ns) => ast.allocator.alloc_str(ns.name.name.as_str()),
        _ => return,
    };

    // First, extract expression from the value if it's an expression container
    if let Some(JSXAttributeValue::ExpressionContainer(container)) = &mut attr.value {
        let old_expr = std::mem::replace(
            &mut container.expression,
            ast.jsx_expression_empty_expression(SPAN),
        );
        if let Some(expr) = jsx_expression_to_expression(old_expr) {
            let replacement = push_attribute_and_get_replacement(state, ast, expr);
            container.expression = JSXExpression::from(replacement);
        }
    }

    // Push the directive identifier as an attribute
    let directive_ident = ast.expression_identifier(SPAN, directive_name);
    let key = push_attribute(state, ast, directive_ident);

    // Add variable declarator: const <directive_name> = _props.<key>
    let binding = ast.binding_pattern_binding_identifier(SPAN, directive_name);
    let init = build_props_member(ast, state.props_name, key);
    let declarator = ast.variable_declarator(
        SPAN,
        VariableDeclarationKind::Const,
        binding,
        NONE,
        Some(init),
        false,
    );
    state.vars.push(declarator);
}

/// Extract from a spread attribute: `{...expr}`.
fn extract_from_spread_attribute<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    element: &mut JSXElement<'a>,
    attr_idx: usize,
) {
    let JSXAttributeItem::SpreadAttribute(spread) =
        &mut element.opening_element.attributes[attr_idx]
    else {
        return;
    };

    let old_arg = std::mem::replace(&mut spread.argument, ast.expression_null_literal(SPAN));
    let replacement = push_attribute_and_get_replacement(state, ast, old_arg);
    spread.argument = replacement;
}

/// Extract from a JSXExpressionContainer child: `{expr}`.
fn extract_from_expression_container<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    container: &mut JSXExpressionContainer<'a>,
) {
    let old_expr = std::mem::replace(
        &mut container.expression,
        ast.jsx_expression_empty_expression(SPAN),
    );
    if let Some(expr) = jsx_expression_to_expression(old_expr) {
        let replacement = push_attribute_and_get_replacement(state, ast, expr);
        container.expression = JSXExpression::from(replacement);
    }
}

/// Extract from a JSXSpreadChild: `{...expr}`.
fn extract_from_spread_child<'a>(
    state: &mut JsxState<'a>,
    ast: AstBuilder<'a>,
    spread: &mut JSXSpreadChild<'a>,
) {
    let old_expr = std::mem::replace(&mut spread.expression, ast.expression_null_literal(SPAN));
    let replacement = push_attribute_and_get_replacement(state, ast, old_expr);
    spread.expression = replacement;
}

// ---------------------------------------------------------------------------
// JSX finder: walk statements/expressions to find JSX
// ---------------------------------------------------------------------------

/// Checks whether a statement contains any JSX expressions (shallow check on
/// the statement kind — deeper check done during the actual transform).
fn statement_may_contain_jsx(stmt: &Statement<'_>) -> bool {
    // We look for JSX in function bodies, variable initializers, etc.
    // Rather than doing a full walk here, we just return true for statements
    // that could possibly contain expressions.
    !matches!(
        stmt,
        Statement::ImportDeclaration(_)
            | Statement::EmptyStatement(_)
            | Statement::BreakStatement(_)
            | Statement::ContinueStatement(_)
            | Statement::DebuggerStatement(_)
    )
}

// ---------------------------------------------------------------------------
// Main transform: process all JSX in the program
// ---------------------------------------------------------------------------

/// Entry point: processes all JSX in the program body.
///
/// Called from `transform.rs` Phase 2 when `state.jsx` is `true`.
pub fn transform_all_jsx<'a>(
    allocator: &'a Allocator,
    source_text: &'a str,
    used_names: &mut HashSet<String>,
    program: &mut Program<'a>,
) {
    let top_level_names = collect_top_level_names(program);

    // We process root statements from the end so insertions don't shift indices
    // for not-yet-processed statements.
    let mut root_idx = program.body.len();
    while root_idx > 0 {
        root_idx -= 1;

        if !statement_may_contain_jsx(&program.body[root_idx]) {
            continue;
        }

        // Find and transform JSX in this statement. We may need multiple passes
        // because a single statement can contain multiple JSX trees (e.g.
        // ternary branches).
        let found = find_and_transform_jsx_in_statement(
            allocator,
            source_text,
            used_names,
            &top_level_names,
            program,
            root_idx,
        );

        // Insert template declarations before the root statement.
        // Insert in reverse so they appear in the order they were found.
        for insertion in found.into_iter().rev() {
            program.body.insert(root_idx, insertion);
        }
    }
}

/// Process one root statement: find all JSX, extract expressions, build templates.
///
/// Returns a list of template `const` declarations to insert before the root
/// statement.
fn find_and_transform_jsx_in_statement<'a>(
    allocator: &'a Allocator,
    source_text: &'a str,
    used_names: &mut HashSet<String>,
    top_level_names: &HashSet<&str>,
    program: &mut Program<'a>,
    root_idx: usize,
) -> Vec<Statement<'a>> {
    let ast = AstBuilder::new(allocator);

    // Get descriptive name from the statement
    let desc_name = get_descriptive_name_from_statement(&program.body[root_idx]);
    let desc_name_owned = desc_name.to_string();

    // Collect JSX locations within this statement, then process each.
    // We use a recursive approach that finds and transforms JSX expressions
    // directly within the statement's expression tree.
    let mut insertions: Vec<Statement<'a>> = Vec::new();

    // Process the statement in place
    process_statement_jsx(
        ast,
        allocator,
        source_text,
        used_names,
        top_level_names,
        &mut program.body[root_idx],
        &mut program.comments,
        &desc_name_owned,
        &mut insertions,
    );

    insertions
}

/// Recursively processes a statement to find and transform JSX.
fn process_statement_jsx<'a>(
    ast: AstBuilder<'a>,
    allocator: &'a Allocator,
    source_text: &'a str,
    used_names: &mut HashSet<String>,
    top_level_names: &HashSet<&str>,
    stmt: &mut Statement<'a>,
    comments: &mut oxc_allocator::Vec<'a, Comment>,
    desc_name: &str,
    insertions: &mut Vec<Statement<'a>>,
) {
    match stmt {
        Statement::VariableDeclaration(var_decl) => {
            for decl in var_decl.declarations.iter_mut() {
                if let Some(ref mut init) = decl.init {
                    process_expression_jsx(
                        ast,
                        allocator,
                        source_text,
                        used_names,
                        top_level_names,
                        init,
                        comments,
                        desc_name,
                        insertions,
                    );
                }
            }
        }
        Statement::ExpressionStatement(expr_stmt) => {
            process_expression_jsx(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut expr_stmt.expression,
                comments,
                desc_name,
                insertions,
            );
        }
        Statement::ReturnStatement(ret) => {
            if let Some(ref mut arg) = ret.argument {
                process_expression_jsx(
                    ast,
                    allocator,
                    source_text,
                    used_names,
                    top_level_names,
                    arg,
                    comments,
                    desc_name,
                    insertions,
                );
            }
        }
        Statement::FunctionDeclaration(func) => {
            // Get name for descriptive purposes
            let fn_name = func
                .id
                .as_ref()
                .map_or(desc_name.to_string(), |id| id.name.to_string());
            if let Some(ref mut body) = func.body {
                for body_stmt in body.statements.iter_mut() {
                    process_statement_jsx(
                        ast,
                        allocator,
                        source_text,
                        used_names,
                        top_level_names,
                        body_stmt,
                        comments,
                        &fn_name,
                        insertions,
                    );
                }
            }
        }
        Statement::ExportNamedDeclaration(export) => {
            if let Some(ref mut decl) = export.declaration {
                process_declaration_jsx(
                    ast,
                    allocator,
                    source_text,
                    used_names,
                    top_level_names,
                    decl,
                    comments,
                    desc_name,
                    insertions,
                );
            }
        }
        Statement::ExportDefaultDeclaration(export) => {
            match &mut export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                    let fn_name = func
                        .id
                        .as_ref()
                        .map_or(desc_name.to_string(), |id| id.name.to_string());
                    if let Some(ref mut body) = func.body {
                        for body_stmt in body.statements.iter_mut() {
                            process_statement_jsx(
                                ast,
                                allocator,
                                source_text,
                                used_names,
                                top_level_names,
                                body_stmt,
                                comments,
                                &fn_name,
                                insertions,
                            );
                        }
                    }
                }
                ExportDefaultDeclarationKind::ClassDeclaration(_)
                | ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => {}
                _ => {
                    // Expression default exports
                    if let Some(expr) = export.declaration.as_expression_mut() {
                        process_expression_jsx(
                            ast,
                            allocator,
                            source_text,
                            used_names,
                            top_level_names,
                            expr,
                            comments,
                            desc_name,
                            insertions,
                        );
                    }
                }
            }
        }
        Statement::IfStatement(if_stmt) => {
            process_statement_jsx(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut if_stmt.consequent,
                comments,
                desc_name,
                insertions,
            );
            if let Some(ref mut alternate) = if_stmt.alternate {
                process_statement_jsx(
                    ast,
                    allocator,
                    source_text,
                    used_names,
                    top_level_names,
                    alternate,
                    comments,
                    desc_name,
                    insertions,
                );
            }
        }
        Statement::BlockStatement(block) => {
            for body_stmt in block.body.iter_mut() {
                process_statement_jsx(
                    ast,
                    allocator,
                    source_text,
                    used_names,
                    top_level_names,
                    body_stmt,
                    comments,
                    desc_name,
                    insertions,
                );
            }
        }
        _ => {}
    }
}

/// Process declarations within export statements.
fn process_declaration_jsx<'a>(
    ast: AstBuilder<'a>,
    allocator: &'a Allocator,
    source_text: &'a str,
    used_names: &mut HashSet<String>,
    top_level_names: &HashSet<&str>,
    decl: &mut Declaration<'a>,
    comments: &mut oxc_allocator::Vec<'a, Comment>,
    desc_name: &str,
    insertions: &mut Vec<Statement<'a>>,
) {
    match decl {
        Declaration::VariableDeclaration(var_decl) => {
            for d in var_decl.declarations.iter_mut() {
                if let Some(ref mut init) = d.init {
                    process_expression_jsx(
                        ast,
                        allocator,
                        source_text,
                        used_names,
                        top_level_names,
                        init,
                        comments,
                        desc_name,
                        insertions,
                    );
                }
            }
        }
        Declaration::FunctionDeclaration(func) => {
            let fn_name = func
                .id
                .as_ref()
                .map_or(desc_name.to_string(), |id| id.name.to_string());
            if let Some(ref mut body) = func.body {
                for body_stmt in body.statements.iter_mut() {
                    process_statement_jsx(
                        ast,
                        allocator,
                        source_text,
                        used_names,
                        top_level_names,
                        body_stmt,
                        comments,
                        &fn_name,
                        insertions,
                    );
                }
            }
        }
        _ => {}
    }
}

/// Recursively process an expression to find and transform JSX.
///
/// When a JSX element/fragment is found:
/// 1. Clone it (the clone becomes the template body)
/// 2. Extract expressions from the clone (mutating it to use `_props.vN`)
/// 3. Build a template component declaration
/// 4. Replace the original JSX with `<TemplateName v0={...} />`
fn process_expression_jsx<'a>(
    ast: AstBuilder<'a>,
    allocator: &'a Allocator,
    source_text: &'a str,
    used_names: &mut HashSet<String>,
    top_level_names: &HashSet<&str>,
    expr: &mut Expression<'a>,
    comments: &mut oxc_allocator::Vec<'a, Comment>,
    desc_name: &str,
    insertions: &mut Vec<Statement<'a>>,
) {
    match expr {
        Expression::JSXElement(_) | Expression::JSXFragment(_) => {
            // Check @refresh jsx-skip
            let span_start = match expr {
                Expression::JSXElement(el) => el.span.start,
                Expression::JSXFragment(frag) => frag.span.start,
                _ => return,
            };
            if should_skip_jsx(span_start, comments, source_text) {
                return;
            }

            transform_single_jsx(
                ast,
                allocator,
                used_names,
                top_level_names,
                expr,
                desc_name,
                insertions,
            );
        }
        Expression::ArrowFunctionExpression(arrow) => {
            // Recurse into arrow body
            if arrow.expression {
                // Expression body: single expression
                if let Some(expr_stmt) = arrow.body.statements.first_mut() {
                    if let Statement::ExpressionStatement(es) = expr_stmt {
                        process_expression_jsx(
                            ast,
                            allocator,
                            source_text,
                            used_names,
                            top_level_names,
                            &mut es.expression,
                            comments,
                            desc_name,
                            insertions,
                        );
                    }
                }
            } else {
                for body_stmt in arrow.body.statements.iter_mut() {
                    process_statement_jsx_inner(
                        ast,
                        allocator,
                        source_text,
                        used_names,
                        top_level_names,
                        body_stmt,
                        comments,
                        desc_name,
                        insertions,
                    );
                }
            }
        }
        Expression::FunctionExpression(func) => {
            let fn_name = func
                .id
                .as_ref()
                .map_or(desc_name.to_string(), |id| id.name.to_string());
            if let Some(ref mut body) = func.body {
                for body_stmt in body.statements.iter_mut() {
                    process_statement_jsx_inner(
                        ast,
                        allocator,
                        source_text,
                        used_names,
                        top_level_names,
                        body_stmt,
                        comments,
                        &fn_name,
                        insertions,
                    );
                }
            }
        }
        Expression::ConditionalExpression(cond) => {
            process_expression_jsx(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut cond.consequent,
                comments,
                desc_name,
                insertions,
            );
            process_expression_jsx(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut cond.alternate,
                comments,
                desc_name,
                insertions,
            );
        }
        Expression::LogicalExpression(logical) => {
            process_expression_jsx(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut logical.left,
                comments,
                desc_name,
                insertions,
            );
            process_expression_jsx(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut logical.right,
                comments,
                desc_name,
                insertions,
            );
        }
        Expression::CallExpression(call) => {
            for arg in call.arguments.iter_mut() {
                if let Some(e) = arg.as_expression_mut() {
                    process_expression_jsx(
                        ast,
                        allocator,
                        source_text,
                        used_names,
                        top_level_names,
                        e,
                        comments,
                        desc_name,
                        insertions,
                    );
                }
            }
        }
        Expression::ParenthesizedExpression(paren) => {
            process_expression_jsx(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut paren.expression,
                comments,
                desc_name,
                insertions,
            );
        }
        Expression::SequenceExpression(seq) => {
            for e in seq.expressions.iter_mut() {
                process_expression_jsx(
                    ast,
                    allocator,
                    source_text,
                    used_names,
                    top_level_names,
                    e,
                    comments,
                    desc_name,
                    insertions,
                );
            }
        }
        Expression::AssignmentExpression(assign) => {
            process_expression_jsx(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut assign.right,
                comments,
                desc_name,
                insertions,
            );
        }
        _ => {}
    }
}

/// Inner statement walker (for function bodies, blocks inside expressions).
fn process_statement_jsx_inner<'a>(
    ast: AstBuilder<'a>,
    allocator: &'a Allocator,
    source_text: &'a str,
    used_names: &mut HashSet<String>,
    top_level_names: &HashSet<&str>,
    stmt: &mut Statement<'a>,
    comments: &mut oxc_allocator::Vec<'a, Comment>,
    desc_name: &str,
    insertions: &mut Vec<Statement<'a>>,
) {
    match stmt {
        Statement::ReturnStatement(ret) => {
            if let Some(ref mut arg) = ret.argument {
                process_expression_jsx(
                    ast,
                    allocator,
                    source_text,
                    used_names,
                    top_level_names,
                    arg,
                    comments,
                    desc_name,
                    insertions,
                );
            }
        }
        Statement::VariableDeclaration(var_decl) => {
            for decl in var_decl.declarations.iter_mut() {
                if let Some(ref mut init) = decl.init {
                    process_expression_jsx(
                        ast,
                        allocator,
                        source_text,
                        used_names,
                        top_level_names,
                        init,
                        comments,
                        desc_name,
                        insertions,
                    );
                }
            }
        }
        Statement::ExpressionStatement(expr_stmt) => {
            process_expression_jsx(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut expr_stmt.expression,
                comments,
                desc_name,
                insertions,
            );
        }
        Statement::IfStatement(if_stmt) => {
            process_statement_jsx_inner(
                ast,
                allocator,
                source_text,
                used_names,
                top_level_names,
                &mut if_stmt.consequent,
                comments,
                desc_name,
                insertions,
            );
            if let Some(ref mut alternate) = if_stmt.alternate {
                process_statement_jsx_inner(
                    ast,
                    allocator,
                    source_text,
                    used_names,
                    top_level_names,
                    alternate,
                    comments,
                    desc_name,
                    insertions,
                );
            }
        }
        Statement::BlockStatement(block) => {
            for body_stmt in block.body.iter_mut() {
                process_statement_jsx_inner(
                    ast,
                    allocator,
                    source_text,
                    used_names,
                    top_level_names,
                    body_stmt,
                    comments,
                    desc_name,
                    insertions,
                );
            }
        }
        _ => {}
    }
}

/// Transform a single JSX element/fragment expression.
///
/// Steps:
/// 1. Clone the JSX for the template body
/// 2. Extract expressions from the clone → `_props.vN` replacements + collected attrs
/// 3. Build `const TemplateName = (_props) => <cloned jsx>`
/// 4. Replace original expression with `<TemplateName v0={} v1={} ... />`
fn transform_single_jsx<'a>(
    ast: AstBuilder<'a>,
    allocator: &'a Allocator,
    used_names: &mut HashSet<String>,
    top_level_names: &HashSet<&str>,
    expr: &mut Expression<'a>,
    desc_name: &str,
    insertions: &mut Vec<Statement<'a>>,
) {
    let props_name: &'a str = "_props";

    let mut state = JsxState {
        props_name,
        attributes: Vec::new(),
        vars: Vec::new(),
        var_count: 0,
    };

    // Clone the JSX — the clone will be mutated and become the template body
    let mut template_jsx = expr.clone_in(allocator);

    // Extract expressions from the cloned JSX
    match &mut template_jsx {
        Expression::JSXElement(el) => {
            let mut wrapper = JSXElementOrFragment::Element(el);
            extract_jsx_expressions(&mut state, ast, top_level_names, &mut wrapper);
        }
        Expression::JSXFragment(frag) => {
            let mut wrapper = JSXElementOrFragment::Fragment(frag);
            extract_jsx_expressions(&mut state, ast, top_level_names, &mut wrapper);
        }
        _ => return,
    }

    // If no expressions were extracted, nothing to do
    if state.attributes.is_empty() {
        return;
    }

    // Generate unique name for the template component
    let name_base = if is_component_ish_name(desc_name) {
        desc_name.to_string()
    } else {
        format!("JSX_{desc_name}")
    };
    let unique_name = generate_unique_name(&name_base, used_names);
    let unique_name_str: &'a str = ast.allocator.alloc_str(&unique_name);

    // Build the template component: (_props) => <template jsx>
    // If there are use: directive vars, wrap in a block body
    let has_vars = !state.vars.is_empty();
    let template_body: FunctionBody<'a> = if !has_vars {
        // Expression body: just the JSX
        let return_stmt = ast.statement_expression(SPAN, template_jsx);
        ast.function_body(SPAN, ast.vec(), ast.vec1(return_stmt))
    } else {
        // Block body with variable declarations + return
        let mut stmts = ast.vec();
        let var_decl = Statement::VariableDeclaration(ast.alloc_variable_declaration(
            SPAN,
            VariableDeclarationKind::Const,
            {
                let mut decls = ast.vec();
                for v in state.vars {
                    decls.push(v);
                }
                decls
            },
            false,
        ));
        stmts.push(var_decl);
        let ret = ast.statement_return(SPAN, Some(template_jsx));
        stmts.push(ret);
        ast.function_body(SPAN, ast.vec(), stmts)
    };

    let param = ast.plain_formal_parameter(
        SPAN,
        ast.binding_pattern_binding_identifier(SPAN, props_name),
    );
    let params = ast.alloc_formal_parameters(
        SPAN,
        FormalParameterKind::ArrowFormalParameters,
        ast.vec1(param),
        NONE,
    );

    let is_expression_body = !has_vars;
    let arrow = ast.expression_arrow_function(
        SPAN,
        is_expression_body,
        false,
        NONE,
        params,
        NONE,
        ast.alloc(template_body),
    );

    // Build: const TemplateName = (_props) => ...
    let binding = ast.binding_pattern_binding_identifier(SPAN, unique_name_str);
    let declarator = ast.variable_declarator(
        SPAN,
        VariableDeclarationKind::Const,
        binding,
        NONE,
        Some(arrow),
        false,
    );
    let template_decl = Statement::VariableDeclaration(ast.alloc_variable_declaration(
        SPAN,
        VariableDeclarationKind::Const,
        ast.vec1(declarator),
        false,
    ));
    insertions.push(template_decl);

    // Build replacement JSX: <TemplateName v0={} v1={} ... />
    let mut replacement_attrs = ast.vec();
    for attr in state.attributes {
        replacement_attrs.push(attr);
    }

    let opening = ast.jsx_opening_element(
        SPAN,
        JSXElementName::Identifier(ast.alloc(ast.jsx_identifier(SPAN, unique_name_str))),
        NONE, // no type arguments
        replacement_attrs,
    );

    let replacement_element = ast.alloc_jsx_element(
        SPAN,
        opening,
        ast.vec(), // no children
        NONE,      // no closing element (self-closing)
    );

    *expr = Expression::JSXElement(replacement_element);
}
