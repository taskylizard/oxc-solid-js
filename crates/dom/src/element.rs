//! Native element transform
//! Handles <div>, <span>, etc. -> template + effects

use oxc_allocator::CloneIn;
use oxc_ast::ast::{
    Argument, Expression, FormalParameterKind, FunctionType, JSXAttribute, JSXAttributeItem,
    JSXAttributeName, JSXAttributeValue, JSXElement, ObjectPropertyKind, PropertyKey, PropertyKind,
    Statement, TemplateElementValue, VariableDeclarationKind,
};
use oxc_ast::AstBuilder;
use oxc_ast::NONE;
use oxc_span::{Span, SPAN};
use oxc_syntax::identifier::is_identifier_name;
use oxc_syntax::keyword::is_reserved_keyword;
use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator};
use oxc_syntax::symbol::SymbolFlags;
use oxc_traverse::TraverseCtx;

use common::{
    constants::{
        ALIASES, ALWAYS_CLOSE, BLOCK_ELEMENTS, CHILD_PROPERTIES, DELEGATED_EVENTS, INLINE_ELEMENTS,
        VOID_ELEMENTS,
    },
    expression::{
        escape_html, jsx_text_source, normalize_jsx_text, to_event_name, trim_whitespace,
    },
    get_attr_name, is_component, is_dynamic, is_namespaced_attr, is_svg_element, TransformOptions,
};

use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::conditional::{is_condition_expression, transform_condition_non_inline_insert};
use crate::expression_utils::{
    expression_to_assignment_target, peel_wrapped_expression as unwrap_ts_expression,
};
use crate::ir::{
    helper_ident_expr, BlockContext, ChildTransformer, Declaration, DynamicBinding, HelperSource,
    StaticTextValue, TransformResult,
};
use crate::output::register_dynamic_binding_helper;
use crate::transform::TransformInfo;

fn ident_expr<'a>(ast: AstBuilder<'a>, span: Span, name: &str) -> Expression<'a> {
    let _ = span;
    match name {
        "NoHydration" | "addEventListener" | "className" | "createComponent" | "effect"
        | "getNextElement" | "getNextMarker" | "getNextMatch" | "getOwner" | "insert" | "memo"
        | "mergeProps" | "runHydrationEvents" | "setAttribute" | "setAttributeNS"
        | "setBoolAttribute" | "setProperty" | "setStyleProperty" | "spread" | "style"
        | "template" | "use" => helper_ident_expr(ast, SPAN, name),
        _ => ast.expression_identifier(SPAN, ast.allocator.alloc_str(name)),
    }
}

fn dom_helper_expr<'a>(
    context: &BlockContext<'a>,
    ast: AstBuilder<'a>,
    span: Span,
    name: &str,
) -> Expression<'a> {
    context.helper_ident_expr_with_source(ast, span, name, HelperSource::Dom)
}

fn static_member<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    object: Expression<'a>,
    property: &str,
) -> Expression<'a> {
    let _ = span;
    let prop = ast.identifier_name(SPAN, ast.allocator.alloc_str(property));
    Expression::StaticMemberExpression(
        ast.alloc_static_member_expression(SPAN, object, prop, false),
    )
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

fn bool_cast_expr<'a>(ast: AstBuilder<'a>, span: Span, expr: Expression<'a>) -> Expression<'a> {
    let _ = span;
    let not_expr = ast.expression_unary(SPAN, UnaryOperator::LogicalNot, expr);
    ast.expression_unary(SPAN, UnaryOperator::LogicalNot, not_expr)
}

fn class_toggle_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    elem_id: &str,
    class_name: &str,
    value: Expression<'a>,
) -> Expression<'a> {
    let elem = ident_expr(ast, span, elem_id);
    let class_list = static_member(ast, span, elem, "classList");
    let toggle = static_member(ast, span, class_list, "toggle");
    let class_name_lit =
        ast.expression_string_literal(SPAN, ast.allocator.alloc_str(class_name), None);
    call_expr(ast, span, toggle, [class_name_lit, value])
}

fn set_style_property_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    elem_id: &str,
    prop_name: &str,
    value: Expression<'a>,
) -> Expression<'a> {
    let callee = ident_expr(ast, span, "setStyleProperty");
    let elem = ident_expr(ast, span, elem_id);
    let prop_name_lit =
        ast.expression_string_literal(SPAN, ast.allocator.alloc_str(prop_name), None);
    call_expr(ast, span, callee, [elem, prop_name_lit, value])
}

fn arrow_zero_params_return_expr<'a>(
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

fn arrow_single_param_return_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    param_name: &str,
    expr: Expression<'a>,
) -> Expression<'a> {
    let _ = span;
    let param = ast.binding_pattern_binding_identifier(SPAN, ast.allocator.alloc_str(param_name));
    let params = ast.alloc_formal_parameters(
        SPAN,
        FormalParameterKind::ArrowFormalParameters,
        ast.vec1(ast.plain_formal_parameter(SPAN, param)),
        NONE,
    );
    let mut statements = ast.vec_with_capacity(1);
    statements.push(Statement::ExpressionStatement(
        ast.alloc_expression_statement(SPAN, expr),
    ));
    let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
    ast.expression_arrow_function(SPAN, true, false, NONE, params, NONE, body)
}

fn arrow_single_param_statement_expr<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    param_name: &str,
    expr: Expression<'a>,
) -> Expression<'a> {
    let _ = span;
    let param = ast.binding_pattern_binding_identifier(SPAN, ast.allocator.alloc_str(param_name));
    let params = ast.alloc_formal_parameters(
        SPAN,
        FormalParameterKind::ArrowFormalParameters,
        ast.vec1(ast.plain_formal_parameter(SPAN, param)),
        NONE,
    );
    let mut statements = ast.vec_with_capacity(1);
    statements.push(Statement::ExpressionStatement(
        ast.alloc_expression_statement(SPAN, expr),
    ));
    let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
    ast.expression_arrow_function(SPAN, false, false, NONE, params, NONE, body)
}

fn template_literal_expr_from_raw<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    raw_value: &str,
) -> Expression<'a> {
    let _ = span;
    let raw = ast.allocator.alloc_str(raw_value);
    let value = TemplateElementValue {
        raw: ast.str(raw),
        cooked: Some(ast.str(raw)),
    };
    let quasis = ast.vec1(ast.template_element(SPAN, value, true));
    let template = ast.template_literal(SPAN, quasis, ast.vec());
    Expression::TemplateLiteral(ast.alloc(template))
}

fn emit_inline_styles_disabled_runtime_update<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    elem_id: &str,
    style_value: Expression<'a>,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
) {
    let _ = span;
    let elem = ident_expr(ast, SPAN, elem_id);
    let style = ident_expr(ast, SPAN, "style");

    if effect_wrapper_enabled(options) {
        result.dynamics.push(DynamicBinding {
            elem: elem_id.to_string(),
            key: "style".to_string(),
            value: style_value,
            is_svg: false,
            is_ce: false,
            tag_name: result.tag_name.clone().unwrap_or_default(),
        });
    } else {
        context.register_helper("style");
        let style_call = call_expr(ast, SPAN, style, [elem, style_value]);
        result.exprs.push(style_call);
    }
}

fn effect_wrapper_enabled(options: &TransformOptions<'_>) -> bool {
    !options.effect_wrapper.is_empty()
}

fn memo_wrapper_enabled(options: &TransformOptions<'_>) -> bool {
    !options.memo_wrapper.is_empty()
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

    !is_dynamic(unwrap_ts_expression(expr))
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
) -> Expression<'a> {
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

fn arrow_zero_params_return_value<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    expr: Expression<'a>,
) -> Expression<'a> {
    arrow_zero_params_return_expr(ast, span, expr)
}

fn getter_return_expr<'a>(ast: AstBuilder<'a>, span: Span, expr: Expression<'a>) -> Expression<'a> {
    let _ = span;
    let params =
        ast.alloc_formal_parameters(SPAN, FormalParameterKind::FormalParameter, ast.vec(), NONE);
    let mut statements = ast.vec_with_capacity(1);
    statements.push(Statement::ReturnStatement(
        ast.alloc_return_statement(SPAN, Some(expr)),
    ));
    let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
    ast.expression_function(
        SPAN,
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

fn is_valid_prop_identifier(key: &str) -> bool {
    is_identifier_name(key)
}

fn is_valid_getter_prop_identifier(key: &str) -> bool {
    is_identifier_name(key) && !is_reserved_keyword(key)
}

fn make_prop_key<'a>(ast: AstBuilder<'a>, span: Span, raw_key: &str) -> PropertyKey<'a> {
    let _ = span;
    let key = ast.allocator.alloc_str(raw_key);
    if is_valid_prop_identifier(raw_key) {
        PropertyKey::StaticIdentifier(ast.alloc_identifier_name(SPAN, key))
    } else {
        PropertyKey::StringLiteral(ast.alloc_string_literal(SPAN, key, None))
    }
}

fn make_getter_prop_key<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    raw_key: &str,
) -> (PropertyKey<'a>, bool) {
    let _ = span;
    let key = ast.allocator.alloc_str(raw_key);
    let key_is_identifier = is_valid_getter_prop_identifier(raw_key);
    let prop_key = if key_is_identifier {
        PropertyKey::StaticIdentifier(ast.alloc_identifier_name(SPAN, key))
    } else {
        PropertyKey::StringLiteral(ast.alloc_string_literal(SPAN, key, None))
    };

    (prop_key, key_is_identifier)
}

fn can_native_spread(key: &str, check_namespaces: bool) -> bool {
    if check_namespaces {
        if let Some((ns, _)) = key.split_once(':') {
            if matches!(ns, "class" | "style" | "use" | "prop" | "attr" | "bool") {
                return false;
            }
        }
    }
    key != "ref"
}

fn push_template_attr(result: &mut TransformResult<'_>, segment: &str) {
    // Keep `template_with_closing_tags` attribute-free for parse5/html validation parity.
    // Babel no longer appends attributes to the validation template string.
    result.template.push_str(segment);
}

fn normalize_inline_attr_value(key: &str, value: &str) -> String {
    let mut normalized = if key == "style" || key == "class" {
        trim_whitespace(value).into_owned()
    } else {
        value.to_string()
    };

    if key == "style" {
        normalized = normalized.replace("; ", ";").replace(": ", ":");
        if normalized.ends_with(';') {
            normalized.pop();
        }
    }

    normalized
}

fn inline_attribute_on_template(
    result: &mut TransformResult<'_>,
    is_svg: bool,
    key: &str,
    value: Option<&str>,
    omit_quotes: bool,
    needs_spacing: &mut bool,
) {
    let mut normalized_key = key.to_string();
    if !is_svg && key != "className" {
        normalized_key = normalized_key.to_ascii_lowercase();
    }

    let prefix = if *needs_spacing { " " } else { "" };
    push_template_attr(result, &format!("{}{}", prefix, normalized_key));

    let Some(raw_value) = value else {
        *needs_spacing = true;
        return;
    };

    let value = normalize_inline_attr_value(&normalized_key, raw_value);
    if value.is_empty() {
        *needs_spacing = true;
        return;
    }

    let mut needs_quoting = !omit_quotes;
    for ch in value.chars() {
        if matches!(
            ch,
            '\'' | '"' | ' ' | '\t' | '\n' | '\r' | '`' | '=' | '<' | '>'
        ) {
            needs_quoting = true;
            break;
        }
    }

    let escaped = escape_html(&value, true);
    if needs_quoting {
        push_template_attr(result, &format!("=\"{}\"", escaped));
        *needs_spacing = false;
    } else {
        push_template_attr(result, &format!("={}", escaped));
        *needs_spacing = true;
    }
}

#[derive(Clone, Debug)]
enum StaticPrimitiveValue {
    String(String),
    Number(f64),
    Boolean(bool),
    Null,
    Undefined,
}

impl StaticPrimitiveValue {
    fn from_static_text_value(value: StaticTextValue) -> Self {
        match value {
            StaticTextValue::String(value) => Self::String(value),
            StaticTextValue::Number(value) => Self::Number(value),
        }
    }

    fn into_static_text_value(self) -> Option<StaticTextValue> {
        match self {
            Self::String(value) => Some(StaticTextValue::String(value)),
            Self::Number(value) => Some(StaticTextValue::Number(value)),
            Self::Boolean(_) | Self::Null | Self::Undefined => None,
        }
    }

    fn as_js_text(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Number(value) => StaticTextValue::Number(*value).as_text(),
            Self::Boolean(value) => value.to_string(),
            Self::Null => "null".to_string(),
            Self::Undefined => "undefined".to_string(),
        }
    }

    fn is_truthy(&self) -> bool {
        match self {
            Self::String(value) => !value.is_empty(),
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::Boolean(value) => *value,
            Self::Null | Self::Undefined => false,
        }
    }

    fn is_nullish(&self) -> bool {
        matches!(self, Self::Null | Self::Undefined)
    }

    fn strict_equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Number(left), Self::Number(right)) => left == right,
            (Self::Boolean(left), Self::Boolean(right)) => left == right,
            (Self::Null, Self::Null) => true,
            (Self::Undefined, Self::Undefined) => true,
            _ => false,
        }
    }
}

fn compare_utf16_strings(left: &str, right: &str) -> std::cmp::Ordering {
    let left = left.encode_utf16().collect::<Vec<_>>();
    let right = right.encode_utf16().collect::<Vec<_>>();
    left.cmp(&right)
}

fn compare_numeric_values(operator: BinaryOperator, left: f64, right: f64) -> bool {
    match operator {
        BinaryOperator::LessThan => left < right,
        BinaryOperator::LessEqualThan => left <= right,
        BinaryOperator::GreaterThan => left > right,
        BinaryOperator::GreaterEqualThan => left >= right,
        _ => false,
    }
}

fn compare_primitive_values(
    operator: BinaryOperator,
    left: &StaticPrimitiveValue,
    right: &StaticPrimitiveValue,
) -> Option<bool> {
    match (left, right) {
        (StaticPrimitiveValue::Number(left), StaticPrimitiveValue::Number(right)) => {
            Some(compare_numeric_values(operator, *left, *right))
        }
        (StaticPrimitiveValue::String(left), StaticPrimitiveValue::String(right)) => {
            let ordering = compare_utf16_strings(left, right);
            let value = match operator {
                BinaryOperator::LessThan => ordering.is_lt(),
                BinaryOperator::LessEqualThan => !ordering.is_gt(),
                BinaryOperator::GreaterThan => ordering.is_gt(),
                BinaryOperator::GreaterEqualThan => !ordering.is_lt(),
                _ => return None,
            };
            Some(value)
        }
        _ => None,
    }
}

fn loose_equals(left: &StaticPrimitiveValue, right: &StaticPrimitiveValue) -> Option<bool> {
    match (left, right) {
        (StaticPrimitiveValue::Null, StaticPrimitiveValue::Undefined)
        | (StaticPrimitiveValue::Undefined, StaticPrimitiveValue::Null) => Some(true),
        (StaticPrimitiveValue::String(_), StaticPrimitiveValue::String(_))
        | (StaticPrimitiveValue::Number(_), StaticPrimitiveValue::Number(_))
        | (StaticPrimitiveValue::Boolean(_), StaticPrimitiveValue::Boolean(_))
        | (StaticPrimitiveValue::Null, StaticPrimitiveValue::Null)
        | (StaticPrimitiveValue::Undefined, StaticPrimitiveValue::Undefined) => {
            Some(left.strict_equals(right))
        }
        _ => None,
    }
}

fn is_confident_static_expression<'a>(expr: &Expression<'a>) -> bool {
    match unwrap_ts_expression(expr) {
        Expression::StringLiteral(_)
        | Expression::NumericLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_) => true,
        Expression::TemplateLiteral(template) => template
            .expressions
            .iter()
            .all(is_confident_static_expression),
        Expression::ArrayExpression(array) => array.elements.iter().all(|element| match element {
            oxc_ast::ast::ArrayExpressionElement::SpreadElement(_) => false,
            oxc_ast::ast::ArrayExpressionElement::Elision(_) => true,
            _ => element
                .as_expression()
                .is_some_and(is_confident_static_expression),
        }),
        Expression::ObjectExpression(object) => {
            object.properties.iter().all(|property| match property {
                ObjectPropertyKind::SpreadProperty(_) => false,
                ObjectPropertyKind::ObjectProperty(prop) => {
                    !prop.computed && is_confident_static_expression(&prop.value)
                }
            })
        }
        Expression::UnaryExpression(unary) => is_confident_static_expression(&unary.argument),
        Expression::BinaryExpression(binary) => {
            is_confident_static_expression(&binary.left)
                && is_confident_static_expression(&binary.right)
        }
        Expression::LogicalExpression(logical) => {
            is_confident_static_expression(&logical.left)
                && is_confident_static_expression(&logical.right)
        }
        Expression::ConditionalExpression(cond) => {
            is_confident_static_expression(&cond.test)
                && is_confident_static_expression(&cond.consequent)
                && is_confident_static_expression(&cond.alternate)
        }
        _ => false,
    }
}

fn static_identifier_value<'a>(
    ident: &oxc_ast::ast::IdentifierReference<'a>,
    context: &BlockContext<'a>,
    ctx: &TraverseCtx<'a, ()>,
) -> Option<StaticTextValue> {
    let reference_id = ident.reference_id.get()?;
    let reference = ctx.scoping().get_reference(reference_id);
    let symbol_id = reference.symbol_id()?;
    context.get_constant_text_value(symbol_id)
}

fn static_identifier_primitive_value<'a>(
    ident: &oxc_ast::ast::IdentifierReference<'a>,
    context: &BlockContext<'a>,
    ctx: &TraverseCtx<'a, ()>,
) -> Option<StaticPrimitiveValue> {
    if let Some(value) = static_identifier_value(ident, context, ctx) {
        return Some(StaticPrimitiveValue::from_static_text_value(value));
    }

    if ident.name == "undefined" {
        let unresolved = match ident.reference_id.get() {
            Some(reference_id) => {
                let reference = ctx.scoping().get_reference(reference_id);
                reference.symbol_id().is_none()
            }
            None => true,
        };
        if unresolved {
            return Some(StaticPrimitiveValue::Undefined);
        }
    }

    None
}

fn evaluate_static_primitive_expression<'a>(
    expr: &Expression<'a>,
    context: &BlockContext<'a>,
    ctx: &TraverseCtx<'a, ()>,
) -> Option<StaticPrimitiveValue> {
    match unwrap_ts_expression(expr) {
        Expression::StringLiteral(lit) => Some(StaticPrimitiveValue::String(lit.value.to_string())),
        Expression::NumericLiteral(num) => Some(StaticPrimitiveValue::Number(num.value)),
        Expression::BooleanLiteral(lit) => Some(StaticPrimitiveValue::Boolean(lit.value)),
        Expression::NullLiteral(_) => Some(StaticPrimitiveValue::Null),
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => {
            let text = template
                .quasis
                .iter()
                .map(|quasi| {
                    quasi
                        .value
                        .cooked
                        .as_ref()
                        .map(|cooked| cooked.as_str())
                        .unwrap_or_else(|| quasi.value.raw.as_str())
                })
                .collect::<String>();
            Some(StaticPrimitiveValue::String(text))
        }
        Expression::TemplateLiteral(template) => {
            let mut text = String::new();
            for (index, quasi) in template.quasis.iter().enumerate() {
                let cooked = quasi
                    .value
                    .cooked
                    .as_ref()
                    .map(|cooked| cooked.as_str())
                    .unwrap_or_else(|| quasi.value.raw.as_str());
                text.push_str(cooked);

                if let Some(expr) = template.expressions.get(index) {
                    let value = evaluate_static_primitive_expression(expr, context, ctx)?;
                    text.push_str(&value.as_js_text());
                }
            }
            Some(StaticPrimitiveValue::String(text))
        }
        Expression::Identifier(ident) => static_identifier_primitive_value(ident, context, ctx),
        Expression::UnaryExpression(unary) => {
            let value = evaluate_static_primitive_expression(&unary.argument, context, ctx)?;
            match unary.operator {
                UnaryOperator::UnaryPlus => {
                    let StaticPrimitiveValue::Number(number) = value else {
                        return None;
                    };
                    Some(StaticPrimitiveValue::Number(number))
                }
                UnaryOperator::UnaryNegation => {
                    let StaticPrimitiveValue::Number(number) = value else {
                        return None;
                    };
                    Some(StaticPrimitiveValue::Number(-number))
                }
                UnaryOperator::LogicalNot => {
                    Some(StaticPrimitiveValue::Boolean(!value.is_truthy()))
                }
                _ => None,
            }
        }
        Expression::BinaryExpression(binary) => {
            let left = evaluate_static_primitive_expression(&binary.left, context, ctx)?;
            let right = evaluate_static_primitive_expression(&binary.right, context, ctx)?;

            match binary.operator {
                BinaryOperator::Addition => match (&left, &right) {
                    (
                        StaticPrimitiveValue::Number(left_number),
                        StaticPrimitiveValue::Number(right_number),
                    ) => Some(StaticPrimitiveValue::Number(left_number + right_number)),
                    (StaticPrimitiveValue::String(_), _) | (_, StaticPrimitiveValue::String(_)) => {
                        Some(StaticPrimitiveValue::String(format!(
                            "{}{}",
                            left.as_js_text(),
                            right.as_js_text()
                        )))
                    }
                    _ => None,
                },
                BinaryOperator::Subtraction => {
                    let (
                        StaticPrimitiveValue::Number(left_number),
                        StaticPrimitiveValue::Number(right_number),
                    ) = (&left, &right)
                    else {
                        return None;
                    };
                    Some(StaticPrimitiveValue::Number(left_number - right_number))
                }
                BinaryOperator::Multiplication => {
                    let (
                        StaticPrimitiveValue::Number(left_number),
                        StaticPrimitiveValue::Number(right_number),
                    ) = (&left, &right)
                    else {
                        return None;
                    };
                    Some(StaticPrimitiveValue::Number(left_number * right_number))
                }
                BinaryOperator::Division => {
                    let (
                        StaticPrimitiveValue::Number(left_number),
                        StaticPrimitiveValue::Number(right_number),
                    ) = (&left, &right)
                    else {
                        return None;
                    };
                    Some(StaticPrimitiveValue::Number(left_number / right_number))
                }
                BinaryOperator::Remainder => {
                    let (
                        StaticPrimitiveValue::Number(left_number),
                        StaticPrimitiveValue::Number(right_number),
                    ) = (&left, &right)
                    else {
                        return None;
                    };
                    Some(StaticPrimitiveValue::Number(left_number % right_number))
                }
                BinaryOperator::Exponential => {
                    let (
                        StaticPrimitiveValue::Number(left_number),
                        StaticPrimitiveValue::Number(right_number),
                    ) = (&left, &right)
                    else {
                        return None;
                    };
                    Some(StaticPrimitiveValue::Number(
                        left_number.powf(*right_number),
                    ))
                }
                BinaryOperator::Equality => {
                    let equals = loose_equals(&left, &right)?;
                    Some(StaticPrimitiveValue::Boolean(equals))
                }
                BinaryOperator::Inequality => {
                    let equals = loose_equals(&left, &right)?;
                    Some(StaticPrimitiveValue::Boolean(!equals))
                }
                BinaryOperator::StrictEquality => {
                    Some(StaticPrimitiveValue::Boolean(left.strict_equals(&right)))
                }
                BinaryOperator::StrictInequality => {
                    Some(StaticPrimitiveValue::Boolean(!left.strict_equals(&right)))
                }
                BinaryOperator::LessThan
                | BinaryOperator::LessEqualThan
                | BinaryOperator::GreaterThan
                | BinaryOperator::GreaterEqualThan => {
                    let comparison = compare_primitive_values(binary.operator, &left, &right)?;
                    Some(StaticPrimitiveValue::Boolean(comparison))
                }
                _ => None,
            }
        }
        Expression::LogicalExpression(logical) => {
            let left = evaluate_static_primitive_expression(&logical.left, context, ctx)?;
            match logical.operator {
                LogicalOperator::And => {
                    if left.is_truthy() {
                        evaluate_static_primitive_expression(&logical.right, context, ctx)
                    } else {
                        Some(left)
                    }
                }
                LogicalOperator::Or => {
                    if left.is_truthy() {
                        Some(left)
                    } else {
                        evaluate_static_primitive_expression(&logical.right, context, ctx)
                    }
                }
                LogicalOperator::Coalesce => {
                    if left.is_nullish() {
                        evaluate_static_primitive_expression(&logical.right, context, ctx)
                    } else {
                        Some(left)
                    }
                }
            }
        }
        Expression::ConditionalExpression(cond) => {
            let test = evaluate_static_primitive_expression(&cond.test, context, ctx)?;
            if test.is_truthy() {
                evaluate_static_primitive_expression(&cond.consequent, context, ctx)
            } else {
                evaluate_static_primitive_expression(&cond.alternate, context, ctx)
            }
        }
        _ => None,
    }
}

pub(crate) fn evaluate_static_text_expression<'a>(
    expr: &Expression<'a>,
    context: &BlockContext<'a>,
    ctx: &TraverseCtx<'a, ()>,
) -> Option<StaticTextValue> {
    evaluate_static_primitive_expression(expr, context, ctx)?.into_static_text_value()
}

pub(crate) fn static_child_text<'a>(
    expr: &Expression<'a>,
    context: &BlockContext<'a>,
    ctx: &TraverseCtx<'a, ()>,
) -> Option<String> {
    evaluate_static_text_expression(expr, context, ctx).map(|value| value.as_text())
}

/// Transform a native HTML/SVG element
pub fn transform_element<'a, 'b>(
    element: &JSXElement<'a>,
    tag_name: &str,
    info: &TransformInfo,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    transform_child: ChildTransformer<'a, 'b>,
    ctx: &TraverseCtx<'a, ()>,
) -> TransformResult<'a> {
    let ast = context.ast();
    let is_svg = is_svg_element(tag_name);
    let is_void = VOID_ELEMENTS.contains(tag_name);
    let has_is_attr = element
        .opening_element
        .attributes
        .iter()
        .any(|attr| match attr {
            JSXAttributeItem::Attribute(attr) => {
                matches!(&attr.name, JSXAttributeName::Identifier(id) if id.name == "is")
            }
            _ => false,
        });
    let is_custom_element = tag_name.contains('-') || has_is_attr;
    let is_import_node = matches!(tag_name, "img" | "iframe")
        && element
            .opening_element
            .attributes
            .iter()
            .any(|attr| match attr {
                JSXAttributeItem::Attribute(attr) => {
                    matches!(&attr.name, JSXAttributeName::Identifier(id) if id.name == "loading")
                }
                _ => false,
            });
    let wrap_svg = info.top_level && tag_name != "svg" && is_svg;

    let mut result = TransformResult {
        span: element.span,
        tag_name: Some(tag_name.to_string()),
        is_svg,
        template_is_svg: wrap_svg,
        has_custom_element: is_custom_element,
        is_import_node,
        ..Default::default()
    };

    let hydratable = options.hydratable;
    if hydratable && matches!(tag_name, "html" | "head" | "body") {
        result.skip_template = true;
        if tag_name == "head" && info.top_level {
            // Hydratable document parity: <head> content is never hydrated on the client.
            // Lower it to `createComponent(NoHydration, {})` to opt out of hydration work.
            context.register_helper("createComponent");
            context.register_helper("NoHydration");
            let callee = ident_expr(ast, element.span, "createComponent");
            let no_hydration = ident_expr(ast, element.span, "NoHydration");
            let props = ast.expression_object(element.span, ast.vec());

            result
                .exprs
                .push(call_expr(ast, element.span, callee, [no_hydration, props]));
            return result;
        }
    }

    // Check if this element needs runtime access (dynamic attributes, refs, events)
    let needs_runtime_access = element_needs_runtime_access(element, options);

    // Generate element ID if needed
    if !info.skip_id && (info.top_level || needs_runtime_access) {
        if info.path.is_empty() {
            if let Some(root_id) = &info.root_id {
                result.id = Some(root_id.clone());
            } else {
                result.id = Some(context.generate_uid("el$"));
            }
        } else {
            let elem_id = info
                .forced_id
                .clone()
                .unwrap_or_else(|| context.generate_uid("el$"));
            result.id = Some(elem_id.clone());

            if let Some(root_id) = &info.root_id {
                let walk = info
                    .path
                    .iter()
                    .fold(ident_expr(ast, element.span, root_id), |acc, step| {
                        static_member(ast, element.span, acc, step)
                    });
                let init = if hydratable {
                    if let Some(match_tag) = info.match_tag.as_ref() {
                        context.register_helper("getNextMatch");
                        let callee = ident_expr(ast, element.span, "getNextMatch");
                        let tag_lit = ast.expression_string_literal(
                            element.span,
                            ast.allocator.alloc_str(match_tag),
                            None,
                        );
                        call_expr(ast, element.span, callee, [walk, tag_lit])
                    } else {
                        walk
                    }
                } else {
                    walk
                };
                result.declarations.push(Declaration {
                    pattern: ast.binding_pattern_binding_identifier(
                        element.span,
                        ast.allocator.alloc_str(&elem_id),
                    ),
                    init,
                });
            }
        }
    }

    // Start building template
    result.template = format!("<{}", tag_name);
    result.template_with_closing_tags = result.template.clone();
    if wrap_svg {
        result.template = format!("<svg>{}", result.template);
        result.template_with_closing_tags = format!("<svg>{}", result.template_with_closing_tags);
    }

    // Transform attributes
    let attributes_result = transform_attributes(element, &mut result, context, options, ctx);
    let needs_text_content_placeholder = attributes_result.needs_text_content_placeholder;
    let synthetic_children = attributes_result.synthetic_children;

    if options.context_to_custom_elements && (tag_name == "slot" || is_custom_element) {
        if let Some(elem_id) = result.id.as_ref() {
            context.register_helper("getOwner");
            let elem = ident_expr(ast, element.span, elem_id);
            let member = static_member(ast, element.span, elem, "_$owner");
            if let Some(target) = expression_to_assignment_target(member) {
                let assign = ast.expression_assignment(
                    SPAN,
                    AssignmentOperator::Assign,
                    target,
                    call_expr(
                        ast,
                        element.span,
                        ident_expr(ast, element.span, "getOwner"),
                        [],
                    ),
                );
                result.exprs.push(assign);
            }
        }
    }

    if let Some((children_expr, force_static_children)) = synthetic_children {
        let elem_id = result
            .id
            .as_deref()
            .expect("children attribute insertion requires an element id");
        context.register_helper("insert");

        let insert_value = if force_static_children {
            children_expr
        } else {
            normalize_insert_value(ast, SPAN, &children_expr, context, options)
        };

        let callee = dom_helper_expr(context, ast, SPAN, "insert");
        let parent = ident_expr(ast, SPAN, elem_id);
        result
            .exprs
            .push(call_expr(ast, SPAN, callee, [parent, insert_value]));
    }

    // Close opening tag
    result.template.push('>');
    result.template_with_closing_tags.push('>');
    if needs_text_content_placeholder && !is_void {
        result.template.push(' ');
        result.template_with_closing_tags.push(' ');
    }

    // Transform children (if not void element)
    if !is_void {
        let should_close_tag = !info.last_element
            || !options.omit_last_closing_tag
            || info.to_be_closed.as_ref().is_some_and(|to_be_closed| {
                !options.omit_nested_closing_tags || to_be_closed.contains(tag_name)
            });

        let child_to_be_closed = if should_close_tag {
            let mut to_be_closed = info.to_be_closed.clone().unwrap_or_else(|| {
                ALWAYS_CLOSE
                    .iter()
                    .map(|tag| (*tag).to_string())
                    .collect::<HashSet<String>>()
            });
            to_be_closed.insert(tag_name.to_string());
            if INLINE_ELEMENTS.contains(tag_name) {
                for block in BLOCK_ELEMENTS.iter() {
                    to_be_closed.insert((*block).to_string());
                }
            }
            Some(to_be_closed)
        } else {
            info.to_be_closed.clone()
        };

        // Pass down the root ID and path for children
        // If this element has an ID, it becomes the new root for children
        // and children's paths reset to be relative to this element
        let child_info = TransformInfo {
            root_id: result.id.clone().or_else(|| info.root_id.clone()),
            path: if result.id.is_some() {
                vec![]
            } else {
                info.path.clone()
            },
            top_level: false,
            forced_id: None,
            to_be_closed: child_to_be_closed,
            match_tag: None,
            ..info.clone()
        };
        if tag_name != "noscript" {
            transform_children(
                element,
                &mut result,
                &child_info,
                context,
                options,
                transform_child,
                ctx,
            );
        }

        if should_close_tag {
            result.template.push_str(&format!("</{}>", tag_name));
        }
        result
            .template_with_closing_tags
            .push_str(&format!("</{}>", tag_name));
    }

    if wrap_svg {
        result.template.push_str("</svg>");
        result.template_with_closing_tags.push_str("</svg>");
    }

    if info.top_level && hydratable && result.has_hydratable_event {
        context.register_helper("runHydrationEvents");
        let callee = ident_expr(ast, element.span, "runHydrationEvents");
        result.post_exprs.push(call_expr(
            ast,
            element.span,
            callee,
            std::iter::empty::<Expression<'a>>(),
        ));
    }

    result
}

/// Check if an element needs runtime access
fn element_needs_runtime_access(element: &JSXElement, options: &TransformOptions) -> bool {
    let tag_name = common::get_tag_name(element);
    let has_is_attr = element
        .opening_element
        .attributes
        .iter()
        .any(|attr| match attr {
            JSXAttributeItem::Attribute(attr) => {
                matches!(&attr.name, JSXAttributeName::Identifier(id) if id.name == "is")
            }
            _ => false,
        });

    if options.context_to_custom_elements
        && (tag_name == "slot" || tag_name.contains('-') || has_is_attr)
    {
        return true;
    }

    // Check attributes
    for attr in &element.opening_element.attributes {
        match attr {
            JSXAttributeItem::Attribute(attr) => {
                // Namespaced attributes like on:click or use:directive always need access
                if is_namespaced_attr(&attr.name) {
                    return true;
                }
                let key = get_attr_name(&attr.name);

                // ref and child-property setters need runtime access
                if key == "ref" || CHILD_PROPERTIES.contains(&*key) {
                    return true;
                }

                // Event handlers need access
                if key.starts_with("on") && key.len() > 2 {
                    return true;
                }

                // Any expression container needs runtime access (we may need to run setters/helpers).
                // This keeps id generation consistent with the rest of the transform.
                if matches!(&attr.value, Some(JSXAttributeValue::ExpressionContainer(_))) {
                    return true;
                }
            }
            JSXAttributeItem::SpreadAttribute(_) => {
                // Spread attributes always need runtime access
                return true;
            }
        }
    }

    // Check children for components or dynamic expressions
    // If any child is a component, we need an ID for insert() calls
    fn children_need_runtime_access<'a>(children: &[oxc_ast::ast::JSXChild<'a>]) -> bool {
        for child in children {
            match child {
                oxc_ast::ast::JSXChild::Element(child_elem) => {
                    let child_tag = common::get_tag_name(child_elem);
                    if is_component(&child_tag) {
                        return true;
                    }
                }
                oxc_ast::ast::JSXChild::ExpressionContainer(_) => {
                    return true;
                }
                oxc_ast::ast::JSXChild::Fragment(fragment) => {
                    if children_need_runtime_access(&fragment.children) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    if tag_name != "noscript" && children_need_runtime_access(&element.children) {
        return true;
    }

    false
}

enum MergedClass<'a> {
    Static(String),
    Dynamic(Expression<'a>),
}

fn build_merged_class_value<'a>(
    ast: AstBuilder<'a>,
    context: &BlockContext<'a>,
    class_attrs: &[&JSXAttribute<'a>],
) -> Option<MergedClass<'a>> {
    enum ClassPart<'a> {
        Static(String),
        Dynamic(Expression<'a>),
    }

    let mut parts: Vec<ClassPart<'a>> = Vec::new();

    for attr in class_attrs {
        match &attr.value {
            Some(JSXAttributeValue::StringLiteral(lit)) => {
                parts.push(ClassPart::Static(lit.value.to_string()));
            }
            Some(JSXAttributeValue::ExpressionContainer(container)) => {
                if let Some(expr) = container.expression.as_expression() {
                    match expr {
                        Expression::StringLiteral(lit) => {
                            parts.push(ClassPart::Static(lit.value.to_string()));
                        }
                        Expression::NumericLiteral(num) => {
                            parts.push(ClassPart::Static(num.value.to_string()));
                        }
                        Expression::NullLiteral(_) => {}
                        Expression::Identifier(ident) if ident.name == "undefined" => {}
                        _ => parts.push(ClassPart::Dynamic(context.clone_expr(expr))),
                    }
                }
            }
            _ => {}
        }
    }

    if parts.is_empty() {
        return None;
    }

    let has_dynamic = parts
        .iter()
        .any(|part| matches!(part, ClassPart::Dynamic(_)));
    if !has_dynamic {
        let merged = parts
            .into_iter()
            .filter_map(|part| match part {
                ClassPart::Static(value) if !value.is_empty() => Some(value),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        return Some(MergedClass::Static(merged));
    }

    let mut values = ast.vec();
    let mut quasis_raw: Vec<String> = vec![String::new()];

    let parts_len = parts.len();
    for (index, part) in parts.into_iter().enumerate() {
        let is_last = index + 1 == parts_len;
        match part {
            ClassPart::Static(value) => {
                let mut last = quasis_raw.pop().unwrap_or_default();
                if !value.is_empty() {
                    if !last.is_empty() && !last.ends_with(' ') {
                        last.push(' ');
                    }
                    last.push_str(&value);
                    if !is_last {
                        last.push(' ');
                    }
                }
                quasis_raw.push(last);
            }
            ClassPart::Dynamic(expr) => {
                let empty = ast.expression_string_literal(SPAN, ast.allocator.alloc_str(""), None);
                let or_expr = Expression::LogicalExpression(ast.alloc_logical_expression(
                    SPAN,
                    expr,
                    LogicalOperator::Or,
                    empty,
                ));
                values.push(or_expr);
                quasis_raw.push(if is_last {
                    String::new()
                } else {
                    " ".to_string()
                });
            }
        }
    }

    let quasis_len = quasis_raw.len();
    let mut quasis = ast.vec_with_capacity(quasis_len);
    for (index, raw) in quasis_raw.into_iter().enumerate() {
        let is_tail = index + 1 == quasis_len;
        let raw_str = ast.allocator.alloc_str(&raw);
        let value = TemplateElementValue {
            raw: ast.str(raw_str),
            cooked: Some(ast.str(raw_str)),
        };
        quasis.push(ast.template_element(SPAN, value, is_tail));
    }

    let template = ast.template_literal(SPAN, quasis, values);
    Some(MergedClass::Dynamic(Expression::TemplateLiteral(
        ast.alloc(template),
    )))
}

fn process_spreads<'a, 'b>(
    element: &'b JSXElement<'a>,
    elem_id: &str,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    ctx: &TraverseCtx<'a, ()>,
) -> (Vec<&'b JSXAttributeItem<'a>>, Option<Expression<'a>>) {
    let ast = context.ast();
    let mut filtered_attributes: Vec<&JSXAttributeItem<'a>> = Vec::new();
    let mut spread_args: Vec<Expression<'a>> = Vec::new();
    let mut running_props: Vec<ObjectPropertyKind<'a>> = Vec::new();
    let mut dynamic_spread = false;

    for attr_item in &element.opening_element.attributes {
        match attr_item {
            JSXAttributeItem::SpreadAttribute(spread) => {
                if !running_props.is_empty() {
                    let mut props = ast.vec_with_capacity(running_props.len());
                    for prop in running_props.drain(..) {
                        props.push(prop);
                    }
                    spread_args.push(ast.expression_object(SPAN, props));
                }

                let has_static_marker =
                    context.has_static_marker_comment(spread.span, options.static_marker);
                let mut spread_expr = if has_static_marker {
                    context.clone_expr_without_trivia(&spread.argument)
                } else {
                    context.clone_expr(&spread.argument)
                };

                if is_dynamic(&spread.argument) {
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
                            arrow_zero_params_return_value(
                                ast,
                                spread.span,
                                context.clone_expr(&spread.argument),
                            )
                        }
                    } else {
                        arrow_zero_params_return_value(
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
                        context.has_static_marker_comment_anywhere(
                            container.span,
                            options.static_marker,
                        )
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

                if can_native_spread(&key, true) {
                    if dynamic {
                        let (prop_key, key_is_identifier) =
                            make_getter_prop_key(ast, attr.span, &key);
                        let expr = attr
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
                                    } else if let Some(static_value) =
                                        evaluate_static_text_expression(expr, context, ctx)
                                    {
                                        ast.expression_string_literal(
                                            attr.span,
                                            ast.allocator.alloc_str(&static_value.as_text()),
                                            None,
                                        )
                                    } else {
                                        context.clone_expr(expr)
                                    }
                                })
                                .unwrap_or_else(|| ast.expression_identifier(SPAN, "undefined")),
                            None => ast.expression_boolean_literal(SPAN, true),
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

    if !running_props.is_empty() {
        let mut props = ast.vec_with_capacity(running_props.len());
        for prop in running_props.drain(..) {
            props.push(prop);
        }
        spread_args.push(ast.expression_object(SPAN, props));
    }

    if spread_args.is_empty() {
        return (filtered_attributes, None);
    }

    let props = if spread_args.len() == 1 && !dynamic_spread {
        spread_args.pop().unwrap()
    } else {
        context.register_helper("mergeProps");
        let callee = ident_expr(ast, SPAN, "mergeProps");
        let mut args = ast.vec_with_capacity(spread_args.len());
        for arg in spread_args {
            args.push(Argument::from(arg));
        }
        ast.expression_call(
            SPAN,
            callee,
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            args,
            false,
        )
    };

    let callee = dom_helper_expr(context, ast, SPAN, "spread");
    let elem = ident_expr(ast, SPAN, elem_id);
    let has_children = ast.expression_boolean_literal(SPAN, !element.children.is_empty());
    let spread_expr = call_expr(ast, SPAN, callee, [elem, props, has_children]);

    (filtered_attributes, Some(spread_expr))
}

struct AttributeTransformResult<'a> {
    needs_text_content_placeholder: bool,
    synthetic_children: Option<(Expression<'a>, bool)>,
}

/// Transform element attributes
fn transform_attributes<'a>(
    element: &JSXElement<'a>,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    ctx: &TraverseCtx<'a, ()>,
) -> AttributeTransformResult<'a> {
    let ast = context.ast();
    let elem_id = result.id.clone();
    let mut class_attrs = Vec::new();
    let mut class_name_attrs = Vec::new();
    let mut other_attrs = Vec::new();
    let mut on_namespace_attrs: Vec<&JSXAttribute<'a>> = Vec::new();
    let mut deferred_style_attrs: Vec<&JSXAttribute<'a>> = Vec::new();
    let mut spread_expr = None;
    let mut synthetic_children: Option<(Expression<'a>, bool)> = None;
    let has_children = !element.children.is_empty();
    let mut needs_text_content_placeholder = false;
    let mut needs_spacing = true;
    let mut first_class_attr_index = None;
    let mut first_non_class_attr_index = None;

    let mut attributes: Vec<&JSXAttributeItem<'a>> =
        element.opening_element.attributes.iter().collect();

    if attributes
        .iter()
        .any(|attr| matches!(attr, JSXAttributeItem::SpreadAttribute(_)))
    {
        let elem_id = elem_id
            .as_deref()
            .expect("Spread attributes require an element id");
        let (filtered, spread) = process_spreads(element, elem_id, context, options, ctx);
        attributes = filtered;
        spread_expr = spread;
        if spread_expr.is_some() && options.hydratable {
            result.has_hydratable_event = true;
        }
    }

    let dynamics_start = result.dynamics.len();

    for (attr_index, attr_item) in attributes.into_iter().enumerate() {
        match attr_item {
            JSXAttributeItem::Attribute(attr) => {
                let key = get_attr_name(&attr.name);
                if key == "class" {
                    if first_class_attr_index.is_none() {
                        first_class_attr_index = Some(attr_index);
                    }
                    class_attrs.push(attr.as_ref());
                } else if key == "className" {
                    class_name_attrs.push(attr.as_ref());
                } else {
                    if first_non_class_attr_index.is_none() {
                        first_non_class_attr_index = Some(attr_index);
                    }
                    other_attrs.push(attr_item);
                }
            }
            JSXAttributeItem::SpreadAttribute(_) => {
                if first_non_class_attr_index.is_none() {
                    first_non_class_attr_index = Some(attr_index);
                }
                other_attrs.push(attr_item);
            }
        }
    }

    let class_precedes_non_class = matches!(
        (first_class_attr_index, first_non_class_attr_index),
        (Some(class_index), Some(non_class_index)) if class_index < non_class_index
    );

    if class_precedes_non_class && !result.is_svg {
        match class_attrs.len() {
            0 => {}
            1 => transform_attribute(
                class_attrs[0],
                elem_id.as_deref(),
                result,
                context,
                options,
                ctx,
                has_children,
                &mut needs_text_content_placeholder,
                &mut needs_spacing,
            ),
            _ => {
                let Some(merged) = build_merged_class_value(ast, context, &class_attrs) else {
                    return AttributeTransformResult {
                        needs_text_content_placeholder,
                        synthetic_children,
                    };
                };
                match merged {
                    MergedClass::Static(value) => {
                        if !value.is_empty() {
                            inline_attribute_on_template(
                                result,
                                result.is_svg,
                                "class",
                                Some(value.as_str()),
                                options.omit_quotes,
                                &mut needs_spacing,
                            );
                        }
                    }
                    MergedClass::Dynamic(expr) => {
                        let elem_id = elem_id
                            .as_deref()
                            .expect("dynamic class requires an element id");
                        result.dynamics.push(DynamicBinding {
                            elem: elem_id.to_string(),
                            key: "class".to_string(),
                            value: expr,
                            is_svg: result.is_svg,
                            is_ce: result.has_custom_element,
                            tag_name: result.tag_name.clone().unwrap_or_default(),
                        });
                    }
                }
            }
        }
    }

    for attr_item in other_attrs {
        match attr_item {
            JSXAttributeItem::Attribute(attr) => {
                let key = get_attr_name(&attr.name);
                if key == "children" {
                    let mut non_primitive_children_expr = None;
                    if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
                        if let Some(expr) = container.expression.as_expression() {
                            if !matches!(
                                unwrap_ts_expression(expr),
                                Expression::StringLiteral(_)
                                    | Expression::NumericLiteral(_)
                                    | Expression::BooleanLiteral(_)
                            ) {
                                let has_static_marker = context.has_static_marker_comment(
                                    container.span,
                                    options.static_marker,
                                );
                                let value = if has_static_marker {
                                    context.clone_expr_without_trivia(expr)
                                } else {
                                    context.clone_expr(expr)
                                };
                                non_primitive_children_expr = Some((value, has_static_marker));
                            }
                        }
                    }

                    if let Some(children_expr) = non_primitive_children_expr {
                        if element.children.is_empty() {
                            synthetic_children = Some(children_expr);
                        }
                        continue;
                    }
                }

                // Babel emits `on:*` listener namespace bindings in reverse source order
                // because they are effectively prepended during attribute transform.
                if key.starts_with("on:") {
                    on_namespace_attrs.push(attr.as_ref());
                    continue;
                }

                if key == "style" && matches!(attr.value, Some(JSXAttributeValue::StringLiteral(_)))
                {
                    deferred_style_attrs.push(attr.as_ref());
                    continue;
                }

                transform_attribute(
                    attr,
                    elem_id.as_deref(),
                    result,
                    context,
                    options,
                    ctx,
                    has_children,
                    &mut needs_text_content_placeholder,
                    &mut needs_spacing,
                );
            }
            JSXAttributeItem::SpreadAttribute(_) => {}
        }
    }

    for attr in on_namespace_attrs.into_iter().rev() {
        transform_attribute(
            attr,
            elem_id.as_deref(),
            result,
            context,
            options,
            ctx,
            has_children,
            &mut needs_text_content_placeholder,
            &mut needs_spacing,
        );
    }

    for attr in deferred_style_attrs {
        transform_attribute(
            attr,
            elem_id.as_deref(),
            result,
            context,
            options,
            ctx,
            has_children,
            &mut needs_text_content_placeholder,
            &mut needs_spacing,
        );
    }

    if !class_precedes_non_class || result.is_svg {
        match class_attrs.len() {
            0 => {}
            1 => transform_attribute(
                class_attrs[0],
                elem_id.as_deref(),
                result,
                context,
                options,
                ctx,
                has_children,
                &mut needs_text_content_placeholder,
                &mut needs_spacing,
            ),
            _ => {
                let Some(merged) = build_merged_class_value(ast, context, &class_attrs) else {
                    return AttributeTransformResult {
                        needs_text_content_placeholder,
                        synthetic_children,
                    };
                };
                match merged {
                    MergedClass::Static(value) => {
                        if !value.is_empty() {
                            inline_attribute_on_template(
                                result,
                                result.is_svg,
                                "class",
                                Some(value.as_str()),
                                options.omit_quotes,
                                &mut needs_spacing,
                            );
                        }
                    }
                    MergedClass::Dynamic(expr) => {
                        let elem_id = elem_id
                            .as_deref()
                            .expect("dynamic class requires an element id");
                        result.dynamics.push(DynamicBinding {
                            elem: elem_id.to_string(),
                            key: "class".to_string(),
                            value: expr,
                            is_svg: result.is_svg,
                            is_ce: result.has_custom_element,
                            tag_name: result.tag_name.clone().unwrap_or_default(),
                        });
                    }
                }
            }
        }
    }

    for class_name_attr in class_name_attrs {
        transform_attribute(
            class_name_attr,
            elem_id.as_deref(),
            result,
            context,
            options,
            ctx,
            has_children,
            &mut needs_text_content_placeholder,
            &mut needs_spacing,
        );
    }

    if result.is_svg && class_precedes_non_class && dynamics_start < result.dynamics.len() {
        let mut class_dynamics = Vec::new();
        let mut other_dynamics = Vec::new();

        for binding in result.dynamics.drain(dynamics_start..) {
            if binding.key == "class" || binding.key == "className" {
                class_dynamics.push(binding);
            } else {
                other_dynamics.push(binding);
            }
        }

        result.dynamics.extend(class_dynamics);
        result.dynamics.extend(other_dynamics);
    }

    if let Some(spread_expr) = spread_expr {
        result.exprs.push(spread_expr);
    }

    AttributeTransformResult {
        needs_text_content_placeholder,
        synthetic_children,
    }
}

fn emit_runtime_attribute_setter<'a>(
    span: Span,
    key: &str,
    value: Expression<'a>,
    elem_id: Option<&str>,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
) {
    let ast = context.ast();
    let elem_id = elem_id.expect("child property attributes require an element id");
    let binding = DynamicBinding {
        elem: elem_id.to_string(),
        key: key.to_string(),
        value: value.clone_in(ast.allocator),
        is_svg: result.is_svg,
        is_ce: result.has_custom_element,
        tag_name: result.tag_name.clone().unwrap_or_default(),
    };

    register_dynamic_binding_helper(context, &binding);
    result
        .exprs
        .push(crate::template::generate_set_attr_expr_with_value(
            ast,
            span,
            &binding,
            value,
            None,
            options.hydratable,
        ));
}

/// Transform a single attribute
fn transform_attribute<'a>(
    attr: &JSXAttribute<'a>,
    elem_id: Option<&str>,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    ctx: &TraverseCtx<'a, ()>,
    has_children: bool,
    needs_text_content_placeholder: &mut bool,
    needs_spacing: &mut bool,
) {
    let key = get_attr_name(&attr.name);
    let ast = context.ast();
    let is_child_property = CHILD_PROPERTIES.contains(&*key);

    // Hydratable DOM parity: `$ServerOnly` marks this element as hydration-only and
    // must not serialize as a normal template attribute.
    if options.hydratable && key == "$ServerOnly" {
        result.skip_template = true;
        return;
    }

    if result.is_svg && key == "xmlns" {
        return;
    }

    // Handle different attribute types
    if key == "ref" {
        let elem_id = elem_id.expect("ref requires an element id");
        transform_ref(attr, elem_id, result, context, ctx);
        return;
    }

    if (key.starts_with("on:") || (key.starts_with("on") && !key.contains(':')))
        && !matches!(attr.value, Some(JSXAttributeValue::StringLiteral(_)))
    {
        let elem_id = elem_id.expect("event handlers require an element id");
        transform_event(attr, &key, elem_id, result, context, options, ctx);
        return;
    }

    if key.starts_with("use:") {
        let elem_id = elem_id.expect("directives require an element id");
        transform_directive(attr, &key, elem_id, result, context);
        return;
    }

    // Handle prop: prefix - direct DOM property assignment
    if key.starts_with("prop:") {
        let elem_id = elem_id.expect("prop: requires an element id");
        transform_prop(attr, &key, elem_id, result, context);
        return;
    }

    // Handle bool: prefix - force boolean attribute mode
    if key.starts_with("bool:") {
        let elem_id = elem_id.expect("bool: requires an element id");
        transform_bool(
            attr,
            &key,
            elem_id,
            result,
            context,
            options,
            options.omit_quotes,
            needs_spacing,
        );
        return;
    }

    // Handle class: prefix - classList.toggle() behavior
    if key.starts_with("class:") {
        let elem_id = elem_id.expect("class: requires an element id");
        transform_class_namespace(attr, &key, elem_id, result, context);
        return;
    }

    if key == "classList" {
        if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
            if let Some(Expression::ObjectExpression(obj)) = container.expression.as_expression() {
                let elem_id = elem_id.expect("classList requires an element id");
                if transform_class_list_object(obj, elem_id, result, context) {
                    return;
                }
            }
        }
    }

    if key == "class" {
        if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
            if let Some(Expression::ObjectExpression(obj)) = container.expression.as_expression() {
                let elem_id = elem_id.expect("class requires an element id");
                if transform_class_object(
                    obj,
                    elem_id,
                    result,
                    context,
                    options.omit_quotes,
                    needs_spacing,
                ) {
                    return;
                }
            }
        }
    }

    // Handle style: prefix - setStyleProperty() behavior
    if key.starts_with("style:") {
        let elem_id = elem_id.expect("style: requires an element id");
        transform_style_namespace(attr, &key, elem_id, result, context);
        return;
    }

    // Handle style attribute specially
    if key == "style" {
        transform_style(
            attr,
            elem_id,
            result,
            context,
            options,
            options.omit_quotes,
            needs_spacing,
        );
        return;
    }

    if key == "textContent" {
        if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
            if let Some(expr) = container.expression.as_expression() {
                let has_static_marker = context
                    .has_static_marker_comment_anywhere(container.span, options.static_marker);
                if !has_children && !has_static_marker && is_dynamic(expr) {
                    let elem_id = elem_id.expect("textContent requires an element id");
                    let text_id = context.generate_uid("el$");
                    *needs_text_content_placeholder = true;
                    result.declarations.push(Declaration {
                        pattern: ast.binding_pattern_binding_identifier(
                            attr.span,
                            ast.allocator.alloc_str(&text_id),
                        ),
                        init: static_member(
                            ast,
                            attr.span,
                            ident_expr(ast, attr.span, elem_id),
                            "firstChild",
                        ),
                    });
                    result.dynamics.push(DynamicBinding {
                        elem: text_id,
                        key: key.to_string(),
                        value: context.clone_expr(expr),
                        is_svg: result.is_svg,
                        is_ce: result.has_custom_element,
                        tag_name: result.tag_name.clone().unwrap_or_default(),
                    });
                    return;
                }
            }
        }
    }

    // Handle innerHTML/textContent/innerText
    if key == "innerHTML" || key == "textContent" || key == "innerText" {
        let elem_id = elem_id.expect("inner content requires an element id");
        transform_inner_content(attr, &key, elem_id, result, context, options);
        return;
    }

    // Regular attribute
    match &attr.value {
        Some(JSXAttributeValue::StringLiteral(lit)) => {
            // JSX attribute string literals should decode HTML entities first
            // (e.g. "Search&hellip;" -> "Search…").
            let decoded = common::expression::decode_html_entities(lit.value.as_str());

            if is_child_property {
                let value = ast.expression_string_literal(
                    attr.span,
                    ast.allocator.alloc_str(decoded.as_str()),
                    None,
                );
                emit_runtime_attribute_setter(
                    attr.span, &*key, value, elem_id, result, context, options,
                );
                return;
            }

            // Static string attribute - inline in template.
            let attr_key = ALIASES.get(&*key).copied().unwrap_or(&*key);
            inline_attribute_on_template(
                result,
                result.is_svg,
                attr_key,
                Some(decoded.as_str()),
                options.omit_quotes,
                needs_spacing,
            );
        }
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            if let Some(expr) = container.expression.as_expression() {
                let has_static_marker = context
                    .has_static_marker_comment_anywhere(container.span, options.static_marker);
                let mut dynamic_expr = !has_static_marker && is_dynamic(expr);
                if !has_static_marker
                    && (key == "class" || key == "style")
                    && !dynamic_expr
                    && !is_confident_static_expression(expr)
                {
                    dynamic_expr = true;
                }
                let expr_value = if has_static_marker {
                    context.clone_expr_without_trivia(expr)
                } else {
                    context.clone_expr(expr)
                };

                let attr_key = ALIASES.get(&*key).copied().unwrap_or(&*key);

                if !is_child_property {
                    if let Some(static_value) = evaluate_static_text_expression(expr, context, ctx)
                    {
                        let static_text = static_value.as_text();
                        inline_attribute_on_template(
                            result,
                            result.is_svg,
                            attr_key,
                            Some(static_text.as_str()),
                            options.omit_quotes,
                            needs_spacing,
                        );
                        return;
                    }

                    match unwrap_ts_expression(expr) {
                        Expression::StringLiteral(lit) => {
                            inline_attribute_on_template(
                                result,
                                result.is_svg,
                                attr_key,
                                Some(lit.value.as_str()),
                                options.omit_quotes,
                                needs_spacing,
                            );
                            return;
                        }
                        Expression::NumericLiteral(num) => {
                            let num_text = num.value.to_string();
                            inline_attribute_on_template(
                                result,
                                result.is_svg,
                                attr_key,
                                Some(num_text.as_str()),
                                options.omit_quotes,
                                needs_spacing,
                            );
                            return;
                        }
                        Expression::BooleanLiteral(lit) => {
                            if lit.value {
                                inline_attribute_on_template(
                                    result,
                                    result.is_svg,
                                    attr_key,
                                    None,
                                    options.omit_quotes,
                                    needs_spacing,
                                );
                            }
                            return;
                        }
                        _ => {}
                    }
                }

                if (key == "value" || key == "checked")
                    && dynamic_expr
                    && effect_wrapper_enabled(options)
                {
                    let elem_id = elem_id.expect("value/checked require an element id");
                    let binding = DynamicBinding {
                        elem: elem_id.to_string(),
                        key: key.to_string(),
                        value: expr_value.clone_in(ast.allocator),
                        is_svg: result.is_svg,
                        is_ce: result.has_custom_element,
                        tag_name: result.tag_name.clone().unwrap_or_default(),
                    };

                    let source = if let Expression::CallExpression(call) = expr {
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
                            arrow_zero_params_return_expr(ast, attr.span, context.clone_expr(expr))
                        }
                    } else {
                        arrow_zero_params_return_expr(ast, attr.span, context.clone_expr(expr))
                    };

                    context.register_helper("effect");
                    register_dynamic_binding_helper(context, &binding);
                    let setter = crate::template::generate_set_attr_expr_with_value(
                        ast,
                        attr.span,
                        &binding,
                        ident_expr(ast, attr.span, "_v$"),
                        None,
                        options.hydratable,
                    );
                    let callback = arrow_single_param_statement_expr(ast, attr.span, "_v$", setter);
                    let effect = ident_expr(ast, attr.span, "effect");
                    result
                        .post_exprs
                        .push(call_expr(ast, attr.span, effect, [source, callback]));
                    return;
                }

                if dynamic_expr {
                    let elem_id = elem_id.expect("dynamic attributes require an element id");
                    result.dynamics.push(DynamicBinding {
                        elem: elem_id.to_string(),
                        key: key.to_string(),
                        value: expr_value,
                        is_svg: result.is_svg,
                        is_ce: result.has_custom_element,
                        tag_name: result.tag_name.clone().unwrap_or_default(),
                    });
                } else {
                    let elem_id = elem_id.expect("expression attributes require an element id");
                    let binding = DynamicBinding {
                        elem: elem_id.to_string(),
                        key: key.to_string(),
                        value: expr_value,
                        is_svg: result.is_svg,
                        is_ce: result.has_custom_element,
                        tag_name: result.tag_name.clone().unwrap_or_default(),
                    };

                    register_dynamic_binding_helper(context, &binding);
                    result
                        .exprs
                        .push(crate::template::generate_set_attr_expr_with_value(
                            ast,
                            attr.span,
                            &binding,
                            binding.value.clone_in(ast.allocator),
                            None,
                            options.hydratable,
                        ));
                }
            }
        }
        None => {
            if is_child_property {
                emit_runtime_attribute_setter(
                    attr.span,
                    &*key,
                    ast.expression_boolean_literal(attr.span, true),
                    elem_id,
                    result,
                    context,
                    options,
                );
                return;
            }

            // Boolean attribute (e.g., disabled)
            inline_attribute_on_template(
                result,
                result.is_svg,
                &key,
                None,
                options.omit_quotes,
                needs_spacing,
            );
        }
        _ => {}
    }
}

/// Transform ref attribute
fn transform_ref<'a>(
    attr: &JSXAttribute<'a>,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    ctx: &TraverseCtx<'a, ()>,
) {
    let ast = context.ast();
    if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
        if let Some(expr) = container.expression.as_expression() {
            let elem = ident_expr(ast, attr.span, elem_id);

            if matches!(
                expr,
                Expression::ArrowFunctionExpression(_)
                    | Expression::FunctionExpression(_)
                    | Expression::ArrayExpression(_)
            ) || !is_writable_ref_target(expr, ctx)
            {
                let ref_callee = dom_helper_expr(context, ast, attr.span, "ref");
                result.exprs.push(call_expr(
                    ast,
                    attr.span,
                    ref_callee,
                    [
                        arrow_zero_params_return_expr(ast, attr.span, context.clone_expr(expr)),
                        elem,
                    ],
                ));
                return;
            }

            let ref_uid = context.generate_uid("ref$");
            let ref_ident = ident_expr(ast, attr.span, &ref_uid);

            let var_decl = {
                let declarator = ast.variable_declarator(
                    attr.span,
                    VariableDeclarationKind::Var,
                    ast.binding_pattern_binding_identifier(
                        attr.span,
                        ast.allocator.alloc_str(&ref_uid),
                    ),
                    NONE,
                    Some(context.clone_expr(expr)),
                    false,
                );
                Statement::VariableDeclaration(ast.alloc_variable_declaration(
                    attr.span,
                    VariableDeclarationKind::Var,
                    ast.vec1(declarator),
                    false,
                ))
            };
            result.statements.push(var_decl);

            let typeof_ref = ast.expression_unary(
                SPAN,
                UnaryOperator::Typeof,
                ref_ident.clone_in(ast.allocator),
            );
            let function_str =
                ast.expression_string_literal(SPAN, ast.allocator.alloc_str("function"), None);
            let test = ast.expression_binary(
                SPAN,
                typeof_ref,
                BinaryOperator::StrictEquality,
                function_str,
            );
            let array_is_array = static_member(
                ast,
                attr.span,
                ident_expr(ast, attr.span, "Array"),
                "isArray",
            );
            let array_test = call_expr(
                ast,
                attr.span,
                array_is_array,
                [ref_ident.clone_in(ast.allocator)],
            );
            let callable_test = ast.expression_logical(SPAN, test, LogicalOperator::Or, array_test);

            let ref_callee = dom_helper_expr(context, ast, attr.span, "ref");
            let ref_call = call_expr(
                ast,
                attr.span,
                ref_callee,
                [
                    arrow_zero_params_return_expr(
                        ast,
                        attr.span,
                        ref_ident.clone_in(ast.allocator),
                    ),
                    elem.clone_in(ast.allocator),
                ],
            );

            if let Some(target) = expression_to_assignment_target(context.clone_expr(expr)) {
                let assign =
                    ast.expression_assignment(SPAN, AssignmentOperator::Assign, target, elem);
                result.exprs.push(ast.expression_conditional(
                    SPAN,
                    callable_test,
                    ref_call,
                    assign,
                ));
            } else {
                result.exprs.push(ast.expression_logical(
                    SPAN,
                    callable_test,
                    LogicalOperator::And,
                    ref_call,
                ));
            }
        }
    }
}

pub(crate) fn is_writable_ref_target<'a>(expr: &Expression<'a>, ctx: &TraverseCtx<'a, ()>) -> bool {
    let Some(ident) = peel_identifier_reference(expr) else {
        return true;
    };

    let Some(reference_id) = ident.reference_id.get() else {
        return true;
    };

    let reference = ctx.scoping.scoping().get_reference(reference_id);
    let Some(symbol_id) = reference.symbol_id() else {
        return true;
    };

    let flags = ctx.scoping.scoping().symbol_flags(symbol_id);
    !(flags.is_const_variable()
        || flags.contains(SymbolFlags::Import)
        || flags.contains(SymbolFlags::TypeImport))
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

fn peel_event_handler_expression<'a, 'b>(expr: &'b Expression<'a>) -> &'b Expression<'a> {
    match expr {
        Expression::ParenthesizedExpression(e) => peel_event_handler_expression(&e.expression),
        Expression::TSAsExpression(e) => peel_event_handler_expression(&e.expression),
        Expression::TSSatisfiesExpression(e) => peel_event_handler_expression(&e.expression),
        Expression::TSNonNullExpression(e) => peel_event_handler_expression(&e.expression),
        Expression::TSTypeAssertion(e) => peel_event_handler_expression(&e.expression),
        _ => expr,
    }
}

fn is_function_handler_expression<'a>(expr: &Expression<'a>) -> bool {
    matches!(
        peel_event_handler_expression(expr),
        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
    )
}

fn is_resolvable_event_handler<'a>(expr: &Expression<'a>, ctx: &TraverseCtx<'a, ()>) -> bool {
    let Expression::Identifier(ident) = peel_event_handler_expression(expr) else {
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
    flags.is_function()
        || flags.is_const_variable()
        || flags.contains(SymbolFlags::Import)
        || flags.contains(SymbolFlags::TypeImport)
}

/// Transform event handler
fn transform_event<'a>(
    attr: &JSXAttribute<'a>,
    key: &str,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    ctx: &TraverseCtx<'a, ()>,
) {
    let ast = context.ast();
    let is_capture_namespace = key.starts_with("oncapture:");
    let is_listener_namespace = key.starts_with("on:");
    // Check for capture mode (onClickCapture -> click with capture=true)
    let is_capture_suffix = key.ends_with("Capture");
    let base_key = if is_capture_suffix {
        &key[..key.len() - 7] // Remove "Capture" suffix
    } else {
        key
    };
    let is_capture = is_capture_namespace || is_capture_suffix;

    let event_name = if is_capture_namespace {
        std::borrow::Cow::Borrowed(key.split_once(':').map(|(_, ev)| ev).unwrap_or(""))
    } else {
        to_event_name(base_key)
    };

    // Get the handler expression
    let handler_expr = attr.value.as_ref().and_then(|v| match v {
        JSXAttributeValue::ExpressionContainer(container) => container.expression.as_expression(),
        _ => None,
    });

    let mut handler = handler_expr
        .map(|e| context.clone_expr(e))
        .unwrap_or_else(|| ast.expression_identifier(SPAN, "undefined"));

    let mut is_array_handler = false;
    let mut handler_data: Option<Expression<'a>> = None;

    if let Some(Expression::ArrayExpression(arr)) = handler_expr {
        is_array_handler = true;
        let first = arr
            .elements
            .first()
            .and_then(|element| element.as_expression().map(|expr| context.clone_expr(expr)));
        let second = arr
            .elements
            .get(1)
            .and_then(|element| element.as_expression().map(|expr| context.clone_expr(expr)));
        handler = first.unwrap_or_else(|| ast.expression_identifier(SPAN, "undefined"));
        handler_data = second;
    }

    let handler_is_function =
        !is_array_handler && handler_expr.is_some_and(|expr| is_function_handler_expression(expr));
    let handler_is_resolvable = !is_array_handler
        && handler_expr.is_some_and(|expr| is_resolvable_event_handler(expr, ctx));

    // on: / oncapture: force non-delegation
    let force_no_delegate = is_listener_namespace || is_capture_namespace;

    // Capture events cannot be delegated
    // Check if this event should be delegated
    let should_delegate = !force_no_delegate
        && !is_capture
        && options.delegate_events
        && (DELEGATED_EVENTS.contains(&*event_name)
            || options.delegated_events.contains(&&*event_name));

    let elem = ident_expr(ast, attr.span, elem_id);
    let event = ast.expression_string_literal(SPAN, ast.allocator.alloc_str(&event_name), None);

    if should_delegate {
        context.register_delegate(&event_name);
        if options.hydratable {
            result.has_hydratable_event = true;
        }

        if is_array_handler || handler_is_function || handler_is_resolvable {
            let prop = format!("$${}", event_name);
            let member = static_member(ast, attr.span, elem.clone_in(ast.allocator), &prop);
            let Some(target) = expression_to_assignment_target(member) else {
                return;
            };
            result.exprs.push(ast.expression_assignment(
                SPAN,
                AssignmentOperator::Assign,
                target,
                handler,
            ));

            if let Some(data) = handler_data {
                let data_prop = format!("$${}Data", event_name);
                let data_member = static_member(ast, attr.span, elem, &data_prop);
                if let Some(target) = expression_to_assignment_target(data_member) {
                    result.exprs.push(ast.expression_assignment(
                        SPAN,
                        AssignmentOperator::Assign,
                        target,
                        data,
                    ));
                }
            }
            return;
        }

        let callee = dom_helper_expr(context, ast, attr.span, "addEvent");
        let delegate = ast.expression_boolean_literal(SPAN, true);
        result.exprs.push(call_expr(
            ast,
            attr.span,
            callee,
            [elem, event, handler, delegate],
        ));
        return;
    }

    let listener_handler = if let Some(data) = handler_data {
        let e_ident = ident_expr(ast, attr.span, "e");
        let call = call_expr(
            ast,
            attr.span,
            handler.clone_in(ast.allocator),
            [data, e_ident.clone_in(ast.allocator)],
        );
        arrow_single_param_return_expr(ast, attr.span, "e", call)
    } else {
        handler
    };

    if is_listener_namespace {
        context.register_helper("addEventListener");
        let callee = ident_expr(ast, attr.span, "addEventListener");
        result.exprs.push(call_expr(
            ast,
            attr.span,
            callee,
            [elem, event, listener_handler],
        ));
        return;
    }

    if is_capture || is_array_handler || handler_is_function || handler_is_resolvable {
        let callee = static_member(
            ast,
            attr.span,
            elem.clone_in(ast.allocator),
            "addEventListener",
        );
        if is_capture {
            let capture = ast.expression_boolean_literal(SPAN, true);
            result.exprs.push(call_expr(
                ast,
                attr.span,
                callee,
                [event, listener_handler, capture],
            ));
        } else {
            result
                .exprs
                .push(call_expr(ast, attr.span, callee, [event, listener_handler]));
        }
        return;
    }

    let callee = dom_helper_expr(context, ast, attr.span, "addEvent");
    result.exprs.push(call_expr(
        ast,
        attr.span,
        callee,
        [elem, event, listener_handler],
    ));
}

/// Transform use: directive
fn transform_directive<'a>(
    attr: &JSXAttribute<'a>,
    key: &str,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
) {
    let ast = context.ast();
    let directive_name = &key[4..]; // Strip "use:"

    let value = attr
        .value
        .as_ref()
        .and_then(|v| match v {
            JSXAttributeValue::ExpressionContainer(container) => {
                container.expression.as_expression()
            }
            _ => None,
        })
        .map(|e| arrow_zero_params_return_expr(ast, attr.span, context.clone_expr(e)))
        .unwrap_or_else(|| {
            arrow_zero_params_return_expr(
                ast,
                attr.span,
                ast.expression_boolean_literal(SPAN, true),
            )
        });

    let callee = dom_helper_expr(context, ast, attr.span, "use");
    let expr = call_expr(
        ast,
        attr.span,
        callee,
        [
            ident_expr(ast, attr.span, directive_name),
            ident_expr(ast, attr.span, elem_id),
            value,
        ],
    );
    result.exprs.insert(0, expr);
}

/// Transform prop: prefix (direct DOM property assignment)
fn transform_prop<'a>(
    attr: &JSXAttribute<'a>,
    key: &str,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
) {
    let ast = context.ast();
    let prop_name = &key[5..]; // Strip "prop:"

    let mut push_assignment = |value: Expression<'a>| {
        let elem = ident_expr(ast, attr.span, elem_id);
        let member = static_member(ast, attr.span, elem, prop_name);
        let Some(target) = expression_to_assignment_target(member) else {
            return;
        };
        let assign = ast.expression_assignment(SPAN, AssignmentOperator::Assign, target, value);
        result.exprs.push(assign);
    };

    match &attr.value {
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            if let Some(expr) = container.expression.as_expression() {
                if is_dynamic(expr) {
                    result.dynamics.push(DynamicBinding {
                        elem: elem_id.to_string(),
                        key: key.to_string(),
                        value: context.clone_expr(expr),
                        is_svg: result.is_svg,
                        is_ce: result.has_custom_element,
                        tag_name: result.tag_name.clone().unwrap_or_default(),
                    });
                    return;
                }

                push_assignment(context.clone_expr(expr));
            }
        }
        Some(JSXAttributeValue::StringLiteral(lit)) => {
            let value =
                ast.expression_string_literal(attr.span, ast.allocator.alloc_str(&lit.value), None);
            push_assignment(value);
        }
        None => {
            push_assignment(ast.expression_boolean_literal(attr.span, true));
        }
        _ => {}
    }
}

fn transform_bool<'a>(
    attr: &JSXAttribute<'a>,
    key: &str,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    omit_quotes: bool,
    needs_spacing: &mut bool,
) {
    let ast = context.ast();
    let bool_name = &key[5..];

    match &attr.value {
        None => {
            inline_attribute_on_template(
                result,
                result.is_svg,
                bool_name,
                None,
                omit_quotes,
                needs_spacing,
            );
        }
        Some(JSXAttributeValue::StringLiteral(lit)) => {
            if !lit.value.is_empty() && lit.value != "0" {
                inline_attribute_on_template(
                    result,
                    result.is_svg,
                    bool_name,
                    None,
                    omit_quotes,
                    needs_spacing,
                );
            }
        }
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            if let Some(expr) = container.expression.as_expression() {
                match expr {
                    Expression::StringLiteral(lit) => {
                        if !lit.value.is_empty() && lit.value != "0" {
                            inline_attribute_on_template(
                                result,
                                result.is_svg,
                                bool_name,
                                None,
                                omit_quotes,
                                needs_spacing,
                            );
                        }
                        return;
                    }
                    Expression::NumericLiteral(num) => {
                        if num.value != 0.0 {
                            inline_attribute_on_template(
                                result,
                                result.is_svg,
                                bool_name,
                                None,
                                omit_quotes,
                                needs_spacing,
                            );
                        }
                        return;
                    }
                    Expression::BooleanLiteral(lit) => {
                        if lit.value {
                            inline_attribute_on_template(
                                result,
                                result.is_svg,
                                bool_name,
                                None,
                                omit_quotes,
                                needs_spacing,
                            );
                        }
                        return;
                    }
                    Expression::NullLiteral(_) => return,
                    Expression::Identifier(ident) if ident.name == "undefined" => return,
                    _ => {}
                }

                context.register_helper("setBoolAttribute");
                let elem = ident_expr(ast, attr.span, elem_id);
                let name =
                    ast.expression_string_literal(SPAN, ast.allocator.alloc_str(bool_name), None);
                let call = call_expr(
                    ast,
                    attr.span,
                    ident_expr(ast, attr.span, "setBoolAttribute"),
                    [elem, name, context.clone_expr(expr)],
                );
                if is_dynamic(expr) {
                    if effect_wrapper_enabled(options) {
                        context.register_helper("effect");
                        let effect = ident_expr(ast, attr.span, "effect");
                        let arrow = arrow_zero_params_return_expr(
                            ast,
                            attr.span,
                            call.clone_in(ast.allocator),
                        );
                        result
                            .exprs
                            .push(call_expr(ast, attr.span, effect, [arrow]));
                    } else {
                        result.exprs.push(call);
                    }
                } else {
                    result.exprs.push(call);
                }
            }
        }
        _ => {}
    }
}

/// Transform class: prefix (maps to classList.toggle)
fn transform_class_namespace<'a>(
    attr: &JSXAttribute<'a>,
    key: &str,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
) {
    let ast = context.ast();
    let class_name = &key[6..]; // Strip "class:"

    match &attr.value {
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            if let Some(expr) = container.expression.as_expression() {
                if is_dynamic(expr) {
                    result.dynamics.push(DynamicBinding {
                        elem: elem_id.to_string(),
                        key: key.to_string(),
                        value: context.clone_expr(expr),
                        is_svg: result.is_svg,
                        is_ce: result.has_custom_element,
                        tag_name: result.tag_name.clone().unwrap_or_default(),
                    });
                } else {
                    let toggle_expr = class_toggle_expr(
                        ast,
                        attr.span,
                        elem_id,
                        class_name,
                        bool_cast_expr(ast, attr.span, context.clone_expr(expr)),
                    );
                    result.exprs.push(toggle_expr);
                }
            }
        }
        Some(JSXAttributeValue::StringLiteral(lit)) => {
            let truthy = ast.expression_boolean_literal(SPAN, !lit.value.is_empty());
            result.exprs.push(class_toggle_expr(
                ast, attr.span, elem_id, class_name, truthy,
            ));
        }
        None => {
            let truthy = ast.expression_boolean_literal(SPAN, true);
            result.exprs.push(class_toggle_expr(
                ast, attr.span, elem_id, class_name, truthy,
            ));
        }
        _ => {}
    }
}

fn transform_class_object<'a>(
    obj: &oxc_ast::ast::ObjectExpression<'a>,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    omit_quotes: bool,
    needs_spacing: &mut bool,
) -> bool {
    let ast = context.ast();
    let mut static_classes = Vec::new();

    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(prop) = prop else {
            return false;
        };

        if prop.computed {
            return false;
        }

        let class_name = match &prop.key {
            PropertyKey::StaticIdentifier(id) => id.name.as_str(),
            PropertyKey::StringLiteral(lit) => lit.value.as_str(),
            _ => return false,
        };

        if class_name.contains(' ') || class_name.contains(':') {
            return false;
        }

        match &prop.value {
            Expression::BooleanLiteral(lit) => {
                if lit.value {
                    static_classes.push(class_name.to_string());
                }
            }
            Expression::StringLiteral(lit) => {
                if !lit.value.is_empty() {
                    static_classes.push(class_name.to_string());
                }
            }
            Expression::NumericLiteral(num) => {
                if num.value != 0.0 {
                    static_classes.push(class_name.to_string());
                }
            }
            Expression::NullLiteral(_) => {}
            Expression::Identifier(ident) if ident.name == "undefined" => {}
            other => {
                if is_dynamic(other) {
                    result.dynamics.push(DynamicBinding {
                        elem: elem_id.to_string(),
                        key: format!("class:{}", class_name),
                        value: context.clone_expr(other),
                        is_svg: result.is_svg,
                        is_ce: result.has_custom_element,
                        tag_name: result.tag_name.clone().unwrap_or_default(),
                    });
                } else {
                    let toggle_expr = class_toggle_expr(
                        ast,
                        prop.span,
                        elem_id,
                        class_name,
                        bool_cast_expr(ast, prop.span, context.clone_expr(other)),
                    );
                    result.exprs.push(toggle_expr);
                }
            }
        }
    }

    if !static_classes.is_empty() {
        let class_value = static_classes.join(" ");
        inline_attribute_on_template(
            result,
            result.is_svg,
            "class",
            Some(class_value.as_str()),
            omit_quotes,
            needs_spacing,
        );
    }

    true
}

fn transform_class_list_object<'a>(
    obj: &oxc_ast::ast::ObjectExpression<'a>,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
) -> bool {
    let ast = context.ast();
    for prop in &obj.properties {
        let oxc_ast::ast::ObjectPropertyKind::ObjectProperty(prop) = prop else {
            return false;
        };

        if prop.computed {
            return false;
        }

        let class_name = match &prop.key {
            PropertyKey::StaticIdentifier(id) => id.name.as_str(),
            PropertyKey::StringLiteral(lit) => lit.value.as_str(),
            _ => return false,
        };

        if class_name.contains(' ') || class_name.contains(':') {
            return false;
        }

        let value_expr = &prop.value;
        let (expr, dynamic) = match value_expr {
            Expression::BooleanLiteral(lit) => {
                if !lit.value {
                    continue;
                }
                (ast.expression_boolean_literal(SPAN, true), false)
            }
            Expression::StringLiteral(lit) => {
                if lit.value.is_empty() {
                    continue;
                }
                (ast.expression_boolean_literal(SPAN, true), false)
            }
            Expression::NumericLiteral(num) => {
                if num.value == 0.0 {
                    continue;
                }
                (ast.expression_boolean_literal(SPAN, true), false)
            }
            Expression::NullLiteral(_) => continue,
            Expression::Identifier(ident) if ident.name == "undefined" => continue,
            _ => {
                let expr = bool_cast_expr(ast, prop.span, context.clone_expr(value_expr));
                (expr, is_dynamic(value_expr))
            }
        };

        let toggle_expr = class_toggle_expr(ast, prop.span, elem_id, class_name, expr);
        if dynamic {
            if context.effect_wrapper_enabled {
                context.register_helper("effect");
                let effect = ident_expr(ast, prop.span, "effect");
                let arrow = arrow_zero_params_return_expr(
                    ast,
                    prop.span,
                    toggle_expr.clone_in(ast.allocator),
                );
                result
                    .exprs
                    .push(call_expr(ast, prop.span, effect, [arrow]));
            } else {
                result.exprs.push(toggle_expr);
            }
        } else {
            result.exprs.push(toggle_expr);
        }
    }
    true
}

/// Transform style: prefix (maps to setStyleProperty)
fn transform_style_namespace<'a>(
    attr: &JSXAttribute<'a>,
    key: &str,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
) {
    let ast = context.ast();
    let prop_name = &key[6..]; // Strip "style:"
    context.register_helper("setStyleProperty");

    match &attr.value {
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            if let Some(expr) = container.expression.as_expression() {
                if is_dynamic(expr) {
                    result.dynamics.push(DynamicBinding {
                        elem: elem_id.to_string(),
                        key: key.to_string(),
                        value: context.clone_expr(expr),
                        is_svg: result.is_svg,
                        is_ce: result.has_custom_element,
                        tag_name: result.tag_name.clone().unwrap_or_default(),
                    });
                } else {
                    let set_prop = set_style_property_expr(
                        ast,
                        attr.span,
                        elem_id,
                        prop_name,
                        context.clone_expr(expr),
                    );
                    result.exprs.push(set_prop);
                }
            }
        }
        Some(JSXAttributeValue::StringLiteral(lit)) => {
            let value =
                ast.expression_string_literal(SPAN, ast.allocator.alloc_str(&lit.value), None);
            result.exprs.push(set_style_property_expr(
                ast, attr.span, elem_id, prop_name, value,
            ));
        }
        _ => {}
    }
}

/// Transform style attribute
fn transform_style<'a>(
    attr: &JSXAttribute<'a>,
    elem_id: Option<&str>,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    omit_quotes: bool,
    needs_spacing: &mut bool,
) {
    let ast = context.ast();

    if !options.inline_styles {
        let elem_id = elem_id.expect("style helper requires an element id");
        match &attr.value {
            Some(JSXAttributeValue::StringLiteral(lit)) => {
                let template_literal =
                    template_literal_expr_from_raw(ast, attr.span, lit.value.as_str());
                emit_inline_styles_disabled_runtime_update(
                    ast,
                    attr.span,
                    elem_id,
                    template_literal,
                    result,
                    context,
                    options,
                );
            }
            Some(JSXAttributeValue::ExpressionContainer(container)) => {
                if let Some(expr) = container.expression.as_expression() {
                    let attr_has_static_marker =
                        context.has_static_marker_comment(container.span, options.static_marker);
                    let style_value = if attr_has_static_marker {
                        context.clone_expr_without_trivia(expr)
                    } else {
                        context.clone_expr(expr)
                    };
                    emit_inline_styles_disabled_runtime_update(
                        ast,
                        attr.span,
                        elem_id,
                        style_value,
                        result,
                        context,
                        options,
                    );
                }
            }
            None | Some(JSXAttributeValue::Element(_)) | Some(JSXAttributeValue::Fragment(_)) => {}
        }
        return;
    }

    match &attr.value {
        Some(JSXAttributeValue::StringLiteral(lit)) => {
            inline_attribute_on_template(
                result,
                result.is_svg,
                "style",
                Some(lit.value.as_str()),
                omit_quotes,
                needs_spacing,
            );
        }
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            if let Some(expr) = container.expression.as_expression() {
                let attr_has_static_marker =
                    context.has_static_marker_comment(container.span, options.static_marker);

                if let Expression::ObjectExpression(obj) = expr {
                    let mut inline_styles: Vec<String> = Vec::new();
                    let mut filtered_props = ast.vec();

                    for prop in &obj.properties {
                        let mut keep_prop = true;

                        if let ObjectPropertyKind::ObjectProperty(object_prop) = prop {
                            if !object_prop.computed {
                                let string_key = match &object_prop.key {
                                    PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
                                    PropertyKey::StringLiteral(lit) => Some(lit.value.as_str()),
                                    _ => None,
                                };

                                match &object_prop.value {
                                    Expression::StringLiteral(lit) => {
                                        if let Some(key) = string_key {
                                            inline_styles.push(format!(
                                                "{}:{}",
                                                camel_to_kebab(key),
                                                lit.value
                                            ));
                                            keep_prop = false;
                                        }
                                    }
                                    Expression::NumericLiteral(num) => {
                                        if let Some(key) = string_key {
                                            inline_styles.push(format!(
                                                "{}:{}",
                                                camel_to_kebab(key),
                                                num.value
                                            ));
                                            keep_prop = false;
                                        }
                                    }
                                    Expression::NullLiteral(_) => {
                                        keep_prop = false;
                                    }
                                    Expression::Identifier(ident) if ident.name == "undefined" => {
                                        keep_prop = false;
                                    }
                                    _ => {}
                                }
                            }
                        }

                        if keep_prop {
                            filtered_props.push(prop.clone_in(ast.allocator));
                        }
                    }

                    if !inline_styles.is_empty() {
                        let inline_style = inline_styles.join(";");
                        inline_attribute_on_template(
                            result,
                            result.is_svg,
                            "style",
                            Some(inline_style.as_str()),
                            omit_quotes,
                            needs_spacing,
                        );
                    }

                    if filtered_props.is_empty() {
                        return;
                    }

                    let can_split_style_props = filtered_props.iter().all(|prop| {
                        matches!(prop, ObjectPropertyKind::ObjectProperty(prop) if !prop.computed)
                    });

                    if can_split_style_props {
                        let elem_id = elem_id.expect("style helper requires an element id");
                        let mut style_props: Vec<(String, Expression<'a>, bool)> = Vec::new();

                        for prop in &filtered_props {
                            let ObjectPropertyKind::ObjectProperty(prop) = prop else {
                                continue;
                            };

                            let raw_key = match &prop.key {
                                PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                                PropertyKey::StringLiteral(lit) => lit.value.to_string(),
                                _ => continue,
                            };

                            let prop_has_static_marker = attr_has_static_marker
                                || context.has_static_marker_comment_anywhere(
                                    prop.span,
                                    options.static_marker,
                                );

                            let (value, dynamic) = if result.is_svg {
                                match &prop.value {
                                    Expression::StringLiteral(lit) => (
                                        ast.expression_string_literal(
                                            SPAN,
                                            ast.allocator.alloc_str(lit.value.as_str()),
                                            None,
                                        ),
                                        false,
                                    ),
                                    Expression::NumericLiteral(num) => {
                                        let value_text = num.value.to_string();
                                        (
                                            ast.expression_string_literal(
                                                SPAN,
                                                ast.allocator.alloc_str(&value_text),
                                                None,
                                            ),
                                            false,
                                        )
                                    }
                                    _ => (
                                        if prop_has_static_marker {
                                            context.clone_expr_without_trivia(&prop.value)
                                        } else {
                                            context.clone_expr(&prop.value)
                                        },
                                        !prop_has_static_marker && is_dynamic(&prop.value),
                                    ),
                                }
                            } else {
                                (
                                    if prop_has_static_marker {
                                        context.clone_expr_without_trivia(&prop.value)
                                    } else {
                                        context.clone_expr(&prop.value)
                                    },
                                    !prop_has_static_marker && is_dynamic(&prop.value),
                                )
                            };

                            style_props.push((raw_key, value, dynamic));
                        }

                        if result.is_svg {
                            let has_static_style_prop =
                                style_props.iter().any(|(_, _, dynamic)| !*dynamic);
                            if has_static_style_prop {
                                context.register_helper("setStyleProperty");
                            }

                            for (prop, value, dynamic) in style_props {
                                if dynamic {
                                    result.dynamics.push(DynamicBinding {
                                        elem: elem_id.to_string(),
                                        key: format!("style:{}", prop),
                                        value,
                                        is_svg: result.is_svg,
                                        is_ce: result.has_custom_element,
                                        tag_name: result.tag_name.clone().unwrap_or_default(),
                                    });
                                } else {
                                    let set_prop = set_style_property_expr(
                                        ast, attr.span, elem_id, &prop, value,
                                    );
                                    result.exprs.push(set_prop);
                                }
                            }
                        } else {
                            context.register_helper("setStyleProperty");
                            for (prop, value, dynamic) in style_props {
                                if dynamic {
                                    result.dynamics.push(DynamicBinding {
                                        elem: elem_id.to_string(),
                                        key: format!("style:{}", prop),
                                        value,
                                        is_svg: result.is_svg,
                                        is_ce: result.has_custom_element,
                                        tag_name: result.tag_name.clone().unwrap_or_default(),
                                    });
                                } else {
                                    let set_prop = set_style_property_expr(
                                        ast, attr.span, elem_id, &prop, value,
                                    );
                                    result.exprs.push(set_prop);
                                }
                            }
                        }

                        return;
                    }

                    let elem_id = elem_id.expect("style helper requires an element id");
                    let style_value = ast.expression_object(attr.span, filtered_props);

                    let style_dynamic = !attr_has_static_marker
                        && (is_dynamic(&style_value)
                            || !is_confident_static_expression(&style_value));
                    if style_dynamic {
                        result.dynamics.push(DynamicBinding {
                            elem: elem_id.to_string(),
                            key: "style".to_string(),
                            value: style_value,
                            is_svg: result.is_svg,
                            is_ce: result.has_custom_element,
                            tag_name: result.tag_name.clone().unwrap_or_default(),
                        });
                    } else {
                        context.register_helper("style");
                        let elem = ident_expr(ast, attr.span, elem_id);
                        let style = ident_expr(ast, attr.span, "style");
                        let call = call_expr(ast, attr.span, style, [elem, style_value]);
                        result.exprs.push(call);
                    }

                    return;
                }

                let elem_id = elem_id.expect("style helper requires an element id");
                let style_value = if attr_has_static_marker {
                    context.clone_expr_without_trivia(expr)
                } else {
                    context.clone_expr(expr)
                };

                let style_dynamic = !attr_has_static_marker
                    && (is_dynamic(expr) || !is_confident_static_expression(expr));
                if style_dynamic {
                    result.dynamics.push(DynamicBinding {
                        elem: elem_id.to_string(),
                        key: "style".to_string(),
                        value: style_value,
                        is_svg: result.is_svg,
                        is_ce: result.has_custom_element,
                        tag_name: result.tag_name.clone().unwrap_or_default(),
                    });
                } else {
                    context.register_helper("style");
                    let elem = ident_expr(ast, attr.span, elem_id);
                    let style = ident_expr(ast, attr.span, "style");
                    let call = call_expr(ast, attr.span, style, [elem, style_value]);
                    result.exprs.push(call);
                }
            }
        }
        None => {}
        _ => {}
    }
}

/// Convert camelCase to kebab-case
fn camel_to_kebab(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('-');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

/// Transform innerHTML/textContent/innerText
fn transform_inner_content<'a>(
    attr: &JSXAttribute<'a>,
    key: &str,
    elem_id: &str,
    result: &mut TransformResult<'a>,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
) {
    let ast = context.ast();
    let emit_setter = |value: Expression<'a>| -> Option<Expression<'a>> {
        if options.hydratable {
            context.register_helper("setProperty");
            let callee = ident_expr(ast, attr.span, "setProperty");
            let elem = ident_expr(ast, attr.span, elem_id);
            let key_lit = ast.expression_string_literal(SPAN, ast.allocator.alloc_str(key), None);
            return Some(call_expr(ast, attr.span, callee, [elem, key_lit, value]));
        }

        let elem = ident_expr(ast, attr.span, elem_id);
        let member = static_member(ast, attr.span, elem, key);
        let target = expression_to_assignment_target(member)?;
        Some(ast.expression_assignment(SPAN, AssignmentOperator::Assign, target, value))
    };

    if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
        if let Some(expr) = container.expression.as_expression() {
            let has_static_marker =
                context.has_static_marker_comment(container.span, options.static_marker);
            let dynamic_expr = !has_static_marker && is_dynamic(expr);
            let value_expr = if has_static_marker {
                context.clone_expr_without_trivia(expr)
            } else {
                context.clone_expr(expr)
            };

            if dynamic_expr {
                if effect_wrapper_enabled(options) {
                    context.register_helper("effect");
                    let effect = ident_expr(ast, attr.span, "effect");

                    if is_condition_expression(expr) {
                        let source =
                            arrow_zero_params_return_expr(ast, attr.span, context.clone_expr(expr));
                        let Some(callback_setter) = emit_setter(ident_expr(ast, attr.span, "_v$"))
                        else {
                            return;
                        };
                        let callback = arrow_single_param_statement_expr(
                            ast,
                            attr.span,
                            "_v$",
                            callback_setter,
                        );

                        result
                            .exprs
                            .push(call_expr(ast, attr.span, effect, [source, callback]));
                    } else {
                        let Some(setter) = emit_setter(value_expr.clone_in(ast.allocator)) else {
                            return;
                        };
                        let arrow = arrow_zero_params_return_expr(ast, attr.span, setter);
                        result
                            .exprs
                            .push(call_expr(ast, attr.span, effect, [arrow]));
                    }
                } else {
                    let Some(setter) = emit_setter(value_expr) else {
                        return;
                    };
                    result.exprs.push(setter);
                }
            } else {
                let Some(setter) = emit_setter(value_expr) else {
                    return;
                };
                result.exprs.push(setter);
            }
        }
    } else if let Some(JSXAttributeValue::StringLiteral(lit)) = &attr.value {
        let value = if key == "innerHTML" {
            ast.expression_string_literal(
                SPAN,
                ast.allocator.alloc_str(&escape_html(&lit.value, false)),
                None,
            )
        } else {
            ast.expression_string_literal(SPAN, ast.allocator.alloc_str(&lit.value), None)
        };

        let Some(setter) = emit_setter(value) else {
            return;
        };
        result.exprs.push(setter);
    }
}

/// Transform element children
fn transform_children<'a, 'b>(
    element: &JSXElement<'a>,
    result: &mut TransformResult<'a>,
    info: &TransformInfo,
    context: &BlockContext<'a>,
    options: &TransformOptions<'a>,
    transform_child: ChildTransformer<'a, 'b>,
    ctx: &TraverseCtx<'a, ()>,
) {
    fn child_path(base: &[String], node_index: usize) -> Vec<String> {
        let mut path = base.to_vec();
        path.push("firstChild".to_string());
        for _ in 0..node_index {
            path.push("nextSibling".to_string());
        }
        path
    }

    fn sibling_walk_path(step_count: usize) -> Vec<String> {
        let mut path = Vec::with_capacity(step_count);
        for _ in 0..step_count {
            path.push("nextSibling".to_string());
        }
        path
    }

    fn child_walk_context(
        base_root_id: &Option<String>,
        base_path: &[String],
        node_index: usize,
        walker_nodes: &BTreeMap<usize, String>,
    ) -> (Option<String>, Vec<String>) {
        if let Some((prev_index, prev_id)) = walker_nodes.range(..=node_index).next_back() {
            return (
                Some(prev_id.clone()),
                sibling_walk_path(node_index - *prev_index),
            );
        }

        (base_root_id.clone(), child_path(base_path, node_index))
    }

    fn child_accessor<'a>(
        ast: AstBuilder<'a>,
        span: Span,
        parent_id: &str,
        node_index: usize,
        walker_nodes: &BTreeMap<usize, String>,
    ) -> Expression<'a> {
        if let Some((prev_index, prev_id)) = walker_nodes.range(..node_index).next_back() {
            let mut expr = ident_expr(ast, span, prev_id);
            for _ in *prev_index..node_index {
                expr = static_member(ast, span, expr, "nextSibling");
            }
            return expr;
        }

        let mut expr = static_member(ast, span, ident_expr(ast, span, parent_id), "firstChild");
        for _ in 0..node_index {
            expr = static_member(ast, span, expr, "nextSibling");
        }
        expr
    }

    fn push_hydration_markers<'a>(
        ast: AstBuilder<'a>,
        span: Span,
        result: &mut TransformResult<'a>,
        context: &BlockContext<'a>,
        parent_id: &str,
        node_index: &mut usize,
        walker_nodes: &mut BTreeMap<usize, String>,
        allow_prev_anchor: bool,
    ) -> (String, String) {
        result.template.push_str("<!$>");
        result.template_with_closing_tags.push_str("<!$>");

        if allow_prev_anchor {
            if let Some((&prev_index, prev_id)) = walker_nodes.range(..*node_index).next_back() {
                if prev_index + 1 == *node_index {
                    result.template.push_str("<!/>");
                    result.template_with_closing_tags.push_str("<!/>");

                    context.register_helper("getNextMarker");
                    let marker_id = context.generate_uid("el$");
                    let content_id = context.generate_uid("co$");
                    let start =
                        static_member(ast, span, ident_expr(ast, span, prev_id), "nextSibling");
                    let callee = ident_expr(ast, span, "getNextMarker");
                    let mut elements = ast.vec_with_capacity(2);
                    elements.push(Some(ast.binding_pattern_binding_identifier(
                        span,
                        ast.allocator.alloc_str(&marker_id),
                    )));
                    elements.push(Some(ast.binding_pattern_binding_identifier(
                        span,
                        ast.allocator.alloc_str(&content_id),
                    )));
                    let pattern = ast.binding_pattern_array_pattern(span, elements, NONE);
                    result.declarations.push(Declaration {
                        pattern,
                        init: call_expr(ast, span, callee, [start]),
                    });
                    walker_nodes.insert(*node_index + 1, marker_id.clone());
                    *node_index += 2;

                    return (marker_id, content_id);
                }
            }
        }

        let open_index = *node_index;
        let open_id = context.generate_uid("el$");
        result.declarations.push(Declaration {
            pattern: ast
                .binding_pattern_binding_identifier(span, ast.allocator.alloc_str(&open_id)),
            init: child_accessor(ast, span, parent_id, *node_index, walker_nodes),
        });
        walker_nodes.insert(open_index, open_id.clone());
        *node_index += 1;

        result.template.push_str("<!/>");
        result.template_with_closing_tags.push_str("<!/>");

        context.register_helper("getNextMarker");
        let marker_id = context.generate_uid("el$");
        let content_id = context.generate_uid("co$");
        let start = static_member(ast, span, ident_expr(ast, span, &open_id), "nextSibling");
        let callee = ident_expr(ast, span, "getNextMarker");
        let mut elements = ast.vec_with_capacity(2);
        elements.push(Some(ast.binding_pattern_binding_identifier(
            span,
            ast.allocator.alloc_str(&marker_id),
        )));
        elements.push(Some(ast.binding_pattern_binding_identifier(
            span,
            ast.allocator.alloc_str(&content_id),
        )));
        let pattern = ast.binding_pattern_array_pattern(span, elements, NONE);
        result.declarations.push(Declaration {
            pattern,
            init: call_expr(ast, span, callee, [start]),
        });
        walker_nodes.insert(*node_index, marker_id.clone());
        *node_index += 1;

        (marker_id, content_id)
    }

    let hydratable = options.hydratable;
    let parent_tag = common::get_tag_name(element);
    let parent_post_exprs = std::mem::take(&mut result.post_exprs);

    /// Check if children list is a single dynamic expression (no markers needed)
    fn is_single_dynamic_child<'a>(
        children: &[oxc_ast::ast::JSXChild<'a>],
        context: &BlockContext<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) -> bool {
        let mut expr_count = 0;
        let mut other_content = false;

        for child in children {
            match child {
                oxc_ast::ast::JSXChild::Text(text) => {
                    if !normalize_jsx_text(text).is_empty() {
                        other_content = true;
                    }
                }
                oxc_ast::ast::JSXChild::Element(_) => {
                    other_content = true;
                }
                oxc_ast::ast::JSXChild::ExpressionContainer(container) => {
                    if let Some(expr) = container.expression.as_expression() {
                        if static_child_text(expr, context, ctx).is_some() {
                            other_content = true;
                        } else {
                            expr_count += 1;
                        }
                    }
                }
                oxc_ast::ast::JSXChild::Fragment(fragment) => {
                    // Recurse into fragments
                    if !is_single_dynamic_child(&fragment.children, context, ctx) {
                        other_content = true;
                    } else {
                        expr_count += 1;
                    }
                }
                _ => {}
            }
        }

        expr_count == 1 && !other_content
    }

    fn child_has_text_content<'a>(
        child: &oxc_ast::ast::JSXChild<'a>,
        context: &BlockContext<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) -> bool {
        match child {
            oxc_ast::ast::JSXChild::Text(text) => !normalize_jsx_text(text).is_empty(),
            oxc_ast::ast::JSXChild::ExpressionContainer(container) => container
                .expression
                .as_expression()
                .is_some_and(|expr| static_child_text(expr, context, ctx).is_some()),
            oxc_ast::ast::JSXChild::Fragment(fragment) => fragment
                .children
                .iter()
                .any(|child| child_has_text_content(child, context, ctx)),
            _ => false,
        }
    }

    fn child_has_static_anchor<'a>(
        child: &oxc_ast::ast::JSXChild<'a>,
        context: &BlockContext<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) -> bool {
        match child {
            oxc_ast::ast::JSXChild::Text(text) => !normalize_jsx_text(text).is_empty(),
            oxc_ast::ast::JSXChild::ExpressionContainer(container) => container
                .expression
                .as_expression()
                .is_some_and(|expr| static_child_text(expr, context, ctx).is_some()),
            oxc_ast::ast::JSXChild::Element(element) => {
                let tag = common::get_tag_name(element);
                !is_component(&tag)
            }
            oxc_ast::ast::JSXChild::Fragment(fragment) => fragment
                .children
                .iter()
                .any(|child| child_has_static_anchor(child, context, ctx)),
            _ => false,
        }
    }

    fn child_is_meaningful(child: &oxc_ast::ast::JSXChild<'_>) -> bool {
        match child {
            oxc_ast::ast::JSXChild::Text(text) => !normalize_jsx_text(text).is_empty(),
            oxc_ast::ast::JSXChild::ExpressionContainer(container) => {
                container.expression.as_expression().is_some()
            }
            oxc_ast::ast::JSXChild::Element(_) => true,
            oxc_ast::ast::JSXChild::Fragment(fragment) => {
                fragment.children.iter().any(child_is_meaningful)
            }
            oxc_ast::ast::JSXChild::Spread(_) => true,
        }
    }

    fn has_multiple_meaningful_children(children: &[oxc_ast::ast::JSXChild<'_>]) -> bool {
        let mut count = 0usize;
        for child in children {
            if child_is_meaningful(child) {
                count += 1;
                if count > 1 {
                    return true;
                }
            }
        }
        false
    }

    fn child_is_last_element_candidate<'a>(
        child: &oxc_ast::ast::JSXChild<'a>,
        hydratable: bool,
        context: &BlockContext<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) -> bool {
        if hydratable {
            return child_is_meaningful(child);
        }

        match child {
            oxc_ast::ast::JSXChild::Text(text) => !normalize_jsx_text(text).is_empty(),
            oxc_ast::ast::JSXChild::ExpressionContainer(container) => container
                .expression
                .as_expression()
                .is_some_and(|expr| static_child_text(expr, context, ctx).is_some()),
            oxc_ast::ast::JSXChild::Element(element) => {
                let tag = common::get_tag_name(element);
                !is_component(&tag)
            }
            oxc_ast::ast::JSXChild::Fragment(fragment) => fragment
                .children
                .iter()
                .any(|child| child_is_last_element_candidate(child, hydratable, context, ctx)),
            oxc_ast::ast::JSXChild::Spread(_) => false,
        }
    }

    fn find_last_element_index<'a>(
        children: &[oxc_ast::ast::JSXChild<'a>],
        hydratable: bool,
        context: &BlockContext<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) -> Option<usize> {
        children
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, child)| {
                child_is_last_element_candidate(child, hydratable, context, ctx).then_some(index)
            })
    }

    fn has_future_static_anchor<'a>(
        children: &[oxc_ast::ast::JSXChild<'a>],
        start: usize,
        context: &BlockContext<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) -> bool {
        children.get(start..).is_some_and(|rest| {
            rest.iter()
                .any(|child| child_has_static_anchor(child, context, ctx))
        })
    }

    fn is_wrapped_by_text<'a>(
        children: &[oxc_ast::ast::JSXChild<'a>],
        start_index: usize,
        context: &BlockContext<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) -> bool {
        let mut index = start_index;
        let mut wrapped = false;

        while index > 0 {
            index -= 1;
            let child = &children[index];
            if child_has_text_content(child, context, ctx) {
                wrapped = true;
                break;
            }
            if child_has_static_anchor(child, context, ctx) {
                return false;
            }
        }

        if !wrapped {
            return false;
        }

        index = start_index;
        while index + 1 < children.len() {
            index += 1;
            let child = &children[index];
            if child_has_text_content(child, context, ctx) {
                return true;
            }
            if child_has_static_anchor(child, context, ctx) {
                return false;
            }
        }

        false
    }

    fn child_has_dynamic_insert<'a>(
        child: &oxc_ast::ast::JSXChild<'a>,
        options: &TransformOptions<'a>,
        context: &BlockContext<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) -> bool {
        match child {
            oxc_ast::ast::JSXChild::Text(_) => false,
            oxc_ast::ast::JSXChild::ExpressionContainer(container) => container
                .expression
                .as_expression()
                .is_some_and(|expr| static_child_text(expr, context, ctx).is_none()),
            oxc_ast::ast::JSXChild::Spread(_) => true,
            oxc_ast::ast::JSXChild::Element(element) => {
                let tag = common::get_tag_name(element);
                if is_component(&tag) {
                    return true;
                }
                if element_needs_runtime_access(element, options) {
                    return true;
                }
                !element.children.is_empty()
                    && detect_expressions(&element.children, 0, options, context, ctx)
            }
            oxc_ast::ast::JSXChild::Fragment(fragment) => fragment
                .children
                .iter()
                .any(|nested| child_has_dynamic_insert(nested, options, context, ctx)),
        }
    }

    fn detect_expressions<'a>(
        children: &[oxc_ast::ast::JSXChild<'a>],
        index: usize,
        options: &TransformOptions<'a>,
        context: &BlockContext<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) -> bool {
        if index > 0 {
            let mut prev_index = index;
            while prev_index > 0 {
                prev_index -= 1;
                let prev = &children[prev_index];
                match prev {
                    oxc_ast::ast::JSXChild::Text(text) if normalize_jsx_text(text).is_empty() => {
                        continue;
                    }
                    oxc_ast::ast::JSXChild::ExpressionContainer(container)
                        if container.expression.as_expression().is_none() =>
                    {
                        continue;
                    }
                    _ => {}
                }

                match prev {
                    oxc_ast::ast::JSXChild::ExpressionContainer(container) => {
                        if container
                            .expression
                            .as_expression()
                            .is_some_and(|expr| static_child_text(expr, context, ctx).is_none())
                        {
                            return true;
                        }
                    }
                    oxc_ast::ast::JSXChild::Element(element) => {
                        let tag = common::get_tag_name(element);
                        if is_component(&tag) {
                            return true;
                        }
                    }
                    _ => {}
                }
                break;
            }
        }

        for child in children.iter().skip(index) {
            match child {
                oxc_ast::ast::JSXChild::ExpressionContainer(container) => {
                    if container
                        .expression
                        .as_expression()
                        .is_some_and(|expr| static_child_text(expr, context, ctx).is_none())
                    {
                        return true;
                    }
                }
                oxc_ast::ast::JSXChild::Element(element) => {
                    let tag = common::get_tag_name(element);
                    if is_component(&tag) {
                        return true;
                    }
                    if element_needs_runtime_access(element, options) {
                        return true;
                    }
                    if !element.children.is_empty()
                        && detect_expressions(&element.children, 0, options, context, ctx)
                    {
                        return true;
                    }
                }
                oxc_ast::ast::JSXChild::Fragment(fragment) => {
                    if detect_expressions(&fragment.children, 0, options, context, ctx) {
                        return true;
                    }
                }
                _ => {}
            }
        }

        false
    }

    fn child_has_component_insert<'a>(child: &oxc_ast::ast::JSXChild<'a>) -> bool {
        match child {
            oxc_ast::ast::JSXChild::Element(element) => {
                let tag = common::get_tag_name(element);
                is_component(&tag)
            }
            oxc_ast::ast::JSXChild::Fragment(fragment) => {
                fragment.children.iter().any(child_has_component_insert)
            }
            _ => false,
        }
    }

    fn collect_static_walker_slots<'a>(
        children: &[oxc_ast::ast::JSXChild<'a>],
        context: &BlockContext<'a>,
        options: &TransformOptions<'a>,
        ctx: &TraverseCtx<'a, ()>,
        split_adjacent_text_slots: bool,
        include_runtime_elements: bool,
        last_was_text: &mut bool,
        slots: &mut usize,
    ) {
        for child in children {
            match child {
                oxc_ast::ast::JSXChild::Text(text) => {
                    if !normalize_jsx_text(text).is_empty() {
                        if split_adjacent_text_slots || !*last_was_text {
                            *slots += 1;
                        }
                        *last_was_text = true;
                    }
                }
                oxc_ast::ast::JSXChild::ExpressionContainer(container) => {
                    let static_text = container
                        .expression
                        .as_expression()
                        .and_then(|expr| static_child_text(expr, context, ctx));
                    if static_text.as_deref().is_some_and(|text| !text.is_empty()) {
                        if split_adjacent_text_slots || !*last_was_text {
                            *slots += 1;
                        }
                        *last_was_text = true;
                    } else {
                        *last_was_text = false;
                    }
                }
                oxc_ast::ast::JSXChild::Element(element) => {
                    let tag = common::get_tag_name(element);
                    if !is_component(&tag)
                        && (include_runtime_elements
                            || !element_needs_runtime_access(element, options))
                    {
                        *slots += 1;
                    }
                    *last_was_text = false;
                }
                oxc_ast::ast::JSXChild::Spread(_) => {
                    *last_was_text = false;
                }
                oxc_ast::ast::JSXChild::Fragment(fragment) => {
                    collect_static_walker_slots(
                        &fragment.children,
                        context,
                        options,
                        ctx,
                        split_adjacent_text_slots,
                        include_runtime_elements,
                        last_was_text,
                        slots,
                    );
                }
            }
        }
    }

    fn reserve_future_static_walker_slots<'a>(
        children: &[oxc_ast::ast::JSXChild<'a>],
        start: usize,
        context: &BlockContext<'a>,
        options: &TransformOptions<'a>,
        ctx: &TraverseCtx<'a, ()>,
        split_adjacent_text_slots: bool,
        include_runtime_elements: bool,
        static_walker_pool: &mut VecDeque<String>,
    ) {
        if !static_walker_pool.is_empty() {
            return;
        }

        let Some(rest) = children.get(start..) else {
            return;
        };

        let mut slots = 0usize;
        let mut scan_last_was_text = false;
        collect_static_walker_slots(
            rest,
            context,
            options,
            ctx,
            split_adjacent_text_slots,
            include_runtime_elements,
            &mut scan_last_was_text,
            &mut slots,
        );

        for _ in 0..slots {
            static_walker_pool.push_back(context.generate_uid("el$"));
        }
    }

    fn take_static_walker_id<'a>(
        static_walker_pool: &mut VecDeque<String>,
        context: &BlockContext<'a>,
    ) -> String {
        static_walker_pool
            .pop_front()
            .unwrap_or_else(|| context.generate_uid("el$"))
    }

    fn transform_children_list<'a, 'b>(
        children: &[oxc_ast::ast::JSXChild<'a>],
        result: &mut TransformResult<'a>,
        info: &TransformInfo,
        context: &BlockContext<'a>,
        options: &TransformOptions<'a>,
        transform_child: ChildTransformer<'a, 'b>,
        ctx: &TraverseCtx<'a, ()>,
        node_index: &mut usize,
        last_was_text: &mut bool,
        next_placeholder: &mut Option<String>,
        parent_tag: &str,
        walker_nodes: &mut BTreeMap<usize, String>,
        hydratable: bool,
        static_walker_pool: &mut VecDeque<String>,
    ) {
        let ast = context.ast();
        if hydratable && parent_tag == "html" {
            let needs_match_helper = children.iter().any(|child| match child {
                oxc_ast::ast::JSXChild::Element(element) => {
                    let tag = common::get_tag_name(element);
                    !is_component(&tag) && tag != "head"
                }
                _ => false,
            });
            if needs_match_helper {
                context.register_helper("getNextMatch");
            }
        }
        let single_dynamic = is_single_dynamic_child(children, context, ctx);
        let multi = has_multiple_meaningful_children(children);
        let last_element_index = find_last_element_index(children, hydratable, context, ctx);
        let has_component_insert = children.iter().any(child_has_component_insert);
        let track_static_walkers = result.id.is_some()
            && children
                .iter()
                .any(|child| child_has_dynamic_insert(child, options, context, ctx));
        let mut precomputed_components: Option<Vec<Option<TransformResult<'a>>>> = None;

        if hydratable && has_component_insert {
            let mut results = Vec::with_capacity(children.len());
            for child in children {
                if matches!(child, oxc_ast::ast::JSXChild::Element(element) if is_component(&common::get_tag_name(element)))
                {
                    results.push(transform_child(child));
                } else {
                    results.push(None);
                }
            }
            precomputed_components = Some(results);
        }

        for (child_index, child) in children.iter().enumerate() {
            match child {
                oxc_ast::ast::JSXChild::Text(text) => {
                    let content = normalize_jsx_text(text);
                    if !content.is_empty() {
                        let raw_text = jsx_text_source(text);
                        let multiline_trailing_space =
                            raw_text.contains('\n') && raw_text.trim_end().len() < raw_text.len();
                        let next_is_expression = matches!(
                            children.get(child_index + 1),
                            Some(oxc_ast::ast::JSXChild::ExpressionContainer(container))
                                if container.expression.as_expression().is_some()
                        );
                        let append_space = multiline_trailing_space
                            && next_is_expression
                            && !content.ends_with(' ');

                        result.template.push_str(&content);
                        result.template_with_closing_tags.push_str(&content);
                        if append_space {
                            result.template.push(' ');
                            result.template_with_closing_tags.push(' ');
                        }
                        let should_track =
                            detect_expressions(children, child_index, options, context, ctx);
                        if !*last_was_text {
                            let current_index = *node_index;
                            if should_track {
                                if let Some(parent_id) = result.id.as_deref() {
                                    if !walker_nodes.contains_key(&current_index) {
                                        let walker_id =
                                            take_static_walker_id(static_walker_pool, context);
                                        result.declarations.push(Declaration {
                                            pattern: ast.binding_pattern_binding_identifier(
                                                text.span,
                                                ast.allocator.alloc_str(&walker_id),
                                            ),
                                            init: child_accessor(
                                                ast,
                                                text.span,
                                                parent_id,
                                                current_index,
                                                walker_nodes,
                                            ),
                                        });
                                        walker_nodes.insert(current_index, walker_id);
                                    }
                                }
                                *node_index += 1;
                            }
                            *last_was_text = true;
                        } else if should_track && has_component_insert {
                            // Babel still burns UID slots for adjacent static text children
                            // that are merged into the same DOM text node.
                            let _ = take_static_walker_id(static_walker_pool, context);
                        }
                        *next_placeholder = None;
                    }
                }
                oxc_ast::ast::JSXChild::Element(child_elem) => {
                    let child_tag = common::get_tag_name(child_elem);

                    if is_component(&child_tag) {
                        *last_was_text = false;
                        let parent_id = result.id.clone();
                        let child_result = precomputed_components
                            .as_mut()
                            .and_then(|results| results.get_mut(child_index))
                            .and_then(|result| result.take())
                            .or_else(|| transform_child(child));
                        if let (Some(parent_id), Some(child_result)) = (parent_id, child_result) {
                            if child_result.exprs.is_empty() {
                                continue;
                            }

                            result.has_hydratable_event |= child_result.has_hydratable_event;
                            context.register_helper("insert");

                            if single_dynamic {
                                *next_placeholder = None;
                                let callee =
                                    dom_helper_expr(context, ast, child_elem.span, "insert");
                                let parent = ident_expr(ast, child_elem.span, parent_id.as_str());
                                let child_expr = child_result.exprs[0].clone_in(ast.allocator);
                                result.exprs.push(call_expr(
                                    ast,
                                    child_elem.span,
                                    callee,
                                    [parent, child_expr],
                                ));
                            } else if hydratable && multi {
                                *next_placeholder = None;
                                if track_static_walkers {
                                    reserve_future_static_walker_slots(
                                        children,
                                        child_index + 1,
                                        context,
                                        options,
                                        ctx,
                                        has_component_insert,
                                        true,
                                        static_walker_pool,
                                    );
                                }
                                let (marker_id, content_id) = push_hydration_markers(
                                    ast,
                                    child_elem.span,
                                    result,
                                    context,
                                    parent_id.as_str(),
                                    node_index,
                                    walker_nodes,
                                    false,
                                );
                                let callee =
                                    dom_helper_expr(context, ast, child_elem.span, "insert");
                                let parent = ident_expr(ast, child_elem.span, parent_id.as_str());
                                let child_expr = child_result.exprs[0].clone_in(ast.allocator);
                                let marker = ident_expr(ast, child_elem.span, &marker_id);
                                let content = ident_expr(ast, child_elem.span, &content_id);
                                result.exprs.push(call_expr(
                                    ast,
                                    child_elem.span,
                                    callee,
                                    [parent, child_expr, marker, content],
                                ));
                            } else if is_wrapped_by_text(children, child_index, context, ctx) {
                                let marker_id = if let Some(existing) = next_placeholder.as_ref() {
                                    existing.clone()
                                } else {
                                    if track_static_walkers {
                                        reserve_future_static_walker_slots(
                                            children,
                                            child_index + 1,
                                            context,
                                            options,
                                            ctx,
                                            has_component_insert,
                                            false,
                                            static_walker_pool,
                                        );
                                    }
                                    result.template.push_str("<!>");
                                    result.template_with_closing_tags.push_str("<!>");
                                    let marker_index = *node_index;
                                    let marker_id = context.generate_uid("el$");
                                    result.declarations.push(Declaration {
                                        pattern: ast.binding_pattern_binding_identifier(
                                            child_elem.span,
                                            ast.allocator.alloc_str(&marker_id),
                                        ),
                                        init: child_accessor(
                                            ast,
                                            child_elem.span,
                                            parent_id.as_str(),
                                            *node_index,
                                            walker_nodes,
                                        ),
                                    });
                                    walker_nodes.insert(marker_index, marker_id.clone());
                                    *node_index += 1;
                                    marker_id
                                };
                                *next_placeholder = Some(marker_id.clone());

                                let callee =
                                    dom_helper_expr(context, ast, child_elem.span, "insert");
                                let parent = ident_expr(ast, child_elem.span, parent_id.as_str());
                                let child_expr = child_result.exprs[0].clone_in(ast.allocator);
                                let marker = ident_expr(ast, child_elem.span, &marker_id);
                                result.exprs.push(call_expr(
                                    ast,
                                    child_elem.span,
                                    callee,
                                    [parent, child_expr, marker],
                                ));
                            } else if multi {
                                *next_placeholder = None;
                                let callee =
                                    dom_helper_expr(context, ast, child_elem.span, "insert");
                                let parent = ident_expr(ast, child_elem.span, parent_id.as_str());
                                let child_expr = child_result.exprs[0].clone_in(ast.allocator);

                                if has_future_static_anchor(children, child_index + 1, context, ctx)
                                {
                                    let marker_index = *node_index;
                                    let marker_id =
                                        take_static_walker_id(static_walker_pool, context);
                                    result.declarations.push(Declaration {
                                        pattern: ast.binding_pattern_binding_identifier(
                                            child_elem.span,
                                            ast.allocator.alloc_str(&marker_id),
                                        ),
                                        init: child_accessor(
                                            ast,
                                            child_elem.span,
                                            parent_id.as_str(),
                                            *node_index,
                                            walker_nodes,
                                        ),
                                    });
                                    walker_nodes.insert(marker_index, marker_id.clone());
                                    let marker = ident_expr(ast, child_elem.span, &marker_id);
                                    result.exprs.push(call_expr(
                                        ast,
                                        child_elem.span,
                                        callee,
                                        [parent, child_expr, marker],
                                    ));
                                } else {
                                    let null_marker = ast.expression_null_literal(child_elem.span);
                                    result.exprs.push(call_expr(
                                        ast,
                                        child_elem.span,
                                        callee,
                                        [parent, child_expr, null_marker],
                                    ));
                                }
                            } else {
                                *next_placeholder = None;
                                let callee =
                                    dom_helper_expr(context, ast, child_elem.span, "insert");
                                let parent = ident_expr(ast, child_elem.span, parent_id.as_str());
                                let child_expr = child_result.exprs[0].clone_in(ast.allocator);
                                result.exprs.push(call_expr(
                                    ast,
                                    child_elem.span,
                                    callee,
                                    [parent, child_expr],
                                ));
                            }
                        }
                        continue;
                    }

                    *last_was_text = false;
                    *next_placeholder = None;
                    let (walk_root_id, walk_path) =
                        child_walk_context(&info.root_id, &info.path, *node_index, walker_nodes);
                    let mut child_info = TransformInfo {
                        top_level: false,
                        last_element: last_element_index == Some(child_index),
                        path: walk_path,
                        root_id: walk_root_id,
                        forced_id: None,
                        ..info.clone()
                    };
                    if hydratable && parent_tag == "html" {
                        child_info.match_tag = Some(child_tag.to_string());
                    }

                    if track_static_walkers
                        && !static_walker_pool.is_empty()
                        && !child_info.skip_id
                        && child_info.root_id.is_some()
                        && !child_info.path.is_empty()
                        && element_needs_runtime_access(child_elem, options)
                    {
                        child_info.forced_id =
                            Some(take_static_walker_id(static_walker_pool, context));
                    }

                    if matches!(options.generate, common::GenerateMode::Dynamic)
                        && options.should_use_universal_for_intrinsic(&child_tag)
                    {
                        panic!(
                            "dynamic mode does not support direct universal intrinsic child <{}> inside DOM parent <{}>",
                            child_tag, parent_tag
                        );
                    }

                    let child_result = transform_element(
                        child_elem,
                        &child_tag,
                        &child_info,
                        context,
                        options,
                        transform_child,
                        ctx,
                    );
                    let child_id = child_result.id.clone();

                    result.template.push_str(&child_result.template);
                    if !child_result.template_with_closing_tags.is_empty() {
                        result
                            .template_with_closing_tags
                            .push_str(&child_result.template_with_closing_tags);
                    } else {
                        result
                            .template_with_closing_tags
                            .push_str(&child_result.template);
                    }

                    if hydratable && child_tag == "head" {
                        context.register_helper("createComponent");
                        context.register_helper("NoHydration");
                        let callee = ident_expr(ast, child_elem.span, "createComponent");
                        let no_hydration = ident_expr(ast, child_elem.span, "NoHydration");
                        let props = ast.expression_object(child_elem.span, ast.vec());

                        result.exprs.push(call_expr(
                            ast,
                            child_elem.span,
                            callee,
                            [no_hydration, props],
                        ));
                        // Babel does not advance the node index for hydratable <head> children.
                        continue;
                    }

                    result.declarations.extend(child_result.declarations);

                    if !child_result.statements.is_empty() && !result.exprs.is_empty() {
                        let existing_exprs = std::mem::take(&mut result.exprs);
                        for expr in existing_exprs {
                            result.statements.push(Statement::ExpressionStatement(
                                ast.alloc_expression_statement(SPAN, expr),
                            ));
                        }
                    }

                    result.statements.extend(child_result.statements);
                    result.exprs.extend(child_result.exprs);
                    result.dynamics.extend(child_result.dynamics);
                    result.post_exprs.extend(child_result.post_exprs);
                    result.has_custom_element |= child_result.has_custom_element;
                    result.has_hydratable_event |= child_result.has_hydratable_event;
                    result.is_import_node |= child_result.is_import_node;

                    if let Some(child_id) = child_id {
                        if child_info.root_id.is_some() && !child_info.path.is_empty() {
                            walker_nodes.insert(*node_index, child_id);
                        }
                    } else if track_static_walkers
                        && child_info.root_id.is_some()
                        && !child_info.path.is_empty()
                    {
                        let current_index = *node_index;
                        if let Some(parent_id) = result.id.as_deref() {
                            if !walker_nodes.contains_key(&current_index) {
                                let walker_id = take_static_walker_id(static_walker_pool, context);
                                result.declarations.push(Declaration {
                                    pattern: ast.binding_pattern_binding_identifier(
                                        child_elem.span,
                                        ast.allocator.alloc_str(&walker_id),
                                    ),
                                    init: child_accessor(
                                        ast,
                                        child_elem.span,
                                        parent_id,
                                        current_index,
                                        walker_nodes,
                                    ),
                                });
                                walker_nodes.insert(current_index, walker_id);
                            }
                        }
                    }

                    *node_index += 1;
                }
                oxc_ast::ast::JSXChild::ExpressionContainer(container) => {
                    let has_static_marker =
                        context.has_static_marker_comment(container.span, options.static_marker);
                    if let Some(expr) = container.expression.as_expression() {
                        if let Some(static_text) = static_child_text(expr, context, ctx) {
                            if !static_text.is_empty() {
                                let escaped = escape_html(&static_text, false);
                                result.template.push_str(&escaped);
                                result.template_with_closing_tags.push_str(&escaped);
                                let should_track = detect_expressions(
                                    children,
                                    child_index,
                                    options,
                                    context,
                                    ctx,
                                );
                                if !*last_was_text {
                                    let current_index = *node_index;
                                    if should_track {
                                        if let Some(parent_id) = result.id.as_deref() {
                                            if !walker_nodes.contains_key(&current_index) {
                                                let walker_id = take_static_walker_id(
                                                    static_walker_pool,
                                                    context,
                                                );
                                                result.declarations.push(Declaration {
                                                    pattern: ast
                                                        .binding_pattern_binding_identifier(
                                                            container.span,
                                                            ast.allocator.alloc_str(&walker_id),
                                                        ),
                                                    init: child_accessor(
                                                        ast,
                                                        container.span,
                                                        parent_id,
                                                        current_index,
                                                        walker_nodes,
                                                    ),
                                                });
                                                walker_nodes.insert(current_index, walker_id);
                                            }
                                        }
                                        *node_index += 1;
                                    }
                                    *last_was_text = true;
                                } else if should_track && has_component_insert {
                                    // Preserve Babel UID consumption for merged adjacent
                                    // static-expression text nodes.
                                    let _ = take_static_walker_id(static_walker_pool, context);
                                }
                            }
                            *next_placeholder = None;
                            continue;
                        }
                    }

                    let parent_id = result.id.clone();
                    if let (Some(parent_id), Some(expr)) =
                        (parent_id, container.expression.as_expression())
                    {
                        *last_was_text = false;

                        let insert_value = if has_static_marker {
                            context.clone_expr_without_trivia(expr)
                        } else {
                            normalize_insert_value(ast, container.span, expr, context, options)
                        };

                        context.register_helper("insert");

                        if single_dynamic {
                            *next_placeholder = None;
                            let callee = dom_helper_expr(context, ast, container.span, "insert");
                            let parent = ident_expr(ast, container.span, parent_id.as_str());
                            result.exprs.push(call_expr(
                                ast,
                                container.span,
                                callee,
                                [parent, insert_value],
                            ));
                        } else if hydratable && multi {
                            *next_placeholder = None;
                            if track_static_walkers {
                                reserve_future_static_walker_slots(
                                    children,
                                    child_index + 1,
                                    context,
                                    options,
                                    ctx,
                                    has_component_insert,
                                    true,
                                    static_walker_pool,
                                );
                            }
                            let (marker_id, content_id) = push_hydration_markers(
                                ast,
                                container.span,
                                result,
                                context,
                                parent_id.as_str(),
                                node_index,
                                walker_nodes,
                                false,
                            );
                            let callee = dom_helper_expr(context, ast, container.span, "insert");
                            let parent = ident_expr(ast, container.span, parent_id.as_str());
                            let marker = ident_expr(ast, container.span, &marker_id);
                            let content = ident_expr(ast, container.span, &content_id);
                            result.exprs.push(call_expr(
                                ast,
                                container.span,
                                callee,
                                [parent, insert_value, marker, content],
                            ));
                        } else if is_wrapped_by_text(children, child_index, context, ctx) {
                            let marker_id = if let Some(existing) = next_placeholder.as_ref() {
                                existing.clone()
                            } else {
                                if track_static_walkers {
                                    reserve_future_static_walker_slots(
                                        children,
                                        child_index + 1,
                                        context,
                                        options,
                                        ctx,
                                        has_component_insert,
                                        false,
                                        static_walker_pool,
                                    );
                                }
                                result.template.push_str("<!>");
                                result.template_with_closing_tags.push_str("<!>");
                                let marker_index = *node_index;
                                let marker_id = context.generate_uid("el$");
                                result.declarations.push(Declaration {
                                    pattern: ast.binding_pattern_binding_identifier(
                                        container.span,
                                        ast.allocator.alloc_str(&marker_id),
                                    ),
                                    init: child_accessor(
                                        ast,
                                        container.span,
                                        parent_id.as_str(),
                                        *node_index,
                                        walker_nodes,
                                    ),
                                });
                                walker_nodes.insert(marker_index, marker_id.clone());
                                *node_index += 1;
                                marker_id
                            };
                            *next_placeholder = Some(marker_id.clone());

                            let callee = dom_helper_expr(context, ast, container.span, "insert");
                            let parent = ident_expr(ast, container.span, parent_id.as_str());
                            let marker = ident_expr(ast, container.span, &marker_id);
                            result.exprs.push(call_expr(
                                ast,
                                container.span,
                                callee,
                                [parent, insert_value, marker],
                            ));
                        } else if multi {
                            *next_placeholder = None;
                            let callee = dom_helper_expr(context, ast, container.span, "insert");
                            let parent = ident_expr(ast, container.span, parent_id.as_str());

                            if has_future_static_anchor(children, child_index + 1, context, ctx) {
                                let marker_index = *node_index;
                                let marker_id = take_static_walker_id(static_walker_pool, context);
                                result.declarations.push(Declaration {
                                    pattern: ast.binding_pattern_binding_identifier(
                                        container.span,
                                        ast.allocator.alloc_str(&marker_id),
                                    ),
                                    init: child_accessor(
                                        ast,
                                        container.span,
                                        parent_id.as_str(),
                                        *node_index,
                                        walker_nodes,
                                    ),
                                });
                                walker_nodes.insert(marker_index, marker_id.clone());
                                let marker = ident_expr(ast, container.span, &marker_id);
                                result.exprs.push(call_expr(
                                    ast,
                                    container.span,
                                    callee,
                                    [parent, insert_value, marker],
                                ));
                            } else {
                                let null_marker = ast.expression_null_literal(container.span);
                                result.exprs.push(call_expr(
                                    ast,
                                    container.span,
                                    callee,
                                    [parent, insert_value, null_marker],
                                ));
                            }
                        } else {
                            *next_placeholder = None;
                            let callee = dom_helper_expr(context, ast, container.span, "insert");
                            let parent = ident_expr(ast, container.span, parent_id.as_str());
                            result.exprs.push(call_expr(
                                ast,
                                container.span,
                                callee,
                                [parent, insert_value],
                            ));
                        }
                    }
                }
                oxc_ast::ast::JSXChild::Spread(spread) => {
                    let parent_id = result.id.clone();
                    if let Some(parent_id) = parent_id {
                        *last_was_text = false;
                        context.register_helper("insert");

                        let insert_value = if is_dynamic(&spread.expression) {
                            arrow_zero_params_return_expr(
                                ast,
                                spread.span,
                                context.clone_expr(&spread.expression),
                            )
                        } else {
                            context.clone_expr(&spread.expression)
                        };

                        if single_dynamic {
                            *next_placeholder = None;
                            let callee = dom_helper_expr(context, ast, spread.span, "insert");
                            let parent = ident_expr(ast, spread.span, parent_id.as_str());
                            result.exprs.push(call_expr(
                                ast,
                                spread.span,
                                callee,
                                [parent, insert_value],
                            ));
                        } else if hydratable && multi {
                            *next_placeholder = None;
                            if track_static_walkers {
                                reserve_future_static_walker_slots(
                                    children,
                                    child_index + 1,
                                    context,
                                    options,
                                    ctx,
                                    has_component_insert,
                                    true,
                                    static_walker_pool,
                                );
                            }
                            let (marker_id, content_id) = push_hydration_markers(
                                ast,
                                spread.span,
                                result,
                                context,
                                parent_id.as_str(),
                                node_index,
                                walker_nodes,
                                false,
                            );
                            let callee = dom_helper_expr(context, ast, spread.span, "insert");
                            let parent = ident_expr(ast, spread.span, parent_id.as_str());
                            let marker = ident_expr(ast, spread.span, &marker_id);
                            let content = ident_expr(ast, spread.span, &content_id);
                            result.exprs.push(call_expr(
                                ast,
                                spread.span,
                                callee,
                                [parent, insert_value, marker, content],
                            ));
                        } else if is_wrapped_by_text(children, child_index, context, ctx) {
                            let marker_id = if let Some(existing) = next_placeholder.as_ref() {
                                existing.clone()
                            } else {
                                if track_static_walkers {
                                    reserve_future_static_walker_slots(
                                        children,
                                        child_index + 1,
                                        context,
                                        options,
                                        ctx,
                                        has_component_insert,
                                        false,
                                        static_walker_pool,
                                    );
                                }
                                result.template.push_str("<!>");
                                result.template_with_closing_tags.push_str("<!>");
                                let marker_index = *node_index;
                                let marker_id = context.generate_uid("el$");
                                result.declarations.push(Declaration {
                                    pattern: ast.binding_pattern_binding_identifier(
                                        spread.span,
                                        ast.allocator.alloc_str(&marker_id),
                                    ),
                                    init: child_accessor(
                                        ast,
                                        spread.span,
                                        parent_id.as_str(),
                                        *node_index,
                                        walker_nodes,
                                    ),
                                });
                                walker_nodes.insert(marker_index, marker_id.clone());
                                *node_index += 1;
                                marker_id
                            };
                            *next_placeholder = Some(marker_id.clone());

                            let callee = dom_helper_expr(context, ast, spread.span, "insert");
                            let parent = ident_expr(ast, spread.span, parent_id.as_str());
                            let marker = ident_expr(ast, spread.span, &marker_id);
                            result.exprs.push(call_expr(
                                ast,
                                spread.span,
                                callee,
                                [parent, insert_value, marker],
                            ));
                        } else if multi {
                            *next_placeholder = None;
                            let callee = dom_helper_expr(context, ast, spread.span, "insert");
                            let parent = ident_expr(ast, spread.span, parent_id.as_str());

                            if has_future_static_anchor(children, child_index + 1, context, ctx) {
                                let marker_index = *node_index;
                                let marker_id = take_static_walker_id(static_walker_pool, context);
                                result.declarations.push(Declaration {
                                    pattern: ast.binding_pattern_binding_identifier(
                                        spread.span,
                                        ast.allocator.alloc_str(&marker_id),
                                    ),
                                    init: child_accessor(
                                        ast,
                                        spread.span,
                                        parent_id.as_str(),
                                        *node_index,
                                        walker_nodes,
                                    ),
                                });
                                walker_nodes.insert(marker_index, marker_id.clone());
                                let marker = ident_expr(ast, spread.span, &marker_id);
                                result.exprs.push(call_expr(
                                    ast,
                                    spread.span,
                                    callee,
                                    [parent, insert_value, marker],
                                ));
                            } else {
                                let null_marker = ast.expression_null_literal(spread.span);
                                result.exprs.push(call_expr(
                                    ast,
                                    spread.span,
                                    callee,
                                    [parent, insert_value, null_marker],
                                ));
                            }
                        } else {
                            *next_placeholder = None;
                            let callee = dom_helper_expr(context, ast, spread.span, "insert");
                            let parent = ident_expr(ast, spread.span, parent_id.as_str());
                            result.exprs.push(call_expr(
                                ast,
                                spread.span,
                                callee,
                                [parent, insert_value],
                            ));
                        }
                    }
                }
                oxc_ast::ast::JSXChild::Fragment(fragment) => {
                    transform_children_list(
                        &fragment.children,
                        result,
                        info,
                        context,
                        options,
                        transform_child,
                        ctx,
                        node_index,
                        last_was_text,
                        next_placeholder,
                        parent_tag,
                        walker_nodes,
                        hydratable,
                        static_walker_pool,
                    );
                }
            }
        }
    }

    let mut static_walker_pool = VecDeque::new();

    let mut node_index = 0usize;
    let mut last_was_text = false;
    let mut next_placeholder = None;
    let mut walker_nodes = BTreeMap::new();
    transform_children_list(
        &element.children,
        result,
        info,
        context,
        options,
        transform_child,
        ctx,
        &mut node_index,
        &mut last_was_text,
        &mut next_placeholder,
        &*parent_tag,
        &mut walker_nodes,
        hydratable,
        &mut static_walker_pool,
    );

    result.post_exprs.extend(parent_post_exprs);
}
