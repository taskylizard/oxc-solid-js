use oxc_allocator::CloneIn;
use oxc_ast::ast::{
    Argument, Expression, FormalParameterKind, FunctionType, JSXAttribute, JSXAttributeItem,
    JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement, ObjectPropertyKind, PropertyKey,
    PropertyKind, Statement, TemplateElementValue,
};
use oxc_ast::AstBuilder;
use oxc_ast::NONE;
use oxc_span::{Span, SPAN};
use oxc_syntax::identifier::is_identifier_name;
use oxc_syntax::keyword::is_reserved_keyword;
use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator};
use oxc_traverse::TraverseCtx;

use common::{
    expression::{escape_html, normalize_jsx_text},
    get_attr_name, is_dynamic, TransformOptions,
};

use crate::conditional::{
    is_condition_expression, transform_condition_inline_expr, transform_condition_non_inline_insert,
};
use crate::element::{is_writable_ref_target, static_child_text};
use crate::expression_utils::{
    expression_to_assignment_target, peel_wrapped_expression as unwrap_ts_expression,
};
use crate::ir::{
    helper_ident_expr, BlockContext, ChildTransformer, Declaration, DynamicBinding, HelperSource,
    OutputKind, TransformResult,
};
use crate::transform::TransformInfo;
use crate::universal_output::build_universal_output_expr;

fn escape_string_for_template(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '{' => escaped.push_str("\\{"),
            '`' => escaped.push_str("\\`"),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            '\u{0008}' => escaped.push_str("\\b"),
            '\u{000C}' => escaped.push_str("\\f"),
            '\u{000B}' => escaped.push_str("\\v"),
            '\r' => escaped.push_str("\\r"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn ident_expr<'a>(ast: AstBuilder<'a>, span: Span, name: &str) -> Expression<'a> {
    ast.expression_identifier(span, ast.allocator.alloc_str(name))
}

fn call_expr<'a>(
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

fn arrow_zero_params_return_expr<'a>(
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

fn getter_return_expr<'a>(ast: AstBuilder<'a>, span: Span, expr: Expression<'a>) -> Expression<'a> {
    let params =
        ast.alloc_formal_parameters(span, FormalParameterKind::FormalParameter, ast.vec(), NONE);
    let mut statements = ast.vec_with_capacity(1);
    statements.push(Statement::ReturnStatement(
        ast.alloc_return_statement(span, Some(expr)),
    ));
    let body = ast.alloc_function_body(span, ast.vec(), statements);
    ast.expression_function(
        span,
        FunctionType::FunctionExpression,
        None,
        false,
        false,
        false,
        NONE,
        NONE,
        params,
        NONE,
        Some(body),
    )
}

fn template_literal_expr_from_raw<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    raw_value: &str,
) -> Expression<'a> {
    let escaped_raw = escape_string_for_template(raw_value);
    let raw = ast.allocator.alloc_str(&escaped_raw);
    let cooked = ast.allocator.alloc_str(raw_value);
    let value = TemplateElementValue {
        raw: ast.str(raw),
        cooked: Some(ast.str(cooked)),
    };
    let quasis = ast.vec1(ast.template_element(span, value, true));
    let template = ast.template_literal(span, quasis, ast.vec());
    Expression::TemplateLiteral(ast.alloc(template))
}

fn make_prop_key<'a>(ast: AstBuilder<'a>, span: Span, raw_key: &str) -> PropertyKey<'a> {
    let key = ast.allocator.alloc_str(raw_key);
    if is_identifier_name(raw_key) {
        PropertyKey::StaticIdentifier(ast.alloc_identifier_name(span, key))
    } else {
        PropertyKey::StringLiteral(ast.alloc_string_literal(span, key, None))
    }
}

fn make_getter_prop_key<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    raw_key: &str,
) -> (PropertyKey<'a>, bool) {
    let key = ast.allocator.alloc_str(raw_key);
    let key_is_identifier = is_identifier_name(raw_key) && !is_reserved_keyword(raw_key);
    let prop_key = if key_is_identifier {
        PropertyKey::StaticIdentifier(ast.alloc_identifier_name(span, key))
    } else {
        PropertyKey::StringLiteral(ast.alloc_string_literal(span, key, None))
    };

    (prop_key, key_is_identifier)
}

fn can_native_spread(key: &str, check_namespaces: bool) -> bool {
    if check_namespaces {
        if let Some((ns, _)) = key.split_once(':') {
            if matches!(ns, "class" | "style" | "use" | "prop") {
                return false;
            }
        }
    }
    key != "ref"
}

fn memo_wrapper_enabled(options: &TransformOptions<'_>) -> bool {
    !options.memo_wrapper.is_empty()
}

fn set_prop_expr_with_value<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    context: &BlockContext<'a>,
    elem_id: &str,
    name: &str,
    value: Expression<'a>,
) -> Expression<'a> {
    context.register_helper("setProp");

    let elem = ident_expr(ast, span, elem_id);
    let prop = ast.expression_string_literal(span, ast.allocator.alloc_str(name), None);

    call_expr(
        ast,
        span,
        helper_ident_expr(ast, span, "setProp"),
        [elem, prop, value],
    )
}

fn push_expr_statement<'a>(
    result: &mut TransformResult<'a>,
    ast: AstBuilder<'a>,
    span: Span,
    expr: Expression<'a>,
) {
    result.statements.push(Statement::ExpressionStatement(
        ast.alloc_expression_statement(span, expr),
    ));
}

fn unshift_expr_statement<'a>(
    result: &mut TransformResult<'a>,
    ast: AstBuilder<'a>,
    span: Span,
    expr: Expression<'a>,
) {
    result.statements.insert(
        0,
        Statement::ExpressionStatement(ast.alloc_expression_statement(span, expr)),
    );
}

fn literal_text_from_expr(expr: &Expression<'_>) -> Option<String> {
    match unwrap_ts_expression(expr) {
        Expression::StringLiteral(lit) => Some(lit.value.to_string()),
        Expression::NumericLiteral(num) => {
            if num.value == 0.0 {
                Some("0".to_string())
            } else {
                Some(num.value.to_string())
            }
        }
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => template
            .quasis
            .first()
            .map(|quasi| quasi.value.raw.as_str().to_string()),
        _ => None,
    }
}

fn is_function_expression(expr: &Expression<'_>) -> bool {
    matches!(
        unwrap_ts_expression(expr),
        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
    )
}

fn is_static_insert_factory_call(expr: &Expression<'_>) -> bool {
    let Expression::CallExpression(call) = unwrap_ts_expression(expr) else {
        return false;
    };

    let Expression::Identifier(callee) = &call.callee else {
        return false;
    };

    let name = callee.name.as_str();
    name.starts_with("_tmpl$") || name.starts_with("_$createElement")
}

fn is_static_insert_branch(expr: &Expression<'_>) -> bool {
    if is_static_insert_factory_call(expr) {
        return true;
    }

    let unwrapped = unwrap_ts_expression(expr);

    if let Expression::CallExpression(call) = unwrapped {
        if call.arguments.is_empty()
            && matches!(
                &call.callee,
                Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
            )
        {
            return !is_dynamic(unwrapped);
        }
    }

    !is_dynamic(unwrapped)
}

fn should_keep_conditional_insert_static(expr: &Expression<'_>) -> bool {
    let Expression::ConditionalExpression(cond) = unwrap_ts_expression(expr) else {
        return false;
    };

    !is_dynamic(&cond.test)
        && is_static_insert_branch(&cond.consequent)
        && is_static_insert_branch(&cond.alternate)
}

fn normalize_insert_value<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    expr: &Expression<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    has_static_marker: bool,
) -> Expression<'a> {
    if has_static_marker {
        return context.clone_expr_without_trivia(expr);
    }

    if let Expression::CallExpression(call) = expr {
        if call.arguments.is_empty()
            && !matches!(
                call.callee,
                Expression::CallExpression(_)
                    | Expression::StaticMemberExpression(_)
                    | Expression::ComputedMemberExpression(_)
            )
        {
            return context.clone_expr(&call.callee);
        }
    }

    if should_keep_conditional_insert_static(expr) {
        return context.clone_expr(expr);
    }

    let dynamic = is_dynamic(expr);

    if options.wrap_conditionals
        && memo_wrapper_enabled(options)
        && dynamic
        && is_condition_expression(expr)
    {
        return transform_condition_non_inline_insert(context.clone_expr(expr), span, context);
    }

    if dynamic {
        return arrow_zero_params_return_expr(ast, span, context.clone_expr(expr));
    }

    context.clone_expr(expr)
}

fn build_child_result_from_expression<'a>(
    expr: &Expression<'a>,
    span: Span,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    ctx: &TraverseCtx<'a, ()>,
    has_static_marker: bool,
) -> TransformResult<'a> {
    let ast = context.ast();

    if let Some(static_text) = static_child_text(expr, context, ctx) {
        return TransformResult {
            span,
            template: escape_html(&static_text, false).into_owned(),
            text: true,
            id: Some(context.generate_uid("el$")),
            ..Default::default()
        };
    }

    let insert_value = normalize_insert_value(ast, span, expr, context, options, has_static_marker);
    TransformResult {
        span,
        exprs: vec![insert_value],
        ..Default::default()
    }
}

fn process_spreads<'a, 'b>(
    element: &'b JSXElement<'a>,
    elem_id: &str,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    has_children: bool,
) -> (Vec<&'b JSXAttributeItem<'a>>, Option<Expression<'a>>) {
    let ast = context.ast();

    let mut filtered_attributes: Vec<&JSXAttributeItem<'a>> = Vec::new();
    let mut spread_args: Vec<Expression<'a>> = Vec::new();
    let mut running_props: Vec<ObjectPropertyKind<'a>> = Vec::new();
    let mut dynamic_spread = false;
    let mut first_spread = false;

    let flush_running_props =
        |spread_args: &mut Vec<Expression<'a>>, running_props: &mut Vec<ObjectPropertyKind<'a>>| {
            if running_props.is_empty() {
                return;
            }

            let mut props = ast.vec_with_capacity(running_props.len());
            for prop in running_props.drain(..) {
                props.push(prop);
            }
            spread_args.push(ast.expression_object(SPAN, props));
        };

    for attr_item in &element.opening_element.attributes {
        match attr_item {
            JSXAttributeItem::SpreadAttribute(spread) => {
                first_spread = true;
                flush_running_props(&mut spread_args, &mut running_props);

                let has_static_marker =
                    context.has_static_marker_comment(spread.span, options.static_marker);
                let mut spread_expr = if has_static_marker {
                    context.clone_expr_without_trivia(&spread.argument)
                } else {
                    context.clone_expr(&spread.argument)
                };

                if !has_static_marker && is_dynamic(&spread.argument) {
                    dynamic_spread = true;
                    spread_expr = if let Expression::CallExpression(call) = &spread.argument {
                        if call.arguments.is_empty()
                            && !matches!(
                                call.callee,
                                Expression::CallExpression(_)
                                    | Expression::StaticMemberExpression(_)
                                    | Expression::ComputedMemberExpression(_)
                            )
                        {
                            context.clone_expr(&call.callee)
                        } else {
                            arrow_zero_params_return_expr(
                                ast,
                                spread.span,
                                context.clone_expr(&spread.argument),
                            )
                        }
                    } else {
                        arrow_zero_params_return_expr(
                            ast,
                            spread.span,
                            context.clone_expr(&spread.argument),
                        )
                    };
                }

                if has_static_marker {
                    let spread_prop =
                        ast.object_property_kind_spread_property(spread.span, spread_expr);
                    spread_args.push(ast.expression_object(SPAN, ast.vec1(spread_prop)));
                } else {
                    spread_args.push(spread_expr);
                }
            }
            JSXAttributeItem::Attribute(attr) => {
                let key = get_attr_name(&attr.name);
                let has_static_marker =
                    if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
                        context.has_static_marker_comment(container.span, options.static_marker)
                    } else {
                        false
                    };

                let dynamic =
                    if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
                        !has_static_marker
                            && container
                                .expression
                                .as_expression()
                                .map(is_dynamic)
                                .unwrap_or(false)
                    } else {
                        false
                    };

                if (first_spread || dynamic) && can_native_spread(&key, true) {
                    if dynamic {
                        let (prop_key, key_is_identifier) =
                            make_getter_prop_key(ast, attr.span, &key);
                        let mut expr = attr
                            .value
                            .as_ref()
                            .and_then(|value| match value {
                                JSXAttributeValue::ExpressionContainer(container) => {
                                    container.expression.as_expression()
                                }
                                _ => None,
                            })
                            .map(|expr| context.clone_expr(expr))
                            .unwrap_or_else(|| ast.expression_identifier(SPAN, "undefined"));

                        if options.wrap_conditionals
                            && memo_wrapper_enabled(options)
                            && is_condition_expression(&expr)
                        {
                            expr = transform_condition_inline_expr(expr, context);
                        }

                        running_props.push(ast.object_property_kind_object_property(
                            attr.span,
                            PropertyKind::Get,
                            prop_key,
                            getter_return_expr(ast, attr.span, expr),
                            false,
                            false,
                            !key_is_identifier,
                        ));
                    } else {
                        let prop_key = make_prop_key(ast, attr.span, &key);
                        let value = match &attr.value {
                            Some(JSXAttributeValue::StringLiteral(lit)) => ast
                                .expression_string_literal(
                                    attr.span,
                                    ast.allocator.alloc_str(&lit.value),
                                    None,
                                ),
                            Some(JSXAttributeValue::ExpressionContainer(container)) => container
                                .expression
                                .as_expression()
                                .map(|expr| {
                                    if has_static_marker {
                                        context.clone_expr_without_trivia(expr)
                                    } else {
                                        context.clone_expr(expr)
                                    }
                                })
                                .unwrap_or_else(|| ast.expression_identifier(SPAN, "undefined")),
                            None => ast.expression_boolean_literal(attr.span, true),
                            _ => ast.expression_identifier(SPAN, "undefined"),
                        };

                        running_props.push(ast.object_property_kind_object_property(
                            attr.span,
                            PropertyKind::Init,
                            prop_key,
                            value,
                            false,
                            false,
                            false,
                        ));
                    }
                } else {
                    filtered_attributes.push(attr_item);
                }
            }
        }
    }

    flush_running_props(&mut spread_args, &mut running_props);

    if spread_args.is_empty() {
        return (filtered_attributes, None);
    }

    let props = if spread_args.len() == 1 && !dynamic_spread {
        spread_args.pop().unwrap()
    } else {
        context.register_helper("mergeProps");
        call_expr(
            ast,
            SPAN,
            helper_ident_expr(ast, SPAN, "mergeProps"),
            spread_args,
        )
    };

    let spread_expr = call_expr(
        ast,
        SPAN,
        context.helper_ident_expr_with_source(ast, SPAN, "spread", HelperSource::Universal),
        [
            ident_expr(ast, SPAN, elem_id),
            props,
            ast.expression_boolean_literal(SPAN, has_children),
        ],
    );

    (filtered_attributes, Some(spread_expr))
}

fn transform_use_directive<'a>(
    attr: &JSXAttribute<'a>,
    key: &str,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
) {
    let ast = context.ast();

    let directive_name = match &attr.name {
        JSXAttributeName::NamespacedName(ns) => ns.name.name.as_str(),
        _ => key.strip_prefix("use:").unwrap_or(""),
    };

    if directive_name.is_empty() {
        return;
    }

    let directive = ident_expr(ast, attr.span, directive_name);
    let elem = ident_expr(ast, attr.span, elem_id);

    let value_expr = match &attr.value {
        Some(JSXAttributeValue::ExpressionContainer(container)) => container
            .expression
            .as_expression()
            .map(|expr| context.clone_expr(expr))
            .unwrap_or_else(|| ast.expression_boolean_literal(attr.span, true)),
        Some(JSXAttributeValue::StringLiteral(lit)) => {
            ast.expression_string_literal(attr.span, ast.allocator.alloc_str(&lit.value), None)
        }
        None => ast.expression_boolean_literal(attr.span, true),
        _ => ast.expression_boolean_literal(attr.span, true),
    };

    let callback = arrow_zero_params_return_expr(ast, attr.span, value_expr);
    let use_call = call_expr(
        ast,
        attr.span,
        context.helper_ident_expr_with_source(ast, attr.span, "use", HelperSource::Universal),
        [directive, elem, callback],
    );

    // Match Babel unshift semantics for use:/ref directives.
    unshift_expr_statement(result, ast, attr.span, use_call);
}

fn peel_identifier_reference<'a, 'b>(
    expr: &'b Expression<'a>,
) -> Option<&'b oxc_ast::ast::IdentifierReference<'a>> {
    match expr {
        Expression::Identifier(ident) => Some(ident),
        Expression::ParenthesizedExpression(e) => peel_identifier_reference(&e.expression),
        Expression::TSAsExpression(e) => peel_identifier_reference(&e.expression),
        Expression::TSSatisfiesExpression(e) => peel_identifier_reference(&e.expression),
        Expression::TSNonNullExpression(e) => peel_identifier_reference(&e.expression),
        Expression::TSTypeAssertion(e) => peel_identifier_reference(&e.expression),
        _ => None,
    }
}

fn is_constant_identifier_ref<'a>(expr: &Expression<'a>, ctx: &TraverseCtx<'a, ()>) -> bool {
    let Some(ident) = peel_identifier_reference(expr) else {
        return false;
    };

    let Some(reference_id) = ident.reference_id.get() else {
        return false;
    };

    let reference = ctx.scoping().get_reference(reference_id);
    let Some(symbol_id) = reference.symbol_id() else {
        return false;
    };

    let flags = ctx.scoping().symbol_flags(symbol_id);
    flags.is_const_variable()
        || flags.contains(oxc_syntax::symbol::SymbolFlags::Import)
        || flags.contains(oxc_syntax::symbol::SymbolFlags::TypeImport)
}

fn ref_temp_declaration<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    temp_name: &str,
    init: Expression<'a>,
) -> Statement<'a> {
    let declarator = ast.variable_declarator(
        span,
        oxc_ast::ast::VariableDeclarationKind::Var,
        ast.binding_pattern_binding_identifier(span, ast.allocator.alloc_str(temp_name)),
        NONE,
        Some(init),
        false,
    );

    Statement::VariableDeclaration(ast.alloc_variable_declaration(
        span,
        oxc_ast::ast::VariableDeclarationKind::Var,
        ast.vec1(declarator),
        false,
    ))
}

fn transform_ref<'a>(
    attr: &JSXAttribute<'a>,
    expr: &Expression<'a>,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    ctx: &TraverseCtx<'a, ()>,
) {
    let ast = context.ast();

    let elem = ident_expr(ast, attr.span, elem_id);
    let writable_target = is_writable_ref_target(expr, ctx);
    let assign_target = expression_to_assignment_target(context.clone_expr(expr));

    // Babel parity: non-const assignable refs use a temp with
    // `typeof temp === "function" ? use(temp, el) : (expr = el)`.
    if writable_target {
        if let Some(target) = assign_target {
            let temp_name = context.generate_uid("ref$");
            let temp_ident = ident_expr(ast, attr.span, &temp_name);
            let var_decl =
                ref_temp_declaration(ast, attr.span, &temp_name, context.clone_expr(expr));

            let typeof_ref = ast.expression_unary(
                SPAN,
                UnaryOperator::Typeof,
                temp_ident.clone_in(ast.allocator),
            );
            let function_str =
                ast.expression_string_literal(SPAN, ast.allocator.alloc_str("function"), None);
            let test = ast.expression_binary(
                SPAN,
                typeof_ref,
                BinaryOperator::StrictEquality,
                function_str,
            );
            let use_call = call_expr(
                ast,
                attr.span,
                context.helper_ident_expr_with_source(
                    ast,
                    attr.span,
                    "use",
                    HelperSource::Universal,
                ),
                [
                    temp_ident.clone_in(ast.allocator),
                    elem.clone_in(ast.allocator),
                ],
            );
            let assign = ast.expression_assignment(SPAN, AssignmentOperator::Assign, target, elem);
            let cond_stmt = Statement::ExpressionStatement(ast.alloc_expression_statement(
                attr.span,
                ast.expression_conditional(SPAN, test, use_call, assign),
            ));
            result.statements.insert(0, cond_stmt);
            result.statements.insert(0, var_decl);
            return;
        }
    }

    // Babel parity: const/module refs and inline function refs are passed directly to use().
    if is_constant_identifier_ref(expr, ctx) || is_function_expression(expr) {
        let use_call = call_expr(
            ast,
            attr.span,
            context.helper_ident_expr_with_source(ast, attr.span, "use", HelperSource::Universal),
            [context.clone_expr(expr), elem],
        );
        unshift_expr_statement(result, ast, attr.span, use_call);
        return;
    }

    // Fallback: evaluate once into temp, then call use(temp, el) only when function.
    let temp_name = context.generate_uid("ref$");
    let temp_ident = ident_expr(ast, attr.span, &temp_name);
    let var_decl = ref_temp_declaration(ast, attr.span, &temp_name, context.clone_expr(expr));

    let typeof_ref = ast.expression_unary(
        SPAN,
        UnaryOperator::Typeof,
        temp_ident.clone_in(ast.allocator),
    );
    let function_str =
        ast.expression_string_literal(SPAN, ast.allocator.alloc_str("function"), None);
    let test = ast.expression_binary(
        SPAN,
        typeof_ref,
        BinaryOperator::StrictEquality,
        function_str,
    );
    let use_call = call_expr(
        ast,
        attr.span,
        context.helper_ident_expr_with_source(ast, attr.span, "use", HelperSource::Universal),
        [temp_ident, elem],
    );
    let logical_stmt = Statement::ExpressionStatement(ast.alloc_expression_statement(
        attr.span,
        ast.expression_logical(SPAN, test, LogicalOperator::And, use_call),
    ));
    result.statements.insert(0, logical_stmt);
    result.statements.insert(0, var_decl);
}

fn transform_attributes<'a>(
    element: &JSXElement<'a>,
    tag_name: &str,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    ctx: &TraverseCtx<'a, ()>,
) -> Option<TransformResult<'a>> {
    let ast = context.ast();

    let has_children = !element.children.is_empty();
    let has_spread = element
        .opening_element
        .attributes
        .iter()
        .any(|attr| matches!(attr, JSXAttributeItem::SpreadAttribute(_)));

    let (attributes, spread_expr) = if has_spread {
        process_spreads(element, elem_id, context, options, has_children)
    } else {
        (
            element
                .opening_element
                .attributes
                .iter()
                .collect::<Vec<&JSXAttributeItem<'a>>>(),
            None,
        )
    };

    let mut injected_children: Option<TransformResult<'a>> = None;

    for item in attributes {
        let JSXAttributeItem::Attribute(attr) = item else {
            continue;
        };

        let key = get_attr_name(&attr.name);

        if key == "children" {
            injected_children = match &attr.value {
                Some(JSXAttributeValue::ExpressionContainer(container)) => {
                    let has_static_marker =
                        context.has_static_marker_comment(container.span, options.static_marker);
                    container.expression.as_expression().map(|expr| {
                        build_child_result_from_expression(
                            expr,
                            attr.span,
                            context,
                            options,
                            ctx,
                            has_static_marker,
                        )
                    })
                }
                Some(JSXAttributeValue::StringLiteral(lit)) => Some(TransformResult {
                    span: attr.span,
                    template: escape_html(&lit.value, false).into_owned(),
                    text: true,
                    id: Some(context.generate_uid("el$")),
                    ..Default::default()
                }),
                _ => None,
            };
            continue;
        }

        if key.starts_with("use:") {
            transform_use_directive(attr, &key, elem_id, result, context);
            continue;
        }

        if key == "ref" {
            if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
                if let Some(expr) = container.expression.as_expression() {
                    transform_ref(attr, expr, elem_id, result, context, ctx);
                }
            }
            continue;
        }

        if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
            if let Some(expr) = container.expression.as_expression() {
                let has_static_marker =
                    context.has_static_marker_comment(container.span, options.static_marker);
                let value_expr = if has_static_marker {
                    context.clone_expr_without_trivia(expr)
                } else {
                    context.clone_expr(expr)
                };

                if !has_static_marker && !options.effect_wrapper.is_empty() && is_dynamic(expr) {
                    result.dynamics.push(DynamicBinding {
                        elem: elem_id.to_string(),
                        key: key.into_owned(),
                        value: value_expr,
                        is_svg: false,
                        is_ce: false,
                        tag_name: tag_name.to_string(),
                    });
                } else {
                    push_expr_statement(
                        result,
                        ast,
                        attr.span,
                        set_prop_expr_with_value(
                            ast, attr.span, context, elem_id, &key, value_expr,
                        ),
                    );
                }
            }
            continue;
        }

        let value_expr = match &attr.value {
            Some(JSXAttributeValue::StringLiteral(lit)) => {
                ast.expression_string_literal(attr.span, ast.allocator.alloc_str(&lit.value), None)
            }
            None => ast.expression_boolean_literal(attr.span, true),
            _ => ast.expression_boolean_literal(attr.span, true),
        };

        push_expr_statement(
            result,
            ast,
            attr.span,
            set_prop_expr_with_value(ast, attr.span, context, elem_id, &key, value_expr),
        );
    }

    if let Some(spread_expr) = spread_expr {
        push_expr_statement(result, ast, SPAN, spread_expr);
    }

    if has_children {
        None
    } else {
        injected_children
    }
}

fn next_child_id(children: &[TransformResult<'_>], index: usize) -> Option<String> {
    children
        .iter()
        .skip(index + 1)
        .find_map(|child| child.id.clone())
}

fn transform_children<'a, 'b>(
    element: &JSXElement<'a>,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    transform_child: ChildTransformer<'a, 'b>,
    extra_child: Option<TransformResult<'a>>,
    ctx: &TraverseCtx<'a, ()>,
) {
    let ast = context.ast();
    let Some(parent_id) = result.id.clone() else {
        return;
    };

    let mut child_nodes: Vec<TransformResult<'a>> = Vec::new();

    for child in &element.children {
        match child {
            JSXChild::Text(text) => {
                let content = normalize_jsx_text(text);
                if content.is_empty() {
                    continue;
                }
                child_nodes.push(TransformResult {
                    span: text.span,
                    template: content.into_owned(),
                    text: true,
                    id: Some(context.generate_uid("el$")),
                    ..Default::default()
                });
            }
            JSXChild::ExpressionContainer(container) => {
                if let Some(expr) = container.expression.as_expression() {
                    let has_static_marker =
                        context.has_static_marker_comment(container.span, options.static_marker);
                    child_nodes.push(build_child_result_from_expression(
                        expr,
                        container.span,
                        context,
                        options,
                        ctx,
                        has_static_marker,
                    ));
                }
            }
            JSXChild::Spread(spread) => {
                let has_static_marker =
                    context.has_static_marker_comment(spread.span, options.static_marker);
                child_nodes.push(build_child_result_from_expression(
                    &spread.expression,
                    spread.span,
                    context,
                    options,
                    ctx,
                    has_static_marker,
                ));
            }
            _ => {
                if let Some(child_result) = transform_child(child) {
                    child_nodes.push(child_result);
                }
            }
        }
    }

    if let Some(child) = extra_child {
        child_nodes.push(child);
    }

    if child_nodes.is_empty() {
        return;
    }

    // Normalize any literal expression results to text before marker resolution.
    for child in &mut child_nodes {
        if !child.text && child.id.is_none() && child.exprs.len() == 1 {
            if let Some(text) = literal_text_from_expr(&child.exprs[0]) {
                child.text = true;
                child.template = escape_html(&text, false).into_owned();
                child.id = Some(context.generate_uid("el$"));
                child.exprs.clear();
            }
        }

        if child.text && child.id.is_none() {
            child.id = Some(context.generate_uid("el$"));
        }
    }

    let multi = child_nodes.len() > 1;

    // Merge adjacent text nodes while preserving consumed ids.
    let mut merged_children: Vec<TransformResult<'a>> = Vec::with_capacity(child_nodes.len());
    for child in child_nodes {
        if child.text && merged_children.last().is_some_and(|previous| previous.text) {
            let previous = merged_children.last_mut().unwrap();
            previous.template.push_str(&child.template);
        } else {
            merged_children.push(child);
        }
    }
    let mut child_nodes = merged_children;

    let mut appends: Vec<Statement<'a>> = Vec::new();

    for index in 0..child_nodes.len() {
        let next_marker = if multi {
            next_child_id(&child_nodes, index)
        } else {
            None
        };

        let child = &mut child_nodes[index];

        if !child.child_results.is_empty() {
            let expr = build_universal_output_expr(child, context);
            child.child_results.clear();
            child.exprs.clear();
            child.exprs.push(expr);
        }

        if child.text || child.id.is_some() {
            let insert_node = helper_ident_expr(ast, child.span, "insertNode");
            context.register_helper("insertNode");

            let insert_child = if child.text {
                context.register_helper("createTextNode");
                let create_text_node = helper_ident_expr(ast, child.span, "createTextNode");
                let text = template_literal_expr_from_raw(ast, child.span, &child.template);

                if multi {
                    let text_id = child
                        .id
                        .clone()
                        .unwrap_or_else(|| context.generate_uid("el$"));
                    child.id = Some(text_id.clone());

                    result.declarations.push(Declaration {
                        pattern: ast.binding_pattern_binding_identifier(
                            child.span,
                            ast.allocator.alloc_str(&text_id),
                        ),
                        init: call_expr(ast, child.span, create_text_node, [text]),
                    });

                    ident_expr(ast, child.span, &text_id)
                } else {
                    call_expr(ast, child.span, create_text_node, [text])
                }
            } else {
                let child_id = child.id.clone().unwrap();
                ident_expr(ast, child.span, &child_id)
            };

            let parent = ident_expr(ast, child.span, &parent_id);
            let append_expr = call_expr(ast, child.span, insert_node, [parent, insert_child]);
            appends.push(Statement::ExpressionStatement(
                ast.alloc_expression_statement(child.span, append_expr),
            ));

            result.declarations.extend(child.declarations.drain(..));
            result.statements.extend(child.statements.drain(..));
            result.dynamics.extend(child.dynamics.drain(..));
            result.post_exprs.extend(child.post_exprs.drain(..));
            for expr in child.exprs.drain(..) {
                push_expr_statement(result, ast, child.span, expr);
            }
            continue;
        }

        if !child.exprs.is_empty() {
            let parent = ident_expr(ast, child.span, &parent_id);
            let value = child.exprs[0].clone_in(ast.allocator);
            let insert_callee = context.helper_ident_expr_with_source(
                ast,
                child.span,
                "insert",
                HelperSource::Universal,
            );

            let insert_expr = if multi {
                let marker = if let Some(marker_id) = next_marker.as_ref() {
                    ident_expr(ast, child.span, marker_id)
                } else {
                    ast.expression_null_literal(child.span)
                };

                call_expr(ast, child.span, insert_callee, [parent, value, marker])
            } else {
                call_expr(ast, child.span, insert_callee, [parent, value])
            };

            result.declarations.extend(child.declarations.drain(..));
            result.statements.extend(child.statements.drain(..));
            result.dynamics.extend(child.dynamics.drain(..));
            result.post_exprs.extend(child.post_exprs.drain(..));
            push_expr_statement(result, ast, child.span, insert_expr);
        }
    }

    if !appends.is_empty() {
        let mut merged = Vec::with_capacity(appends.len() + result.statements.len());
        merged.extend(appends);
        merged.extend(result.statements.drain(..));
        result.statements = merged;
    }
}

pub fn transform_element<'a, 'b>(
    element: &JSXElement<'a>,
    tag_name: &str,
    _info: &TransformInfo,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    transform_child: ChildTransformer<'a, 'b>,
    ctx: &TraverseCtx<'a, ()>,
) -> TransformResult<'a> {
    let ast = context.ast();

    let elem_id = context.generate_uid("el$");
    let mut result = TransformResult {
        span: element.span,
        tag_name: Some(tag_name.to_string()),
        output_kind: OutputKind::Universal,
        id: Some(elem_id.clone()),
        ..Default::default()
    };

    context.register_helper("createElement");
    result.declarations.push(Declaration {
        pattern: ast.binding_pattern_binding_identifier(SPAN, ast.allocator.alloc_str(&elem_id)),
        init: call_expr(
            ast,
            SPAN,
            helper_ident_expr(ast, SPAN, "createElement"),
            [ast.expression_string_literal(SPAN, ast.allocator.alloc_str(tag_name), None)],
        ),
    });

    let extra_child = transform_attributes(
        element,
        tag_name,
        &elem_id,
        &mut result,
        context,
        options,
        ctx,
    );

    transform_children(
        element,
        &mut result,
        context,
        options,
        transform_child,
        extra_child,
        ctx,
    );

    result
}
