//! SSR element transform
//!
//! Transforms native HTML elements into SSR template strings.
//! Unlike DOM, we don't create DOM nodes - we build strings.

use oxc_allocator::CloneIn;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, BinaryOperator, Expression, FormalParameterKind,
    JSXAttribute, JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement,
    ObjectPropertyKind, PropertyKey, PropertyKind, Statement,
};
use oxc_ast::NONE;
use oxc_span::{GetSpanMut, Span, SPAN};
use oxc_syntax::{
    identifier::is_identifier_name,
    keyword::is_reserved_keyword,
    operator::{LogicalOperator, UnaryOperator},
};

use common::{
    constants::{ALIASES, CHILD_PROPERTIES, VOID_ELEMENTS},
    expression::{escape_html, expr_to_string},
    get_attr_name, is_dynamic, is_svg_element, TransformOptions,
};

use crate::{
    ir::{helper_ident_expr, SSRContext, SSRResult},
    template::{self, hoist_expression},
};

/// Transform a native HTML/SVG element for SSR
pub fn transform_element<'a>(
    element: &JSXElement<'a>,
    tag_name: &str,
    top_level: bool,
    context: &SSRContext<'a>,
    options: &TransformOptions<'a>,
) -> SSRResult<'a> {
    let is_void = VOID_ELEMENTS.contains(tag_name);
    let is_script_or_style = tag_name == "script" || tag_name == "style";
    let ast = context.ast();

    let mut result = SSRResult::new();
    result.span = element.span;

    // Check for spread attributes - need different handling
    let has_spread = element
        .opening_element
        .attributes
        .iter()
        .any(|a| matches!(a, JSXAttributeItem::SpreadAttribute(_)));

    if has_spread {
        return transform_element_with_spread(element, tag_name, top_level, context, options);
    }

    // Start the tag
    result.push_static(&format!("<{}", tag_name));

    // Add hydration key if needed
    if top_level && context.hydratable && options.hydratable {
        context.register_helper("ssrHydrationKey");
        let expr = ast.expression_call(
            SPAN,
            helper_ident_expr(ast, SPAN, "ssrHydrationKey"),
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            ast.vec(),
            false,
        );
        let hoisted = hoist_expression(context, &mut result, expr, false, false, true);
        result.push_dynamic(hoisted);
    }

    // Transform attributes
    transform_attributes(element, tag_name, &mut result, context, options);

    // Close opening tag
    result.push_static(">");

    // Transform children (if not void element)
    if !is_void {
        transform_children(element, &mut result, is_script_or_style, context, options);
        result.push_static(&format!("</{}>", tag_name));
    }

    result
}

fn arrow_expr<'a>(
    ast: oxc_ast::AstBuilder<'a>,
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

fn normalize_spread_argument_expr<'a>(
    context: &SSRContext<'a>,
    expr: &Expression<'a>,
) -> Expression<'a> {
    if let Expression::CallExpression(call) = expr {
        if call.arguments.is_empty() {
            if let Expression::Identifier(ident) = &call.callee {
                if ident.name == "results" {
                    return context.clone_expr(&call.callee);
                }
            }
        }
    }

    context.clone_expr(expr)
}

fn is_filtered_spread_child(child: &JSXChild<'_>) -> bool {
    match child {
        JSXChild::ExpressionContainer(container) => container.expression.as_expression().is_none(),
        JSXChild::Text(text) => {
            let raw = text.value.as_str();
            let starts_with_newline = raw.starts_with('\n') || raw.starts_with('\r');
            starts_with_newline && raw.chars().all(|c| c.is_whitespace())
        }
        _ => false,
    }
}

fn spread_children_are_multi(filtered_children: &[&JSXChild<'_>]) -> bool {
    let mut count = 0usize;

    for child in filtered_children {
        match child {
            JSXChild::ExpressionContainer(container) => {
                if container.expression.as_expression().is_some() {
                    count += 1;
                }
            }
            JSXChild::Text(text) => {
                let raw = text.value.as_str();
                let all_whitespace = raw.chars().all(|c| c.is_whitespace());
                let all_spaces_only = !raw.is_empty() && raw.chars().all(|c| c == ' ');
                if !all_whitespace || all_spaces_only {
                    count += 1;
                }
            }
            _ => {
                count += 1;
            }
        }

        if count > 1 {
            return true;
        }
    }

    false
}

/// Transform element with spread attributes using ssrElement()
fn transform_element_with_spread<'a>(
    element: &JSXElement<'a>,
    tag_name: &str,
    top_level: bool,
    context: &SSRContext<'a>,
    options: &TransformOptions<'a>,
) -> SSRResult<'a> {
    context.register_helper("ssrElement");
    context.register_helper("escape");
    let ast = context.ast();
    let span = SPAN;

    let mut result = SSRResult::new();
    result.span = element.span;
    result.spread_element = true;

    // Build props as chunks to preserve spread boundaries.
    let is_svg = is_svg_element(tag_name);
    let has_children = !element.children.is_empty();
    let do_not_escape = matches!(tag_name, "script" | "style");
    let mut props_chunks: Vec<Expression<'a>> = Vec::new();
    let mut running_object: Vec<ObjectPropertyKind<'a>> = Vec::new();

    let make_prop_key = |attr_name: &str| {
        if is_identifier_name(attr_name) {
            PropertyKey::StaticIdentifier(
                ast.alloc_identifier_name(span, ast.allocator.alloc_str(attr_name)),
            )
        } else {
            PropertyKey::StringLiteral(ast.alloc_string_literal(
                span,
                ast.allocator.alloc_str(attr_name),
                None,
            ))
        }
    };

    let make_getter = |expr: Expression<'a>| {
        let params = ast.alloc_formal_parameters(
            SPAN,
            oxc_ast::ast::FormalParameterKind::FormalParameter,
            ast.vec(),
            NONE,
        );
        let mut statements = ast.vec_with_capacity(1);
        statements.push(Statement::ReturnStatement(
            ast.alloc_return_statement(SPAN, Some(expr)),
        ));
        let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
        ast.expression_function(
            SPAN,
            oxc_ast::ast::FunctionType::FunctionExpression,
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
    };

    let flush_running_object =
        |props_chunks: &mut Vec<Expression<'a>>,
         running_object: &mut Vec<ObjectPropertyKind<'a>>| {
            if running_object.is_empty() {
                return;
            }

            let mut object_props = ast.vec_with_capacity(running_object.len());
            for prop in running_object.drain(..) {
                object_props.push(prop);
            }
            props_chunks.push(ast.expression_object(span, object_props));
        };

    for attr in &element.opening_element.attributes {
        match attr {
            JSXAttributeItem::SpreadAttribute(spread) => {
                flush_running_object(&mut props_chunks, &mut running_object);
                props_chunks.push(normalize_spread_argument_expr(context, &spread.argument));
            }
            JSXAttributeItem::Attribute(attr) => {
                let key = get_attr_name(&attr.name);
                // JSX children override spread/object `children` props in Babel's createElement.
                if has_children && key == "children" {
                    continue;
                }
                // Skip client-only attributes
                if key == "ref"
                    || key.starts_with("on")
                    || key.starts_with("use:")
                    || key.starts_with("prop:")
                {
                    continue;
                }

                let attr_name = if is_svg {
                    key.to_string()
                } else {
                    ALIASES.get(&*key).copied().unwrap_or(&*key).to_string()
                };

                match &attr.value {
                    Some(JSXAttributeValue::StringLiteral(lit)) => {
                        let key = make_prop_key(&attr_name);
                        let value = ast.expression_string_literal(
                            span,
                            ast.allocator.alloc_str(&escape_html(&lit.value, true)),
                            None,
                        );
                        running_object.push(ast.object_property_kind_object_property(
                            span,
                            PropertyKind::Init,
                            key,
                            value,
                            false,
                            false,
                            false,
                        ));
                    }
                    Some(JSXAttributeValue::ExpressionContainer(container)) => {
                        if let Some(expr) = container.expression.as_expression() {
                            let has_static_marker = context
                                .has_static_marker_comment(container.span, options.static_marker);
                            let value_expr = if has_static_marker {
                                context.clone_expr_without_trivia(expr)
                            } else {
                                context.clone_expr(expr)
                            };
                            let dynamic_expr = !has_static_marker && is_dynamic(expr);

                            if dynamic_expr {
                                let getter_computed = !is_identifier_name(&attr_name)
                                    || is_reserved_keyword(&attr_name);
                                let getter_key = if getter_computed {
                                    PropertyKey::StringLiteral(ast.alloc_string_literal(
                                        span,
                                        ast.allocator.alloc_str(&attr_name),
                                        None,
                                    ))
                                } else {
                                    make_prop_key(&attr_name)
                                };

                                running_object.push(ast.object_property_kind_object_property(
                                    span,
                                    PropertyKind::Get,
                                    getter_key,
                                    make_getter(value_expr),
                                    false,
                                    false,
                                    getter_computed,
                                ));
                            } else {
                                running_object.push(ast.object_property_kind_object_property(
                                    span,
                                    PropertyKind::Init,
                                    make_prop_key(&attr_name),
                                    value_expr,
                                    false,
                                    false,
                                    false,
                                ));
                            }
                        }
                    }
                    None => {
                        let key = make_prop_key(&attr_name);
                        let value = ast.expression_boolean_literal(span, true);
                        running_object.push(ast.object_property_kind_object_property(
                            span,
                            PropertyKind::Init,
                            key,
                            value,
                            false,
                            false,
                            false,
                        ));
                    }
                    _ => {}
                }
            }
        }
    }

    flush_running_object(&mut props_chunks, &mut running_object);
    if props_chunks.is_empty() {
        props_chunks.push(ast.expression_object(span, ast.vec()));
    }

    let props_expr = if props_chunks.len() > 1 {
        context.register_helper("mergeProps");
        let callee = helper_ident_expr(ast, span, "mergeProps");
        let mut args = ast.vec_with_capacity(props_chunks.len());
        for chunk in props_chunks {
            args.push(Argument::from(chunk));
        }
        ast.expression_call(
            span,
            callee,
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            args,
            false,
        )
    } else {
        props_chunks
            .pop()
            .unwrap_or_else(|| ast.expression_object(span, ast.vec()))
    };

    // Build children (Babel createElement parity for spread-element path)
    let filtered_children: Vec<&JSXChild<'a>> = element
        .children
        .iter()
        .filter(|child| !is_filtered_spread_child(child))
        .collect();
    let markers =
        context.hydratable && options.hydratable && spread_children_are_multi(&filtered_children);

    let mut child_nodes: Vec<Expression<'a>> = Vec::new();
    for child in filtered_children {
        match child {
            JSXChild::Text(text) => {
                let content = common::expression::trim_whitespace(&text.value);
                if !content.is_empty() {
                    child_nodes.push(ast.expression_string_literal(
                        span,
                        ast.allocator.alloc_str(&content),
                        None,
                    ));
                }
            }
            JSXChild::ExpressionContainer(container) => {
                if let Some(expr) = container.expression.as_expression() {
                    let has_static_marker =
                        context.has_static_marker_comment(container.span, options.static_marker);
                    let dynamic_expr = !has_static_marker && is_dynamic(expr);
                    let mut value_expr = if has_static_marker {
                        context.clone_expr_without_trivia(expr)
                    } else {
                        context.clone_expr(expr)
                    };

                    if !do_not_escape {
                        value_expr = wrap_escape_expr(context, value_expr, false);
                    }
                    if dynamic_expr {
                        value_expr = arrow_expr(ast, span, value_expr);
                    }

                    if markers {
                        child_nodes.push(ast.expression_string_literal(span, "<!--$-->", None));
                    }
                    child_nodes.push(value_expr);
                    if markers {
                        child_nodes.push(ast.expression_string_literal(span, "<!--/-->", None));
                    }
                }
            }
            JSXChild::Spread(spread) => {
                let has_static_marker =
                    context.has_static_marker_comment(spread.span, options.static_marker);
                let dynamic_expr = !has_static_marker && is_dynamic(&spread.expression);
                let mut value_expr = if has_static_marker {
                    context.clone_expr_without_trivia(&spread.expression)
                } else {
                    context.clone_expr(&spread.expression)
                };

                if !do_not_escape {
                    value_expr = wrap_escape_expr(context, value_expr, false);
                }
                if dynamic_expr {
                    value_expr = arrow_expr(ast, span, value_expr);
                }

                if markers {
                    child_nodes.push(ast.expression_string_literal(span, "<!--$-->", None));
                }
                child_nodes.push(value_expr);
                if markers {
                    child_nodes.push(ast.expression_string_literal(span, "<!--/-->", None));
                }
            }
            JSXChild::Element(child_elem) => {
                // Recursively transform child element - check if component or native
                let child_tag = common::get_tag_name(child_elem);
                let child_result = if common::is_component(&child_tag) {
                    // Component - use component transformer
                    let child_transformer = |child: &JSXChild<'a>| -> Option<SSRResult<'a>> {
                        match child {
                            JSXChild::Element(el) => {
                                let tag = common::get_tag_name(el);
                                Some(if common::is_component(&tag) {
                                    // For deeply nested components, use simple fallback
                                    context.register_helper("createComponent");
                                    context.register_helper("escape");

                                    let mut r = SSRResult::new();
                                    r.span = el.span;
                                    let callee = helper_ident_expr(ast, span, "createComponent");
                                    let mut args = ast.vec();
                                    let tag_expr = ast
                                        .expression_identifier(span, ast.allocator.alloc_str(&tag));
                                    args.push(Argument::from(tag_expr));
                                    args.push(Argument::from(
                                        ast.expression_object(span, ast.vec()),
                                    ));
                                    let call = ast.expression_call(
                                        span,
                                        callee,
                                        None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                                        args,
                                        false,
                                    );
                                    r.append_expr(call);
                                    r
                                } else {
                                    transform_element(el, &tag, false, context, options)
                                })
                            }
                            _ => None,
                        }
                    };
                    crate::component::transform_component(
                        child_elem,
                        &child_tag,
                        context,
                        options,
                        &child_transformer,
                    )
                } else {
                    transform_element(child_elem, &child_tag, false, context, options)
                };

                let child_has_expr = !child_result.exprs.is_empty();
                let child_is_spread_element = child_result.spread_element;
                let mut child_expr =
                    template::create_template_expression(context, ast, &child_result, None);

                if child_has_expr && !do_not_escape && !child_is_spread_element {
                    child_expr = wrap_escape_expr(context, child_expr, false);
                }

                if markers && child_has_expr && !child_is_spread_element {
                    child_nodes.push(ast.expression_string_literal(span, "<!--$-->", None));
                }
                child_nodes.push(child_expr);
                if markers && child_has_expr && !child_is_spread_element {
                    child_nodes.push(ast.expression_string_literal(span, "<!--/-->", None));
                }
            }
            JSXChild::Fragment(fragment) => {
                let mut fragment_result = SSRResult::new();
                fragment_result.span = fragment.span;
                process_jsx_children(
                    &fragment.children,
                    &mut fragment_result,
                    do_not_escape,
                    context,
                    options,
                );

                let child_has_expr = !fragment_result.exprs.is_empty();
                let child_is_spread_element = fragment_result.spread_element;
                let mut child_expr =
                    template::create_template_expression(context, ast, &fragment_result, None);

                if child_has_expr && !do_not_escape && !child_is_spread_element {
                    child_expr = wrap_escape_expr(context, child_expr, false);
                }

                if markers && child_has_expr && !child_is_spread_element {
                    child_nodes.push(ast.expression_string_literal(span, "<!--$-->", None));
                }
                child_nodes.push(child_expr);
                if markers && child_has_expr && !child_is_spread_element {
                    child_nodes.push(ast.expression_string_literal(span, "<!--/-->", None));
                }
            }
        }
    }

    let child_nodes_len = child_nodes.len();
    let child_value = if child_nodes_len == 0 {
        ast.expression_identifier(span, "undefined")
    } else if child_nodes_len == 1 {
        child_nodes
            .pop()
            .unwrap_or_else(|| ast.expression_identifier(span, "undefined"))
    } else {
        let mut elements = ast.vec_with_capacity(child_nodes_len);
        for expr in child_nodes {
            elements.push(ArrayExpressionElement::from(expr));
        }
        ast.expression_array(span, elements)
    };

    let children_expr = if child_nodes_len > 0 && context.hydratable && options.hydratable {
        arrow_expr(ast, span, child_value)
    } else {
        child_value
    };

    // For spread, we generate: ssrElement("tag", props, children, needsHydrationKey)
    let callee = helper_ident_expr(ast, span, "ssrElement");
    let mut args = ast.vec();
    args.push(Argument::from(ast.expression_string_literal(
        span,
        ast.allocator.alloc_str(tag_name),
        None,
    )));
    args.push(Argument::from(props_expr));
    args.push(Argument::from(children_expr));
    args.push(Argument::from(ast.expression_boolean_literal(
        span,
        top_level && context.hydratable && options.hydratable,
    )));
    let call = ast.expression_call(
        span,
        callee,
        None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
        args,
        false,
    );
    result.append_expr(call);

    result
}

#[derive(Debug)]
struct NamespaceProperty<'a> {
    name: String,
    value: Expression<'a>,
    span: Span,
}

fn attribute_expression<'a>(
    attr: &JSXAttribute<'a>,
    context: &SSRContext<'a>,
) -> Option<(Expression<'a>, Span)> {
    let JSXAttributeValue::ExpressionContainer(container) = attr.value.as_ref()? else {
        return None;
    };

    container
        .expression
        .as_expression()
        .map(|expr| (context.clone_expr(expr), container.span))
}

fn namespace_property_value<'a>(
    attr: &JSXAttribute<'a>,
    context: &SSRContext<'a>,
) -> Option<(Expression<'a>, Span)> {
    let ast = context.ast();

    match &attr.value {
        Some(JSXAttributeValue::ExpressionContainer(container)) => container
            .expression
            .as_expression()
            .map(|expr| (context.clone_expr(expr), container.span)),
        Some(JSXAttributeValue::StringLiteral(lit)) => Some((
            ast.expression_string_literal(SPAN, ast.allocator.alloc_str(lit.value.as_str()), None),
            attr.span,
        )),
        None => Some((ast.expression_boolean_literal(SPAN, true), attr.span)),
        _ => None,
    }
}

fn namespace_object_property<'a>(
    context: &SSRContext<'a>,
    name: &str,
    value: Expression<'a>,
) -> ObjectPropertyKind<'a> {
    let ast = context.ast();
    let key = PropertyKey::StringLiteral(ast.alloc_string_literal(
        SPAN,
        ast.allocator.alloc_str(name),
        None,
    ));

    ast.object_property_kind_object_property(
        SPAN,
        PropertyKind::Init,
        key,
        value,
        false,
        false,
        false,
    )
}

fn build_namespace_object_expression<'a>(
    context: &SSRContext<'a>,
    props: &[NamespaceProperty<'a>],
) -> Expression<'a> {
    let ast = context.ast();
    let mut object_props = ast.vec_with_capacity(props.len());

    for prop in props {
        object_props.push(namespace_object_property(
            context,
            &prop.name,
            context.clone_expr(&prop.value),
        ));
    }

    ast.expression_object(SPAN, object_props)
}

fn merge_namespace_object_expression<'a>(
    context: &SSRContext<'a>,
    base_expr: &Expression<'a>,
    namespace_props: &[NamespaceProperty<'a>],
) -> Option<Expression<'a>> {
    let ast = context.ast();
    let Expression::ObjectExpression(object) = peel_wrapped_expression(base_expr) else {
        return None;
    };

    let mut object_props = ast.vec_with_capacity(object.properties.len() + namespace_props.len());
    for prop in &object.properties {
        object_props.push(prop.clone_in(ast.allocator));
    }
    for prop in namespace_props {
        object_props.push(namespace_object_property(
            context,
            &prop.name,
            context.clone_expr(&prop.value),
        ));
    }

    Some(ast.expression_object(SPAN, object_props))
}

fn first_namespace_span(props: &[NamespaceProperty<'_>]) -> Span {
    props.first().map(|prop| prop.span).unwrap_or(SPAN)
}

/// Transform element attributes for SSR
fn transform_attributes<'a>(
    element: &JSXElement<'a>,
    tag_name: &str,
    result: &mut SSRResult<'a>,
    context: &SSRContext<'a>,
    options: &TransformOptions<'a>,
) {
    let is_svg = is_svg_element(tag_name);

    let mut style_namespace_props: Vec<NamespaceProperty<'a>> = Vec::new();
    let mut class_namespace_props: Vec<NamespaceProperty<'a>> = Vec::new();
    let mut style_namespace_indices: Vec<usize> = Vec::new();
    let mut class_namespace_indices: Vec<usize> = Vec::new();
    let mut style_attr_index: Option<usize> = None;
    let mut class_attr_index: Option<usize> = None;
    let mut class_attr_indices: Vec<usize> = Vec::new();

    for (index, item) in element.opening_element.attributes.iter().enumerate() {
        let JSXAttributeItem::Attribute(attr) = item else {
            continue;
        };

        let key = get_attr_name(&attr.name);
        if style_attr_index.is_none() && key == "style" {
            style_attr_index = Some(index);
        }
        if key == "class" {
            class_attr_index = Some(index);
            class_attr_indices.push(index);
        }

        if let Some(prop_name) = key.strip_prefix("style:") {
            if let Some((value, span)) = namespace_property_value(attr, context) {
                style_namespace_indices.push(index);
                style_namespace_props.push(NamespaceProperty {
                    name: prop_name.to_string(),
                    value,
                    span,
                });
            }
            continue;
        }

        if let Some(prop_name) = key.strip_prefix("class:") {
            if let Some((value, span)) = namespace_property_value(attr, context) {
                class_namespace_indices.push(index);
                class_namespace_props.push(NamespaceProperty {
                    name: prop_name.to_string(),
                    value,
                    span,
                });
            }
        }
    }

    let first_style_namespace_index = style_namespace_indices.first().copied();
    let first_class_namespace_index = class_namespace_indices.first().copied();

    let mut merged_style_into_existing = false;
    let mut merged_class_into_existing = false;
    let mut emitted_synthetic_style = false;
    let mut emitted_synthetic_class = false;

    // Babel's normalizeAttributes(transformToObject) mutates NodePath arrays with stale indexes.
    // For mixed `class` + multiple `class:*` attributes this can remove the existing `class`
    // attribute before the merged object is emitted, leaving only later namespace attrs.
    // We mirror that behavior for fixture parity.
    let mut class_removed_by_stale_splice: Vec<usize> = Vec::new();
    let mut class_attr_removed_by_stale_splice = false;
    if let Some(existing_class_index) = class_attr_index {
        if !class_namespace_indices.is_empty() {
            let mut live_attribute_indices: Vec<usize> = element
                .opening_element
                .attributes
                .iter()
                .enumerate()
                .filter_map(|(idx, item)| {
                    matches!(item, JSXAttributeItem::Attribute(_)).then_some(idx)
                })
                .collect();

            for namespace_index in &class_namespace_indices {
                if *namespace_index < live_attribute_indices.len() {
                    let removed_original_index = live_attribute_indices.remove(*namespace_index);
                    class_removed_by_stale_splice.push(removed_original_index);
                }
            }

            class_attr_removed_by_stale_splice =
                class_removed_by_stale_splice.contains(&existing_class_index);
        }
    }

    for (index, item) in element.opening_element.attributes.iter().enumerate() {
        let JSXAttributeItem::Attribute(attr) = item else {
            continue;
        };
        let key = get_attr_name(&attr.name);

        if class_removed_by_stale_splice.contains(&index) {
            continue;
        }

        if key == "class" && class_attr_indices.len() > 1 && Some(index) != class_attr_index {
            continue;
        }

        if style_namespace_indices.contains(&index) {
            if style_attr_index.is_none()
                && !emitted_synthetic_style
                && Some(index) == first_style_namespace_index
                && !style_namespace_props.is_empty()
            {
                let merged_expr =
                    build_namespace_object_expression(context, &style_namespace_props);
                transform_expression_attribute(
                    "style",
                    &merged_expr,
                    first_namespace_span(&style_namespace_props),
                    result,
                    context,
                    options,
                    is_svg,
                );
                emitted_synthetic_style = true;
            }
            continue;
        }

        if class_namespace_indices.contains(&index) {
            if class_attr_removed_by_stale_splice {
                // Preserve remaining namespace attrs verbatim when stale index splicing
                // removed the existing `class` attribute.
            } else {
                if class_attr_index.is_none()
                    && !emitted_synthetic_class
                    && Some(index) == first_class_namespace_index
                    && !class_namespace_props.is_empty()
                {
                    let merged_expr =
                        build_namespace_object_expression(context, &class_namespace_props);
                    transform_expression_attribute(
                        "class",
                        &merged_expr,
                        first_namespace_span(&class_namespace_props),
                        result,
                        context,
                        options,
                        is_svg,
                    );
                    emitted_synthetic_class = true;
                }
                continue;
            }
        }

        if Some(index) == style_attr_index && !style_namespace_props.is_empty() {
            if let Some((expr, expr_span)) = attribute_expression(attr, context) {
                if let Some(merged_expr) =
                    merge_namespace_object_expression(context, &expr, &style_namespace_props)
                {
                    transform_expression_attribute(
                        "style",
                        &merged_expr,
                        expr_span,
                        result,
                        context,
                        options,
                        is_svg,
                    );
                    merged_style_into_existing = true;
                    continue;
                }
            }
        }

        if Some(index) == class_attr_index
            && !class_namespace_props.is_empty()
            && !class_attr_removed_by_stale_splice
        {
            if let Some((expr, expr_span)) = attribute_expression(attr, context) {
                if let Some(merged_expr) =
                    merge_namespace_object_expression(context, &expr, &class_namespace_props)
                {
                    transform_expression_attribute(
                        "class",
                        &merged_expr,
                        expr_span,
                        result,
                        context,
                        options,
                        is_svg,
                    );
                    merged_class_into_existing = true;
                    continue;
                }
            }
        }

        transform_attribute(attr, result, context, options, is_svg);
    }

    if !style_namespace_props.is_empty() && !merged_style_into_existing && !emitted_synthetic_style
    {
        let merged_expr = build_namespace_object_expression(context, &style_namespace_props);
        transform_expression_attribute(
            "style",
            &merged_expr,
            first_namespace_span(&style_namespace_props),
            result,
            context,
            options,
            is_svg,
        );
    }

    if !class_namespace_props.is_empty()
        && !merged_class_into_existing
        && !emitted_synthetic_class
        && !class_attr_removed_by_stale_splice
    {
        let merged_expr = build_namespace_object_expression(context, &class_namespace_props);
        transform_expression_attribute(
            "class",
            &merged_expr,
            first_namespace_span(&class_namespace_props),
            result,
            context,
            options,
            is_svg,
        );
    }
}

fn peel_wrapped_expression<'a>(expr: &'a Expression<'a>) -> &'a Expression<'a> {
    match expr {
        Expression::ParenthesizedExpression(paren) => peel_wrapped_expression(&paren.expression),
        Expression::TSAsExpression(ts) => peel_wrapped_expression(&ts.expression),
        Expression::TSSatisfiesExpression(ts) => peel_wrapped_expression(&ts.expression),
        Expression::TSNonNullExpression(ts) => peel_wrapped_expression(&ts.expression),
        Expression::TSTypeAssertion(ts) => peel_wrapped_expression(&ts.expression),
        _ => expr,
    }
}

fn static_expr_to_attr_string(expr: &Expression<'_>) -> Option<String> {
    let expr = peel_wrapped_expression(expr);
    match expr {
        Expression::StringLiteral(lit) => Some(lit.value.to_string()),
        Expression::NumericLiteral(_) => Some(expr_to_string(expr)),
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => template
            .quasis
            .first()
            .map(|q| q.value.raw.as_str().to_string()),
        _ => None,
    }
}

fn static_expr_to_text_string(expr: &Expression<'_>) -> Option<String> {
    let expr = peel_wrapped_expression(expr);
    match expr {
        Expression::StringLiteral(lit) => Some(lit.value.to_string()),
        Expression::NumericLiteral(_) => Some(expr_to_string(expr)),
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => template
            .quasis
            .first()
            .map(|q| q.value.raw.as_str().to_string()),
        _ => None,
    }
}

fn object_prop_name(prop: &oxc_ast::ast::ObjectProperty<'_>) -> Option<String> {
    if prop.computed {
        return None;
    }
    match &prop.key {
        PropertyKey::StaticIdentifier(ident) => Some(ident.name.as_str().to_string()),
        PropertyKey::StringLiteral(lit) => Some(lit.value.as_str().to_string()),
        _ => None,
    }
}

fn is_simple_static_member_expr(expr: &Expression<'_>) -> bool {
    match peel_wrapped_expression(expr) {
        Expression::StaticMemberExpression(member) => {
            matches!(
                peel_wrapped_expression(&member.object),
                Expression::Identifier(_)
            )
        }
        Expression::ComputedMemberExpression(member) => {
            matches!(
                peel_wrapped_expression(&member.object),
                Expression::Identifier(_)
            ) && matches!(
                peel_wrapped_expression(&member.expression),
                Expression::StringLiteral(_)
            )
        }
        _ => false,
    }
}

fn class_object_all_true_string(expr: &Expression<'_>) -> Option<String> {
    let Expression::ObjectExpression(object) = peel_wrapped_expression(expr) else {
        return None;
    };

    let mut keys = Vec::new();
    for property in &object.properties {
        let ObjectPropertyKind::ObjectProperty(prop) = property else {
            return None;
        };
        let Expression::BooleanLiteral(value) = peel_wrapped_expression(&prop.value) else {
            return None;
        };
        if !value.value {
            continue;
        }
        keys.push(object_prop_name(prop)?);
    }

    Some(keys.join("  "))
}

fn try_build_ssr_style_property_chain<'a>(
    context: &SSRContext<'a>,
    expr: &Expression<'a>,
) -> Option<Expression<'a>> {
    let ast = context.ast();
    let Expression::ObjectExpression(object) = peel_wrapped_expression(expr) else {
        return None;
    };

    if object
        .properties
        .iter()
        .any(|prop| matches!(prop, ObjectPropertyKind::SpreadProperty(_)))
    {
        return None;
    }

    let mut style_props: Vec<(String, Expression<'a>)> =
        Vec::with_capacity(object.properties.len());
    for prop in &object.properties {
        let ObjectPropertyKind::ObjectProperty(prop) = prop else {
            return None;
        };

        let name = match &prop.key {
            PropertyKey::StaticIdentifier(id) => id.name.to_string(),
            PropertyKey::StringLiteral(lit) => lit.value.to_string(),
            _ => return None,
        };

        let mut value = prop.value.clone_in(ast.allocator);
        *value.span_mut() = SPAN;
        style_props.push((name, value));
    }

    if style_props.is_empty() {
        return Some(ast.expression_string_literal(SPAN, "", None));
    }

    context.register_helper("ssrStyleProperty");

    let mut chained: Option<Expression<'a>> = None;
    for (index, (name, value)) in style_props.into_iter().enumerate() {
        let mut full_name = String::with_capacity(name.len() + 2);
        if index != 0 {
            full_name.push(';');
        }
        full_name.push_str(&name);
        full_name.push(':');

        let value_expr = wrap_escape_expr_with_options(context, value, true, true);

        let mut args = ast.vec();
        args.push(Argument::from(ast.expression_string_literal(
            SPAN,
            ast.allocator.alloc_str(&full_name),
            None,
        )));
        args.push(Argument::from(value_expr));

        let prop_expr = ast.expression_call(
            SPAN,
            helper_ident_expr(ast, SPAN, "ssrStyleProperty"),
            None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
            args,
            false,
        );

        chained = Some(match chained {
            Some(prev) => ast.expression_binary(SPAN, prev, BinaryOperator::Addition, prop_expr),
            None => prop_expr,
        });
    }

    Some(chained.unwrap_or_else(|| ast.expression_string_literal(SPAN, "", None)))
}

fn build_escape_call<'a>(
    context: &SSRContext<'a>,
    expr: Expression<'a>,
    is_attr: bool,
) -> Expression<'a> {
    let ast = context.ast();
    context.register_helper("escape");
    let mut args = ast.vec();
    args.push(Argument::from(expr));
    if is_attr {
        args.push(Argument::from(ast.expression_boolean_literal(SPAN, true)));
    }
    ast.expression_call(
        SPAN,
        helper_ident_expr(ast, SPAN, "escape"),
        None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
        args,
        false,
    )
}

fn wrap_escape_expr<'a>(
    context: &SSRContext<'a>,
    expr: Expression<'a>,
    is_attr: bool,
) -> Expression<'a> {
    wrap_escape_expr_with_options(context, expr, is_attr, false)
}

fn wrap_escape_expr_with_options<'a>(
    context: &SSRContext<'a>,
    expr: Expression<'a>,
    is_attr: bool,
    escape_literals: bool,
) -> Expression<'a> {
    let ast = context.ast();

    match expr {
        Expression::StringLiteral(lit) => {
            if escape_literals {
                let escaped = escape_html(lit.value.as_str(), is_attr);
                ast.expression_string_literal(SPAN, ast.allocator.alloc_str(&escaped), None)
            } else {
                Expression::StringLiteral(lit)
            }
        }
        Expression::NumericLiteral(number) => Expression::NumericLiteral(number),
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => {
            if escape_literals {
                let raw = template
                    .quasis
                    .first()
                    .map(|quasi| quasi.value.raw.as_str())
                    .unwrap_or("");
                let escaped = escape_html(raw, is_attr);
                ast.expression_string_literal(SPAN, ast.allocator.alloc_str(&escaped), None)
            } else {
                Expression::TemplateLiteral(template)
            }
        }
        Expression::FunctionExpression(function) => {
            let mut function = function.unbox();
            if let Some(body) = function.body.as_mut() {
                for statement in body.statements.iter_mut() {
                    if let Statement::ReturnStatement(return_stmt) = statement {
                        if let Some(argument) = return_stmt.argument.take() {
                            return_stmt.argument = Some(wrap_escape_expr_with_options(
                                context,
                                argument,
                                is_attr,
                                escape_literals,
                            ));
                        }
                    }
                }
            }
            Expression::FunctionExpression(ast.alloc(function))
        }
        Expression::ArrowFunctionExpression(arrow) => {
            let mut arrow = arrow.unbox();
            if arrow.expression {
                if let Some(Statement::ExpressionStatement(expr_stmt)) =
                    arrow.body.statements.first_mut()
                {
                    let current = std::mem::replace(
                        &mut expr_stmt.expression,
                        ast.expression_identifier(SPAN, "undefined"),
                    );
                    expr_stmt.expression =
                        wrap_escape_expr_with_options(context, current, is_attr, escape_literals);
                }
            } else {
                for statement in arrow.body.statements.iter_mut() {
                    if let Statement::ReturnStatement(return_stmt) = statement {
                        if let Some(argument) = return_stmt.argument.take() {
                            return_stmt.argument = Some(wrap_escape_expr_with_options(
                                context,
                                argument,
                                is_attr,
                                escape_literals,
                            ));
                        }
                    }
                }
            }
            Expression::ArrowFunctionExpression(ast.alloc(arrow))
        }
        Expression::TemplateLiteral(template) => {
            let mut template = template.unbox();
            let old_expressions = std::mem::replace(&mut template.expressions, ast.vec());
            let mut escaped_expressions = ast.vec_with_capacity(old_expressions.len());
            for expression in old_expressions {
                escaped_expressions.push(wrap_escape_expr_with_options(
                    context,
                    expression,
                    is_attr,
                    escape_literals,
                ));
            }
            template.expressions = escaped_expressions;
            Expression::TemplateLiteral(ast.alloc(template))
        }
        Expression::UnaryExpression(unary) => Expression::UnaryExpression(unary),
        Expression::BinaryExpression(binary) => {
            let mut binary = binary.unbox();
            binary.left =
                wrap_escape_expr_with_options(context, binary.left, is_attr, escape_literals);
            binary.right =
                wrap_escape_expr_with_options(context, binary.right, is_attr, escape_literals);
            Expression::BinaryExpression(ast.alloc(binary))
        }
        Expression::ConditionalExpression(conditional) => {
            let mut conditional = conditional.unbox();
            conditional.consequent = wrap_escape_expr_with_options(
                context,
                conditional.consequent,
                is_attr,
                escape_literals,
            );
            conditional.alternate = wrap_escape_expr_with_options(
                context,
                conditional.alternate,
                is_attr,
                escape_literals,
            );
            Expression::ConditionalExpression(ast.alloc(conditional))
        }
        Expression::LogicalExpression(logical) => {
            let mut logical = logical.unbox();
            logical.right =
                wrap_escape_expr_with_options(context, logical.right, is_attr, escape_literals);
            if logical.operator != LogicalOperator::And {
                logical.left =
                    wrap_escape_expr_with_options(context, logical.left, is_attr, escape_literals);
            }
            Expression::LogicalExpression(ast.alloc(logical))
        }
        Expression::CallExpression(call) => {
            let mut call = call.unbox();
            let callee = call.callee;
            match callee {
                Expression::FunctionExpression(function) => {
                    let mut function = function.unbox();
                    if let Some(body) = function.body.as_mut() {
                        for statement in body.statements.iter_mut() {
                            if let Statement::ReturnStatement(return_stmt) = statement {
                                if let Some(argument) = return_stmt.argument.take() {
                                    return_stmt.argument = Some(wrap_escape_expr_with_options(
                                        context,
                                        argument,
                                        is_attr,
                                        escape_literals,
                                    ));
                                }
                            }
                        }
                    }
                    call.callee = Expression::FunctionExpression(ast.alloc(function));
                    Expression::CallExpression(ast.alloc(call))
                }
                Expression::ArrowFunctionExpression(arrow) => {
                    let mut arrow = arrow.unbox();
                    if arrow.expression {
                        if let Some(Statement::ExpressionStatement(expr_stmt)) =
                            arrow.body.statements.first_mut()
                        {
                            let current = std::mem::replace(
                                &mut expr_stmt.expression,
                                ast.expression_identifier(SPAN, "undefined"),
                            );
                            expr_stmt.expression = wrap_escape_expr_with_options(
                                context,
                                current,
                                is_attr,
                                escape_literals,
                            );
                        }
                    } else {
                        for statement in arrow.body.statements.iter_mut() {
                            if let Statement::ReturnStatement(return_stmt) = statement {
                                if let Some(argument) = return_stmt.argument.take() {
                                    return_stmt.argument = Some(wrap_escape_expr_with_options(
                                        context,
                                        argument,
                                        is_attr,
                                        escape_literals,
                                    ));
                                }
                            }
                        }
                    }
                    call.callee = Expression::ArrowFunctionExpression(ast.alloc(arrow));
                    Expression::CallExpression(ast.alloc(call))
                }
                other => {
                    call.callee = other;
                    build_escape_call(
                        context,
                        Expression::CallExpression(ast.alloc(call)),
                        is_attr,
                    )
                }
            }
        }
        Expression::JSXElement(element) => {
            let tag = common::get_tag_name(&element);
            if !common::is_component(&tag) {
                Expression::JSXElement(element)
            } else {
                build_escape_call(context, Expression::JSXElement(element), is_attr)
            }
        }
        other => build_escape_call(context, other, is_attr),
    }
}

fn wrap_escape_text_with_space_fallback<'a>(
    context: &SSRContext<'a>,
    expr: Expression<'a>,
) -> Expression<'a> {
    let ast = context.ast();
    let escaped = wrap_escape_expr(context, expr, false);
    ast.expression_logical(
        SPAN,
        escaped,
        LogicalOperator::Or,
        ast.expression_string_literal(SPAN, " ", None),
    )
}

fn strip_reserved_namespace_prefix(key: &str) -> &str {
    if let Some((namespace, name)) = key.split_once(':') {
        if matches!(namespace, "class" | "on" | "style" | "use" | "prop") {
            return name;
        }
    }
    key
}

fn build_ssr_attribute_expr<'a>(
    context: &SSRContext<'a>,
    attr_name: &str,
    value: Expression<'a>,
) -> Expression<'a> {
    let ast = context.ast();
    context.register_helper("ssrAttribute");
    let mut args = ast.vec();
    args.push(Argument::from(ast.expression_string_literal(
        SPAN,
        ast.allocator.alloc_str(attr_name),
        None,
    )));
    args.push(Argument::from(value));
    ast.expression_call(
        SPAN,
        helper_ident_expr(ast, SPAN, "ssrAttribute"),
        None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
        args,
        false,
    )
}

fn transform_expression_attribute<'a>(
    key: &str,
    expr: &Expression<'a>,
    expr_span: Span,
    result: &mut SSRResult<'a>,
    context: &SSRContext<'a>,
    options: &TransformOptions<'a>,
    is_svg: bool,
) {
    let ast = context.ast();

    let stripped_key = strip_reserved_namespace_prefix(key);
    let attr_name = if is_svg {
        stripped_key.to_string()
    } else {
        ALIASES
            .get(stripped_key)
            .copied()
            .unwrap_or(stripped_key)
            .to_string()
    };

    let peeled = peel_wrapped_expression(expr);

    // Static boolean expression containers follow Babel SSR behavior:
    // true => bare attribute, false => omitted.
    if let Expression::BooleanLiteral(boolean_lit) = peeled {
        if boolean_lit.value {
            result.push_static(&format!(" {}", attr_name));
        }
        return;
    }

    // Static literal expression containers are inlined into template text.
    if let Some(static_attr) = static_expr_to_attr_string(expr) {
        let escaped = escape_html(&static_attr, true);
        if escaped.is_empty() {
            result.push_static(&format!(" {}", attr_name));
        } else {
            result.push_static(&format!(" {}=\"{}\"", attr_name, escaped));
        }
        return;
    }

    let has_static_marker = context.has_static_marker_comment(expr_span, options.static_marker);
    let dynamic_expr = !has_static_marker && is_dynamic(expr);
    let expr = if has_static_marker {
        context.clone_expr_without_trivia(expr)
    } else {
        context.clone_expr(expr)
    };

    // Handle special attributes
    if key == "style" {
        result.push_static(&format!(" {}=\"", attr_name));
        let mut style_expr =
            if let Some(style_expr) = try_build_ssr_style_property_chain(context, &expr) {
                style_expr
            } else {
                context.register_helper("ssrStyle");
                let callee = helper_ident_expr(ast, SPAN, "ssrStyle");
                let mut args = ast.vec();
                args.push(Argument::from(expr));
                ast.expression_call(
                    SPAN,
                    callee,
                    None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                    args,
                    false,
                )
            };
        if dynamic_expr {
            let params = ast.alloc_formal_parameters(
                SPAN,
                oxc_ast::ast::FormalParameterKind::ArrowFormalParameters,
                ast.vec(),
                NONE,
            );
            let mut statements = ast.vec_with_capacity(1);
            statements.push(Statement::ExpressionStatement(
                ast.alloc_expression_statement(SPAN, style_expr),
            ));
            let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
            let arrow = ast.expression_arrow_function(SPAN, true, false, NONE, params, NONE, body);
            style_expr = hoist_expression(context, result, arrow, true, false, false);
        }
        result.push_template_part("\"");
        result.push_template_value(style_expr);
    } else if key == "class" {
        result.push_static(" class=\"");
        let should_hoist_class = dynamic_expr && !is_simple_static_member_expr(&expr);
        let mut class_expr = if let Some(static_class) = class_object_all_true_string(&expr) {
            ast.expression_string_literal(SPAN, ast.allocator.alloc_str(&static_class), None)
        } else {
            context.register_helper("ssrClassName");
            let callee = helper_ident_expr(ast, SPAN, "ssrClassName");
            let mut args = ast.vec();
            args.push(Argument::from(expr));
            ast.expression_call(
                SPAN,
                callee,
                None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                args,
                false,
            )
        };
        if should_hoist_class {
            let params = ast.alloc_formal_parameters(
                SPAN,
                oxc_ast::ast::FormalParameterKind::ArrowFormalParameters,
                ast.vec(),
                NONE,
            );
            let mut statements = ast.vec_with_capacity(1);
            statements.push(Statement::ExpressionStatement(
                ast.alloc_expression_statement(SPAN, class_expr),
            ));
            let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
            let arrow = ast.expression_arrow_function(SPAN, true, false, NONE, params, NONE, body);
            class_expr = hoist_expression(context, result, arrow, false, false, true);
        }
        result.push_template_part("\"");
        result.push_template_value(class_expr);
    } else {
        // `void 0` should be forwarded directly to ssrAttribute without escaping.
        let is_void_expr = matches!(
            peel_wrapped_expression(&expr),
            Expression::UnaryExpression(unary)
                if unary.operator == UnaryOperator::Void
        );
        let escaped_or_raw = if is_void_expr {
            expr
        } else {
            wrap_escape_expr(context, expr, true)
        };

        // Generic dynamic attributes use ssrAttribute helper (including className/value/checked).
        let mut attr_expr = build_ssr_attribute_expr(context, &attr_name, escaped_or_raw);

        let post = key == "value" || key == "checked";

        // Dynamic attrs are hoisted through grouped ssrRunInScope calls for parity.
        if dynamic_expr {
            let params = ast.alloc_formal_parameters(
                SPAN,
                oxc_ast::ast::FormalParameterKind::ArrowFormalParameters,
                ast.vec(),
                NONE,
            );
            let mut statements = ast.vec_with_capacity(1);
            statements.push(Statement::ExpressionStatement(
                ast.alloc_expression_statement(SPAN, attr_expr),
            ));
            let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
            let arrow = ast.expression_arrow_function(SPAN, true, false, NONE, params, NONE, body);
            attr_expr = hoist_expression(context, result, arrow, true, post, false);
        }

        // Babel setAttr parity:
        // dynamic non-post: templateValues.push(expr)
        // dynamic post: templateValues.push(expr); template.push("")
        // static: templateValues.push(expr); template.push("")
        result.push_template_value(attr_expr);
        if post || !dynamic_expr {
            result.push_template_part("");
        }
    }
}

/// Transform a single attribute for SSR
fn transform_attribute<'a>(
    attr: &JSXAttribute<'a>,
    result: &mut SSRResult<'a>,
    context: &SSRContext<'a>,
    options: &TransformOptions<'a>,
    is_svg: bool,
) {
    let key = get_attr_name(&attr.name);

    // Skip client-only attributes
    if key == "ref" || key.starts_with("use:") || key.starts_with("prop:") {
        return;
    }

    // Handle child properties (innerHTML, textContent)
    if CHILD_PROPERTIES.contains(&*key) {
        // These are handled in children transform
        return;
    }

    // Get the attribute name (handle aliases like className -> class)
    let attr_name = if is_svg {
        key.to_string()
    } else {
        ALIASES.get(&*key).copied().unwrap_or(&*key).to_string()
    };

    match &attr.value {
        // Static string value
        Some(JSXAttributeValue::StringLiteral(lit)) => {
            let mut text = lit.value.to_string();
            if key == "style" || key == "class" {
                text = common::expression::trim_whitespace(&text).into_owned();
                if key == "style" {
                    text = text.replace("; ", ";").replace(": ", ":");
                }
            }

            let escaped = escape_html(&text, true);
            if escaped.is_empty() {
                result.push_static(&format!(" {}", attr_name));
            } else {
                result.push_static(&format!(" {}=\"{}\"", attr_name, escaped));
            }
        }

        // Dynamic value
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            if key.starts_with("on") {
                return;
            }

            if let Some(expr) = container.expression.as_expression() {
                transform_expression_attribute(
                    &key,
                    expr,
                    container.span,
                    result,
                    context,
                    options,
                    is_svg,
                );
            }
        }

        // Boolean attribute (no value)
        None => {
            result.push_static(&format!(" {}", attr_name));
        }

        _ => {}
    }
}

/// Transform element children for SSR
fn transform_children<'a>(
    element: &JSXElement<'a>,
    result: &mut SSRResult<'a>,
    skip_escape: bool,
    context: &SSRContext<'a>,
    options: &TransformOptions<'a>,
) {
    let ast = context.ast();

    // Check for innerHTML/textContent in attributes first
    for attr in &element.opening_element.attributes {
        if let JSXAttributeItem::Attribute(attr) = attr {
            let key = match &attr.name {
                JSXAttributeName::Identifier(id) => id.name.as_str(),
                _ => continue,
            };

            if key == "innerHTML" {
                if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
                    if let Some(expr) = container.expression.as_expression() {
                        if let Some(static_text) = static_expr_to_text_string(expr) {
                            result.push_static(&static_text);
                        } else {
                            // innerHTML - don't escape
                            result.push_dynamic(context.clone_expr(expr));
                        }
                        return;
                    }
                }
            } else if key == "textContent" || key == "innerText" {
                match &attr.value {
                    Some(JSXAttributeValue::StringLiteral(lit)) => {
                        result.push_static(&escape_html(&lit.value, false));
                        return;
                    }
                    Some(JSXAttributeValue::ExpressionContainer(container)) => {
                        if let Some(expr) = container.expression.as_expression() {
                            if let Some(static_text) = static_expr_to_text_string(expr) {
                                result.push_static(&escape_html(&static_text, false));
                            } else {
                                let has_static_marker = context.has_static_marker_comment(
                                    container.span,
                                    options.static_marker,
                                );
                                let dynamic_expr = !has_static_marker && is_dynamic(&expr);
                                let escaped = wrap_escape_text_with_space_fallback(
                                    context,
                                    if has_static_marker {
                                        context.clone_expr_without_trivia(expr)
                                    } else {
                                        context.clone_expr(expr)
                                    },
                                );
                                if dynamic_expr {
                                    let params = ast.alloc_formal_parameters(
                                        SPAN,
                                        oxc_ast::ast::FormalParameterKind::ArrowFormalParameters,
                                        ast.vec(),
                                        NONE,
                                    );
                                    let mut statements = ast.vec_with_capacity(1);
                                    statements.push(Statement::ExpressionStatement(
                                        ast.alloc_expression_statement(SPAN, escaped),
                                    ));
                                    let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
                                    let arrow = ast.expression_arrow_function(
                                        SPAN, true, false, NONE, params, NONE, body,
                                    );
                                    let hoisted = hoist_expression(
                                        context, result, arrow, false, false, false,
                                    );
                                    result.push_dynamic(hoisted);
                                } else {
                                    let hoisted = hoist_expression(
                                        context, result, escaped, false, false, true,
                                    );
                                    result.push_dynamic(hoisted);
                                }
                            }
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Process children
    process_jsx_children(&element.children, result, skip_escape, context, options);
}

/// Process a list of JSX children, appending to the result.
/// This is extracted as a helper to enable recursive processing of fragment children.
fn process_jsx_children<'a>(
    children: &oxc_allocator::Vec<'a, JSXChild<'a>>,
    result: &mut SSRResult<'a>,
    skip_escape: bool,
    context: &SSRContext<'a>,
    options: &TransformOptions<'a>,
) {
    let ast = context.ast();
    let filtered_children: Vec<&JSXChild<'a>> = children
        .iter()
        .filter(|child| !is_filtered_spread_child(child))
        .collect();
    let markers =
        context.hydratable && options.hydratable && spread_children_are_multi(&filtered_children);

    for child in filtered_children {
        match child {
            JSXChild::Text(text) => {
                let content = common::expression::trim_whitespace(&text.value);
                if !content.is_empty() {
                    if skip_escape {
                        result.push_static(&content);
                    } else {
                        result.push_static(&escape_html(&content, false));
                    }
                }
            }

            JSXChild::Element(child_elem) => {
                let child_tag = common::get_tag_name(child_elem);
                let child_result = if common::is_component(&child_tag) {
                    // Create a child transformer for nested components
                    let child_transformer = |child: &JSXChild<'a>| -> Option<SSRResult<'a>> {
                        match child {
                            JSXChild::Element(el) => {
                                let tag = common::get_tag_name(el);
                                Some(if common::is_component(&tag) {
                                    // For deeply nested components, use simple fallback
                                    context.register_helper("createComponent");
                                    context.register_helper("escape");
                                    let mut r = SSRResult::new();
                                    r.span = el.span;
                                    let callee = helper_ident_expr(ast, SPAN, "createComponent");
                                    let mut args = ast.vec();
                                    let tag_expr = ast
                                        .expression_identifier(SPAN, ast.allocator.alloc_str(&tag));
                                    args.push(Argument::from(tag_expr));
                                    args.push(Argument::from(
                                        ast.expression_object(SPAN, ast.vec()),
                                    ));
                                    let call = ast.expression_call(
                                        SPAN,
                                        callee,
                                        None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                                        args,
                                        false,
                                    );
                                    r.append_expr(call);
                                    r
                                } else {
                                    transform_element(el, &tag, false, context, options)
                                })
                            }
                            _ => None,
                        }
                    };
                    crate::component::transform_component(
                        child_elem,
                        &child_tag,
                        context,
                        options,
                        &child_transformer,
                    )
                } else {
                    transform_element(child_elem, &child_tag, false, context, options)
                };

                if child_result.exprs.is_empty() {
                    result.merge(child_result);
                } else {
                    let child_is_spread_element = child_result.spread_element;
                    let mut child_expr =
                        template::create_template_expression(context, ast, &child_result, None);
                    if !skip_escape && !child_is_spread_element {
                        child_expr = wrap_escape_expr(context, child_expr, false);
                    }
                    let hoisted =
                        hoist_expression(context, result, child_expr, false, false, false);
                    if markers && !child_is_spread_element {
                        result.push_static("<!--$-->");
                    }
                    result.push_dynamic(hoisted);
                    if markers && !child_is_spread_element {
                        result.push_static("<!--/-->");
                    }
                }
            }

            JSXChild::ExpressionContainer(container) => {
                if let Some(expr) = container.expression.as_expression() {
                    if let Some(static_text) = static_expr_to_text_string(expr) {
                        if skip_escape {
                            result.push_static(&static_text);
                        } else {
                            result.push_static(&escape_html(&static_text, false));
                        }
                    } else {
                        let has_static_marker = context
                            .has_static_marker_comment(container.span, options.static_marker);
                        let expr = if has_static_marker {
                            context.clone_expr_without_trivia(expr)
                        } else {
                            context.clone_expr(expr)
                        };
                        if skip_escape {
                            let slot_expr = if !has_static_marker && is_dynamic(&expr) {
                                arrow_expr(ast, SPAN, expr)
                            } else {
                                expr
                            };
                            let hoisted =
                                hoist_expression(context, result, slot_expr, false, false, false);
                            if markers {
                                result.push_static("<!--$-->");
                            }
                            result.push_dynamic(hoisted);
                            if markers {
                                result.push_static("<!--/-->");
                            }
                        } else {
                            // Normal content - escape
                            let dynamic_expr = !has_static_marker && is_dynamic(&expr);
                            let escaped = wrap_escape_expr(context, expr, false);
                            let slot_expr = if dynamic_expr {
                                arrow_expr(ast, SPAN, escaped)
                            } else {
                                escaped
                            };
                            let hoisted =
                                hoist_expression(context, result, slot_expr, false, false, false);
                            if markers {
                                result.push_static("<!--$-->");
                            }
                            result.push_dynamic(hoisted);
                            if markers {
                                result.push_static("<!--/-->");
                            }
                        }
                    }
                }
            }

            JSXChild::Spread(spread) => {
                let has_static_marker =
                    context.has_static_marker_comment(spread.span, options.static_marker);
                let expr = if has_static_marker {
                    context.clone_expr_without_trivia(&spread.expression)
                } else {
                    context.clone_expr(&spread.expression)
                };

                if skip_escape {
                    let slot_expr = if !has_static_marker && is_dynamic(&expr) {
                        arrow_expr(ast, SPAN, expr)
                    } else {
                        expr
                    };
                    let hoisted = hoist_expression(context, result, slot_expr, false, false, false);
                    if markers {
                        result.push_static("<!--$-->");
                    }
                    result.push_dynamic(hoisted);
                    if markers {
                        result.push_static("<!--/-->");
                    }
                } else {
                    let dynamic_expr = !has_static_marker && is_dynamic(&expr);
                    let escaped = wrap_escape_expr(context, expr, false);
                    let slot_expr = if dynamic_expr {
                        arrow_expr(ast, SPAN, escaped)
                    } else {
                        escaped
                    };
                    let hoisted = hoist_expression(context, result, slot_expr, false, false, false);
                    if markers {
                        result.push_static("<!--$-->");
                    }
                    result.push_dynamic(hoisted);
                    if markers {
                        result.push_static("<!--/-->");
                    }
                }
            }

            JSXChild::Fragment(fragment) => {
                // Recursively process fragment children with same escape settings
                process_jsx_children(&fragment.children, result, skip_escape, context, options);
            }
        }
    }
}
