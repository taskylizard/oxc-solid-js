use oxc_allocator::CloneIn;
use oxc_ast::ast::{Argument, Expression, FormalParameterKind, Statement, VariableDeclarationKind};
use oxc_ast::{AstBuilder, NONE};
use oxc_span::{Span, SPAN};
use oxc_syntax::operator::{BinaryOperator, LogicalOperator, UnaryOperator};

use common::is_dynamic;

use crate::ir::{helper_ident_expr, BlockContext};

pub fn is_condition_expression(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::ConditionalExpression(_) | Expression::LogicalExpression(_) => true,
        Expression::ParenthesizedExpression(paren) => is_condition_expression(&paren.expression),
        _ => false,
    }
}

pub fn transform_condition_inline_expr<'a>(
    expr: Expression<'a>,
    context: &BlockContext<'a>,
) -> Expression<'a> {
    // Babel eagerly registers memo() when entering transformCondition,
    // even when no memo call ends up emitted.
    context.register_helper("memo");
    transform_condition_internal(expr, context, true).expr
}

pub fn transform_condition_non_inline_insert<'a>(
    expr: Expression<'a>,
    span: Span,
    context: &BlockContext<'a>,
) -> Expression<'a> {
    // Keep helper import parity with Babel transformCondition.
    context.register_helper("memo");
    let ast = context.ast();
    let transformed = transform_condition_internal(expr, context, false);

    if let Some(hoisted) = transformed.hoisted {
        return build_non_inline_wrapper(ast, span, transformed.expr, hoisted, context);
    }

    arrow_zero_params_expr(ast, span, transformed.expr)
}

struct HoistedMemo<'a> {
    id: String,
    cond: Expression<'a>,
}

struct TransformConditionResult<'a> {
    expr: Expression<'a>,
    hoisted: Option<HoistedMemo<'a>>,
}

fn is_dynamic_condition_expr(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::JSXElement(_) | Expression::JSXFragment(_) => true,
        _ => is_dynamic(expr),
    }
}

fn transform_condition_internal<'a>(
    expr: Expression<'a>,
    context: &BlockContext<'a>,
    inline: bool,
) -> TransformConditionResult<'a> {
    let ast = context.ast();

    match expr {
        Expression::ParenthesizedExpression(paren) => {
            let mut paren = paren.unbox();
            let transformed = transform_condition_internal(paren.expression, context, inline);
            paren.expression = transformed.expr;

            TransformConditionResult {
                expr: Expression::ParenthesizedExpression(ast.alloc(paren)),
                hoisted: transformed.hoisted,
            }
        }
        Expression::ConditionalExpression(cond) => {
            let mut cond = cond.unbox();
            let mut hoisted = None;

            if (is_dynamic_condition_expr(&cond.consequent)
                || is_dynamic_condition_expr(&cond.alternate))
                && is_dynamic_condition_expr(&cond.test)
            {
                let span = cond.span;
                let test = cond.test;
                let condition = normalize_test_condition(ast, span, test);

                if inline {
                    let memo = memo_getter_expr(ast, span, condition, context);
                    cond.test = call_expr(ast, span, memo, []);
                } else {
                    let id = context.generate_uid("c$");
                    cond.test = call_expr(ast, span, ident_expr(ast, span, &id), []);
                    hoisted = Some(HoistedMemo {
                        id,
                        cond: condition,
                    });
                }

                let recurse_consequent = is_condition_expression(&cond.consequent);
                if recurse_consequent {
                    let consequent = cond.consequent;
                    cond.consequent = transform_condition_internal(consequent, context, true).expr;
                }

                let recurse_alternate = is_condition_expression(&cond.alternate);
                if recurse_alternate {
                    let alternate = cond.alternate;
                    cond.alternate = transform_condition_internal(alternate, context, true).expr;
                }
            }

            TransformConditionResult {
                expr: Expression::ConditionalExpression(ast.alloc(cond)),
                hoisted,
            }
        }
        Expression::LogicalExpression(logical) => {
            let mut expr = Expression::LogicalExpression(ast.alloc(logical.unbox()));
            let hoisted = transform_logical_condition(&mut expr, context, inline);

            TransformConditionResult { expr, hoisted }
        }
        _ => TransformConditionResult {
            expr,
            hoisted: None,
        },
    }
}

fn transform_logical_condition<'a>(
    expr: &mut Expression<'a>,
    context: &BlockContext<'a>,
    inline: bool,
) -> Option<HoistedMemo<'a>> {
    let ast = context.ast();
    let target_expr = find_and_target_expression(expr)?;
    let Expression::LogicalExpression(target) = target_expr else {
        return None;
    };

    if !is_dynamic_condition_expr(&target.right) || !is_dynamic_condition_expr(&target.left) {
        return None;
    }

    let span = target.span;
    let left = std::mem::replace(&mut target.left, ident_expr(ast, span, "undefined"));
    let left_is_boolean = is_boolean_expression(&left);
    let condition = normalize_test_condition(ast, span, left.clone_in(ast.allocator));

    let (memo_call, hoisted) = if inline {
        let memo = memo_getter_expr(ast, span, condition, context);
        (call_expr(ast, span, memo, []), None)
    } else {
        let id = context.generate_uid("c$");
        (
            call_expr(ast, span, ident_expr(ast, span, &id), []),
            Some(HoistedMemo {
                id,
                cond: condition,
            }),
        )
    };

    if left_is_boolean {
        target.left = memo_call;
    } else {
        let right = std::mem::replace(&mut target.right, ident_expr(ast, span, "undefined"));
        *target_expr = ast.expression_conditional(span, memo_call, right, left);
    }

    hoisted
}

fn find_and_target_expression<'a, 'b>(
    expr: &'b mut Expression<'a>,
) -> Option<&'b mut Expression<'a>> {
    if matches!(expr, Expression::LogicalExpression(logical) if logical.operator == LogicalOperator::And)
    {
        return Some(expr);
    }

    match expr {
        Expression::LogicalExpression(logical) => find_and_target_expression(&mut logical.left),
        Expression::ParenthesizedExpression(paren) => {
            find_and_target_expression(&mut paren.expression)
        }
        _ => None,
    }
}

fn is_boolean_expression(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::BooleanLiteral(_) => true,
        Expression::UnaryExpression(unary) => unary.operator == UnaryOperator::LogicalNot,
        Expression::BinaryExpression(binary) => matches!(
            binary.operator,
            BinaryOperator::Equality
                | BinaryOperator::Inequality
                | BinaryOperator::StrictEquality
                | BinaryOperator::StrictInequality
                | BinaryOperator::LessThan
                | BinaryOperator::LessEqualThan
                | BinaryOperator::GreaterThan
                | BinaryOperator::GreaterEqualThan
                | BinaryOperator::Instanceof
                | BinaryOperator::In
        ),
        Expression::ParenthesizedExpression(paren) => is_boolean_expression(&paren.expression),
        Expression::TSAsExpression(ts) => is_boolean_expression(&ts.expression),
        Expression::TSSatisfiesExpression(ts) => is_boolean_expression(&ts.expression),
        Expression::TSNonNullExpression(ts) => is_boolean_expression(&ts.expression),
        Expression::TSTypeAssertion(ts) => is_boolean_expression(&ts.expression),
        _ => false,
    }
}

fn normalize_test_condition<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    expr: Expression<'a>,
) -> Expression<'a> {
    if is_boolean_expression(&expr) {
        expr
    } else {
        bool_cast_expr(ast, span, expr)
    }
}

fn memo_getter_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    condition: Expression<'a>,
    context: &BlockContext<'a>,
) -> Expression<'a> {
    context.register_helper("memo");
    let callee = helper_ident_expr(ast, span, "memo");
    let getter = arrow_zero_params_expr(ast, span, condition);
    call_expr(ast, span, callee, [getter])
}

fn build_non_inline_wrapper<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    expr: Expression<'a>,
    hoisted: HoistedMemo<'a>,
    context: &BlockContext<'a>,
) -> Expression<'a> {
    let memo_init = memo_getter_expr(ast, span, hoisted.cond, context);

    let declarator = ast.variable_declarator(
        SPAN,
        VariableDeclarationKind::Var,
        ast.binding_pattern_binding_identifier(SPAN, ast.allocator.alloc_str(&hoisted.id)),
        NONE,
        Some(memo_init),
        false,
    );

    let mut statements = ast.vec_with_capacity(2);
    statements.push(Statement::VariableDeclaration(
        ast.alloc_variable_declaration(
            SPAN,
            VariableDeclarationKind::Var,
            ast.vec1(declarator),
            false,
        ),
    ));
    statements.push(Statement::ReturnStatement(ast.alloc_return_statement(
        SPAN,
        Some(arrow_zero_params_expr(ast, span, expr)),
    )));

    let params = ast.alloc_formal_parameters(
        SPAN,
        FormalParameterKind::ArrowFormalParameters,
        ast.vec(),
        NONE,
    );
    let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
    let wrapper = ast.expression_arrow_function(SPAN, false, false, NONE, params, NONE, body);

    call_expr(ast, span, wrapper, [])
}

fn ident_expr<'a>(ast: AstBuilder<'a>, span: Span, name: &str) -> Expression<'a> {
    let _ = span;
    ast.expression_identifier(SPAN, ast.allocator.alloc_str(name))
}

fn call_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    callee: Expression<'a>,
    args: impl IntoIterator<Item = Expression<'a>>,
) -> Expression<'a> {
    let _ = span;
    let mut arguments = ast.vec();
    for arg in args {
        arguments.push(Argument::from(arg));
    }
    ast.expression_call(
        SPAN,
        callee,
        None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
        arguments,
        false,
    )
}

fn arrow_zero_params_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    expr: Expression<'a>,
) -> Expression<'a> {
    let _ = span;
    let params = ast.alloc_formal_parameters(
        SPAN,
        FormalParameterKind::ArrowFormalParameters,
        ast.vec(),
        NONE,
    );
    let mut statements = ast.vec_with_capacity(1);
    statements.push(Statement::ExpressionStatement(
        ast.alloc_expression_statement(SPAN, expr),
    ));
    let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
    ast.expression_arrow_function(SPAN, true, false, NONE, params, NONE, body)
}

fn bool_cast_expr<'a>(ast: AstBuilder<'a>, span: Span, expr: Expression<'a>) -> Expression<'a> {
    let _ = span;
    let not_expr = ast.expression_unary(SPAN, UnaryOperator::LogicalNot, expr);
    ast.expression_unary(SPAN, UnaryOperator::LogicalNot, not_expr)
}
