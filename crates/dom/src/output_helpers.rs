use oxc_allocator::CloneIn;
use oxc_ast::ast::{Argument, Expression, FormalParameterKind, Statement};
use oxc_ast::{AstBuilder, NONE};
use oxc_span::Span;

pub(crate) fn ident_expr<'a>(ast: AstBuilder<'a>, span: Span, name: &str) -> Expression<'a> {
    ast.expression_identifier(span, ast.allocator.alloc_str(name))
}

pub(crate) fn static_member<'a>(
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

pub(crate) fn call_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    callee: Expression<'a>,
    args: impl IntoIterator<Item = Expression<'a>>,
) -> Expression<'a> {
    let mut arguments = ast.vec();
    for arg in args {
        arguments.push(Argument::from(arg));
    }
    ast.expression_call(
        span,
        callee,
        None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
        arguments,
        false,
    )
}

pub(crate) fn arrow_zero_params_body<'a>(
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

pub(crate) fn inline_effect_source_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    expr: &Expression<'a>,
) -> Expression<'a> {
    if let Expression::CallExpression(call) = expr {
        if call.arguments.is_empty()
            && matches!(
                call.callee,
                Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
            )
        {
            return call.callee.clone_in(ast.allocator);
        }
    }

    arrow_zero_params_body(ast, span, expr.clone_in(ast.allocator))
}

const NUMBERED_ID_CHARS: &[u8] = b"etaoinshrdlucwmfygpbTAOISWCBvkxjqzPHFMDRELNGUKVYJQZX_$";

pub(crate) fn get_numbered_id(mut num: usize) -> String {
    let base = NUMBERED_ID_CHARS.len();
    let mut out = Vec::new();

    loop {
        let digit = num % base;
        num /= base;
        out.push(NUMBERED_ID_CHARS[digit] as char);
        if num == 0 {
            break;
        }
    }

    out.into_iter().rev().collect()
}
