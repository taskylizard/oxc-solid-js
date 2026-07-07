use oxc_allocator::CloneIn;
use oxc_ast::ast::{AssignmentTarget, Expression, FormalParameterKind, Statement};
use oxc_ast::{AstBuilder, NONE};
use oxc_span::Span;
use oxc_syntax::operator::{AssignmentOperator, LogicalOperator};

use crate::ir::{helper_ident_expr, DynamicBinding};

fn ident_expr<'a>(ast: AstBuilder<'a>, span: Span, name: &str) -> Expression<'a> {
    ast.expression_identifier(span, ast.allocator.alloc_str(name))
}

fn static_member<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    object: Expression<'a>,
    property: &str,
) -> Expression<'a> {
    let prop = ast.identifier_name(span, ast.allocator.alloc_str(property));
    Expression::StaticMemberExpression(
        ast.alloc_static_member_expression(span, object, prop, false),
    )
}

fn arrow_zero_params_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    expr: Expression<'a>,
) -> Expression<'a> {
    let params = ast.alloc_formal_parameters(
        span,
        FormalParameterKind::ArrowFormalParameters,
        ast.vec(),
        NONE,
    );
    let mut statements = ast.vec_with_capacity(1);
    statements.push(Statement::ExpressionStatement(
        ast.alloc_expression_statement(span, expr),
    ));
    let body = ast.alloc_function_body(span, ast.vec(), statements);
    ast.expression_arrow_function(span, true, false, NONE, params, NONE, body)
}

fn expression_to_assignment_target<'a>(expr: Expression<'a>) -> Option<AssignmentTarget<'a>> {
    match expr {
        Expression::Identifier(ident) => Some(AssignmentTarget::AssignmentTargetIdentifier(ident)),
        Expression::StaticMemberExpression(m) => Some(AssignmentTarget::StaticMemberExpression(m)),
        Expression::ComputedMemberExpression(m) => {
            Some(AssignmentTarget::ComputedMemberExpression(m))
        }
        Expression::PrivateFieldExpression(m) => Some(AssignmentTarget::PrivateFieldExpression(m)),
        Expression::TSAsExpression(e) => Some(AssignmentTarget::TSAsExpression(e)),
        Expression::TSSatisfiesExpression(e) => Some(AssignmentTarget::TSSatisfiesExpression(e)),
        Expression::TSNonNullExpression(e) => Some(AssignmentTarget::TSNonNullExpression(e)),
        Expression::TSTypeAssertion(e) => Some(AssignmentTarget::TSTypeAssertion(e)),
        _ => None,
    }
}

pub fn generate_set_attr_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    binding: &DynamicBinding<'a>,
    hydratable: bool,
) -> Expression<'a> {
    generate_set_attr_expr_with_value(
        ast,
        span,
        binding,
        binding.value.clone_in(ast.allocator),
        None,
        hydratable,
    )
}

pub fn generate_set_attr_expr_with_value<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    binding: &DynamicBinding<'a>,
    value: Expression<'a>,
    prev_value: Option<Expression<'a>>,
    hydratable: bool,
) -> Expression<'a> {
    let key = binding.key.as_str();
    let elem = ident_expr(ast, span, &binding.elem);
    let mut prev_value = prev_value;

    // Handle special cases
    if let Some(name) = key.strip_prefix("bool:") {
        let callee = helper_ident_expr(ast, span, "setBoolAttribute");
        let name = ast.expression_string_literal(span, ast.allocator.alloc_str(name), None);
        return ast.expression_call(
            span,
            callee,
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            ast.vec_from_array([elem.into(), name.into(), value.into()]),
            false,
        );
    }

    if let Some(name) = key.strip_prefix("style:") {
        let callee = helper_ident_expr(ast, span, "setStyleProperty");
        let name = ast.expression_string_literal(span, ast.allocator.alloc_str(name), None);
        return ast.expression_call(
            span,
            callee,
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            ast.vec_from_array([elem.into(), name.into(), value.into()]),
            false,
        );
    }

    if let Some(name) = key.strip_prefix("class:") {
        let class_list = static_member(ast, span, elem, "classList");
        let toggle = static_member(ast, span, class_list, "toggle");
        let name = ast.expression_string_literal(span, ast.allocator.alloc_str(name), None);
        return ast.expression_call(
            span,
            toggle,
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            ast.vec_from_array([name.into(), value.into()]),
            false,
        );
    }

    if let Some(name) = key.strip_prefix("prop:") {
        let member = static_member(ast, span, elem, name);
        if let Some(target) = expression_to_assignment_target(member) {
            return ast.expression_assignment(span, AssignmentOperator::Assign, target, value);
        }
        return ast.expression_identifier(span, "undefined");
    }

    if key == "class" {
        let callee = helper_ident_expr(ast, span, "className");
        let args = if let Some(prev) = prev_value.take() {
            ast.vec_from_array([elem.into(), value.into(), prev.into()])
        } else {
            ast.vec_from_array([elem.into(), value.into()])
        };
        return ast.expression_call(
            span,
            callee,
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            args,
            false,
        );
    }

    if key == "style" {
        let callee = helper_ident_expr(ast, span, "style");
        let args = if let Some(prev) = prev_value.take() {
            ast.vec_from_array([elem.into(), value.into(), prev.into()])
        } else {
            ast.vec_from_array([elem.into(), value.into()])
        };
        return ast.expression_call(
            span,
            callee,
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            args,
            false,
        );
    }

    if key == "classList" {
        let callee = helper_ident_expr(ast, span, "classList");
        return ast.expression_call(
            span,
            callee,
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            ast.vec_from_array([elem.into(), value.into()]),
            false,
        );
    }

    if key == "textContent" || key == "innerText" {
        if hydratable {
            let callee = helper_ident_expr(ast, span, "setProperty");
            let name = ast.expression_string_literal(span, ast.allocator.alloc_str("data"), None);
            return ast.expression_call(
                span,
                callee,
                None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                ast.vec_from_array([elem.into(), name.into(), value.into()]),
                false,
            );
        }

        let member = static_member(ast, span, elem, "data");
        if let Some(target) = expression_to_assignment_target(member) {
            return ast.expression_assignment(span, AssignmentOperator::Assign, target, value);
        }
        return ast.expression_identifier(span, "undefined");
    }

    if let Some((prefix, _)) = key.split_once(':') {
        if let Some(ns) = common::constants::SVG_NAMESPACE.get(prefix) {
            let callee = helper_ident_expr(ast, span, "setAttributeNS");
            let ns_lit = ast.expression_string_literal(span, ast.allocator.alloc_str(ns), None);
            let name = ast.expression_string_literal(span, ast.allocator.alloc_str(key), None);
            return ast.expression_call(
                span,
                callee,
                None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                ast.vec_from_array([elem.into(), ns_lit.into(), name.into(), value.into()]),
                false,
            );
        }
    }

    let is_child_property = common::constants::CHILD_PROPERTIES.contains(key);
    let is_property = common::constants::PROPERTIES.contains(key);

    if is_child_property || is_property {
        let prop_name = if is_child_property {
            key
        } else {
            common::constants::get_prop_alias(key, binding.tag_name.as_str()).unwrap_or(key)
        };

        if hydratable {
            let callee = helper_ident_expr(ast, span, "setProperty");
            let prop_name_lit =
                ast.expression_string_literal(span, ast.allocator.alloc_str(prop_name), None);
            return ast.expression_call(
                span,
                callee,
                None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                ast.vec_from_array([elem.into(), prop_name_lit.into(), value.into()]),
                false,
            );
        }

        let member = static_member(ast, span, elem, prop_name);
        if let Some(target) = expression_to_assignment_target(member) {
            let assignment =
                ast.expression_assignment(span, AssignmentOperator::Assign, target, value);

            if key == "value" && binding.tag_name.eq_ignore_ascii_case("select") {
                let callback =
                    arrow_zero_params_expr(ast, span, assignment.clone_in(ast.allocator));
                let queue_microtask = ast.expression_identifier(span, "queueMicrotask");
                let queue_call = ast.expression_call(
                    span,
                    queue_microtask,
                    None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                    ast.vec1(callback.into()),
                    false,
                );
                return ast.expression_logical(span, queue_call, LogicalOperator::Or, assignment);
            }

            return assignment;
        }
        return ast.expression_identifier(span, "undefined");
    }

    // Custom elements should not blindly route attributes to property assignment.
    // Babel only uses property mode for explicit property-targeted paths (handled
    // earlier in element transform) and known property keys.
    let callee = helper_ident_expr(ast, span, "setAttribute");
    let name = ast.expression_string_literal(span, ast.allocator.alloc_str(key), None);
    ast.expression_call(
        span,
        callee,
        None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
        ast.vec_from_array([elem.into(), name.into(), value.into()]),
        false,
    )
}
