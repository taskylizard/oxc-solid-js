use oxc_allocator::CloneIn;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ChainElement, Expression, FormalParameterKind, PropertyKind,
    Statement, VariableDeclarationKind,
};
use oxc_ast::{AstBuilder, NONE};
use oxc_span::{Span, SPAN};
use oxc_syntax::operator::{BinaryOperator, LogicalOperator, UnaryOperator};

use crate::ir::{
    helper_ident_expr, template_var_name, BlockContext, DynamicBinding, OutputKind, TransformResult,
};
use crate::output_helpers::{
    arrow_zero_params_body, call_expr, get_numbered_id, ident_expr, inline_effect_source_expr,
    static_member,
};
use crate::universal_output::build_universal_output_expr;

fn bool_cast_expr<'a>(ast: AstBuilder<'a>, span: Span, expr: Expression<'a>) -> Expression<'a> {
    let not_expr = ast.expression_unary(span, UnaryOperator::LogicalNot, expr);
    ast.expression_unary(span, UnaryOperator::LogicalNot, not_expr)
}

fn optional_static_member<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    object: Expression<'a>,
    property: &str,
) -> Expression<'a> {
    let prop = ast.identifier_name(span, ast.allocator.alloc_str(property));
    let member = ast.alloc_static_member_expression(span, object, prop, true);
    Expression::ChainExpression(
        ast.alloc_chain_expression(span, ChainElement::StaticMemberExpression(member)),
    )
}

fn is_math_ml_template(template: &str) -> bool {
    const MATHML_TAGS: [&str; 32] = [
        "annotation",
        "annotation-xml",
        "maction",
        "math",
        "merror",
        "mfrac",
        "mi",
        "mmultiscripts",
        "mn",
        "mo",
        "mover",
        "mpadded",
        "mphantom",
        "mprescripts",
        "mroot",
        "mrow",
        "ms",
        "mspace",
        "msqrt",
        "mstyle",
        "msub",
        "msubsup",
        "msup",
        "mtable",
        "mtd",
        "mtext",
        "mtr",
        "munder",
        "munderover",
        "semantics",
        "menclose",
        "mfenced",
    ];

    let trimmed = template.trim_start();
    let Some(rest) = trimmed.strip_prefix('<') else {
        return false;
    };

    let mut name = String::new();
    for ch in rest.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            name.push(ch);
        } else {
            break;
        }
    }

    if name.is_empty() {
        return false;
    }

    if !MATHML_TAGS.contains(&name.as_str()) {
        return false;
    }

    let suffix = &rest[name.len()..];
    let mut chars = suffix.chars();
    match chars.next() {
        Some('>') | Some(' ') | Some('\t') | Some('\n') | Some('\r') => true,
        _ => false,
    }
}

pub(crate) fn register_dynamic_binding_helper<'a>(
    context: &BlockContext<'a>,
    binding: &DynamicBinding<'a>,
) {
    if binding.key == "style" {
        context.register_helper("style");
        return;
    }

    if binding.key == "class" {
        context.register_helper("className");
        return;
    }

    if binding.key == "classList" {
        context.register_helper("classList");
        return;
    }

    if binding.key.starts_with("style:") {
        context.register_helper("setStyleProperty");
        return;
    }

    if binding.key.starts_with("class:") || binding.key.starts_with("prop:") {
        return;
    }

    if binding.key.starts_with("bool:") {
        context.register_helper("setBoolAttribute");
        return;
    }

    if binding.key == "textContent" || binding.key == "innerText" {
        if context.hydratable {
            context.register_helper("setProperty");
        }
        return;
    }

    if let Some((prefix, _)) = binding.key.split_once(':') {
        if common::constants::SVG_NAMESPACE.get(prefix).is_some() {
            context.register_helper("setAttributeNS");
            return;
        }
    }

    if common::constants::PROPERTIES.contains(binding.key.as_str()) {
        if context.hydratable {
            context.register_helper("setProperty");
        }
        return;
    }

    context.register_helper("setAttribute");
}

fn build_single_dynamic_effect_call<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    binding: &DynamicBinding<'a>,
    hydratable: bool,
) -> Expression<'a> {
    let value_name = "_v$";
    let prev_name = "_$p";
    let track_prev = binding.key == "class" || binding.key == "style";

    let mut callback_params = ast.vec_with_capacity(if track_prev { 2 } else { 1 });
    callback_params.push(ast.plain_formal_parameter(
        span,
        ast.binding_pattern_binding_identifier(span, ast.allocator.alloc_str(value_name)),
    ));
    if track_prev {
        callback_params.push(ast.plain_formal_parameter(
            span,
            ast.binding_pattern_binding_identifier(span, ast.allocator.alloc_str(prev_name)),
        ));
    }
    let callback_params = ast.alloc_formal_parameters(
        span,
        FormalParameterKind::ArrowFormalParameters,
        callback_params,
        NONE,
    );

    let setter = crate::template::generate_set_attr_expr_with_value(
        ast,
        span,
        binding,
        ident_expr(ast, span, value_name),
        if track_prev {
            Some(ident_expr(ast, span, prev_name))
        } else {
            None
        },
        hydratable,
    );
    let mut callback_statements = ast.vec_with_capacity(1);
    callback_statements.push(Statement::ExpressionStatement(
        ast.alloc_expression_statement(span, setter),
    ));
    let callback_body = ast.alloc_function_body(span, ast.vec(), callback_statements);
    let callback = ast.expression_arrow_function(
        span,
        false,
        false,
        NONE,
        callback_params,
        NONE,
        callback_body,
    );

    let effect = helper_ident_expr(ast, span, "effect");
    let source_value = if binding.key.starts_with("class:")
        && !matches!(
            binding.value,
            Expression::BooleanLiteral(_) | Expression::UnaryExpression(_)
        ) {
        bool_cast_expr(ast, span, binding.value.clone_in(ast.allocator))
    } else {
        binding.value.clone_in(ast.allocator)
    };
    let source = inline_effect_source_expr(ast, span, &source_value);
    call_expr(ast, span, effect, [source, callback])
}

fn build_multi_dynamic_effect_call<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    dynamics: &[DynamicBinding<'a>],
    hydratable: bool,
) -> Expression<'a> {
    let prev_name = "_p$";
    let ids: Vec<String> = (0..dynamics.len()).map(get_numbered_id).collect();

    let mut value_props = ast.vec_with_capacity(dynamics.len());
    let mut current_props = ast.vec_with_capacity(dynamics.len());
    let mut update_statements = ast.vec_with_capacity(dynamics.len());

    for (binding, id) in dynamics.iter().zip(ids.iter()) {
        let value_expr = if binding.key.starts_with("class:")
            && !matches!(
                binding.value,
                Expression::BooleanLiteral(_) | Expression::UnaryExpression(_)
            ) {
            bool_cast_expr(ast, span, binding.value.clone_in(ast.allocator))
        } else {
            binding.value.clone_in(ast.allocator)
        };

        let key = ast.property_key_static_identifier(span, ast.allocator.alloc_str(id));
        value_props.push(ast.object_property_kind_object_property(
            span,
            PropertyKind::Init,
            key,
            value_expr,
            false,
            false,
            false,
        ));

        let current_key = ast.property_key_static_identifier(span, ast.allocator.alloc_str(id));
        let current_value =
            ast.binding_pattern_binding_identifier(span, ast.allocator.alloc_str(id));
        current_props.push(ast.binding_property(span, current_key, current_value, true, false));

        let current = ident_expr(ast, span, id);
        let prev = optional_static_member(ast, span, ident_expr(ast, span, prev_name), id);
        let always_set = binding.key == "class" || binding.key == "style";

        let setter = crate::template::generate_set_attr_expr_with_value(
            ast,
            span,
            binding,
            current.clone_in(ast.allocator),
            if always_set {
                Some(prev.clone_in(ast.allocator))
            } else {
                None
            },
            hydratable,
        );

        let update = if always_set {
            setter
        } else {
            let changed =
                ast.expression_binary(span, current, BinaryOperator::StrictInequality, prev);
            ast.expression_logical(span, changed, LogicalOperator::And, setter)
        };

        update_statements.push(Statement::ExpressionStatement(
            ast.alloc_expression_statement(span, update),
        ));
    }

    let values = ast.expression_object(span, value_props);
    let getter = arrow_zero_params_body(ast, span, values);

    let mut callback_params = ast.vec_with_capacity(2);
    let current_pattern = ast.binding_pattern_object_pattern(span, current_props, NONE);
    callback_params.push(ast.plain_formal_parameter(span, current_pattern));
    callback_params.push(ast.plain_formal_parameter(
        span,
        ast.binding_pattern_binding_identifier(span, ast.allocator.alloc_str(prev_name)),
    ));
    let callback_params = ast.alloc_formal_parameters(
        span,
        FormalParameterKind::ArrowFormalParameters,
        callback_params,
        NONE,
    );
    let callback_body = ast.alloc_function_body(span, ast.vec(), update_statements);
    let callback = ast.expression_arrow_function(
        span,
        false,
        false,
        NONE,
        callback_params,
        NONE,
        callback_body,
    );

    let effect = helper_ident_expr(ast, span, "effect");
    call_expr(ast, span, effect, [getter, callback])
}

pub fn build_dom_output_expr<'a>(
    result: &TransformResult<'a>,
    context: &BlockContext<'a>,
) -> Expression<'a> {
    let ast = context.ast();
    let gen_span = SPAN;

    // Fragment with mixed children (array output)
    if !result.child_results.is_empty() {
        let mut elements = ast.vec_with_capacity(result.child_results.len());
        for child in &result.child_results {
            let expr = match child.output_kind {
                OutputKind::Universal => build_universal_output_expr(child, context),
                OutputKind::Dom => build_dom_output_expr(child, context),
            };
            elements.push(ArrayExpressionElement::from(expr));
        }
        return ast.expression_array(gen_span, elements);
    }

    // Text-only result
    if result.text && !result.template.is_empty() {
        return ast.expression_string_literal(
            gen_span,
            ast.allocator.alloc_str(&result.template),
            None,
        );
    }

    // Template-backed result
    if !result.template.is_empty() {
        // Push template and get variable name (unless we are skipping templates for hydration)
        // The template string is generated code; don't attribute it to the source with spans.
        let tmpl_var = if !result.skip_template {
            let use_import_node = result.has_custom_element || result.is_import_node;
            let is_math_ml = is_math_ml_template(&result.template);
            let validation_template = if result.template_with_closing_tags.is_empty() {
                result.template.clone()
            } else {
                result.template_with_closing_tags.clone()
            };
            let tmpl_idx = context.push_template(
                result.template.clone(),
                validation_template,
                result.template_is_svg,
                use_import_node,
                is_math_ml,
                gen_span,
            );
            Some(template_var_name(tmpl_idx))
        } else {
            None
        };

        // Static templates should bypass IIFE wrapping when there is no runtime work.
        // Note: `result.declarations` can still contain internal walker bookkeeping that is
        // unused once everything folds into static template HTML, so declarations alone should
        // not force an IIFE.
        let has_no_runtime_work = result.statements.is_empty()
            && result.exprs.is_empty()
            && result.dynamics.is_empty()
            && result.post_exprs.is_empty();

        if has_no_runtime_work {
            if context.hydratable {
                context.register_helper("getNextElement");
                let callee = helper_ident_expr(ast, gen_span, "getNextElement");
                let args = if let Some(tmpl_var) = &tmpl_var {
                    vec![ident_expr(ast, gen_span, tmpl_var)]
                } else {
                    vec![]
                };
                return call_expr(ast, gen_span, callee, args);
            }

            if !result.skip_template {
                let tmpl_var = tmpl_var.expect("template var required for static template output");
                return call_expr(ast, gen_span, ident_expr(ast, gen_span, &tmpl_var), []);
            }
        }

        // Use the generated element ID when available (matches expression wiring).
        // Fall back to a local _el$ when the element didn't require a stable ID.
        let elem_var = result.id.clone().unwrap_or_else(|| "_el$".to_string());

        let elem_init = if context.hydratable {
            context.register_helper("getNextElement");
            let callee = helper_ident_expr(ast, gen_span, "getNextElement");
            let args = if let Some(tmpl_var) = &tmpl_var {
                vec![ident_expr(ast, gen_span, tmpl_var)]
            } else {
                vec![]
            };
            call_expr(ast, gen_span, callee, args)
        } else {
            let tmpl_var = tmpl_var.expect("template var required for non-hydratable output");
            call_expr(ast, gen_span, ident_expr(ast, gen_span, &tmpl_var), [])
        };

        let mut statements = ast.vec();

        let mut declarators = ast.vec_with_capacity(1 + result.declarations.len());
        declarators.push(ast.variable_declarator(
            gen_span,
            VariableDeclarationKind::Var,
            ast.binding_pattern_binding_identifier(gen_span, ast.allocator.alloc_str(&elem_var)),
            NONE,
            Some(elem_init),
            false,
        ));
        for decl in &result.declarations {
            declarators.push(ast.variable_declarator(
                gen_span,
                VariableDeclarationKind::Var,
                decl.pattern.clone_in(ast.allocator),
                NONE,
                Some(decl.init.clone_in(ast.allocator)),
                false,
            ));
        }
        statements.push(Statement::VariableDeclaration(
            ast.alloc_variable_declaration(
                gen_span,
                VariableDeclarationKind::Var,
                declarators,
                false,
            ),
        ));

        // Additional statements emitted by transforms (e.g., ref temporaries).
        for stmt in &result.statements {
            statements.push(stmt.clone_in(ast.allocator));
        }

        // Expressions (effects, inserts, etc.)
        for expr in &result.exprs {
            statements.push(Statement::ExpressionStatement(
                ast.alloc_expression_statement(gen_span, expr.clone_in(ast.allocator)),
            ));
        }

        // Dynamic bindings.
        if !result.dynamics.is_empty() {
            if context.effect_wrapper_enabled {
                // Default mode: batched effect wrapper parity with Babel wrapDynamics.
                context.register_helper("effect");
                for binding in &result.dynamics {
                    register_dynamic_binding_helper(context, binding);
                }

                let effect_call = if result.dynamics.len() == 1 {
                    build_single_dynamic_effect_call(
                        ast,
                        gen_span,
                        &result.dynamics[0],
                        context.hydratable,
                    )
                } else {
                    build_multi_dynamic_effect_call(
                        ast,
                        gen_span,
                        &result.dynamics,
                        context.hydratable,
                    )
                };

                statements.push(Statement::ExpressionStatement(
                    ast.alloc_expression_statement(gen_span, effect_call),
                ));
            } else {
                // Wrapperless mode: emit direct dynamic setter expressions without effect().
                for binding in &result.dynamics {
                    register_dynamic_binding_helper(context, binding);
                    let value_expr = if binding.key.starts_with("class:")
                        && !matches!(
                            binding.value,
                            Expression::BooleanLiteral(_) | Expression::UnaryExpression(_)
                        ) {
                        bool_cast_expr(ast, gen_span, binding.value.clone_in(ast.allocator))
                    } else {
                        binding.value.clone_in(ast.allocator)
                    };

                    let setter = crate::template::generate_set_attr_expr_with_value(
                        ast,
                        gen_span,
                        binding,
                        value_expr,
                        None,
                        context.hydratable,
                    );
                    statements.push(Statement::ExpressionStatement(
                        ast.alloc_expression_statement(gen_span, setter),
                    ));
                }
            }
        }

        // Post expressions
        for expr in &result.post_exprs {
            statements.push(Statement::ExpressionStatement(
                ast.alloc_expression_statement(gen_span, expr.clone_in(ast.allocator)),
            ));
        }

        // return _el$;
        statements.push(Statement::ReturnStatement(ast.alloc_return_statement(
            gen_span,
            Some(ident_expr(ast, gen_span, &elem_var)),
        )));

        // (() => { ... })()
        let params = ast.alloc_formal_parameters(
            gen_span,
            FormalParameterKind::ArrowFormalParameters,
            ast.vec(),
            NONE,
        );
        let body = ast.alloc_function_body(gen_span, ast.vec(), statements);
        let arrow_fn =
            ast.expression_arrow_function(gen_span, false, false, NONE, params, NONE, body);
        return call_expr(ast, gen_span, arrow_fn, []);
    }

    // Expression-only result (like createComponent(...) or fragment expression)
    if !result.exprs.is_empty() {
        if result.needs_memo {
            context.register_helper("memo");
            let callee = helper_ident_expr(ast, gen_span, "memo");
            let mut args = ast.vec_with_capacity(result.exprs.len());
            for expr in &result.exprs {
                args.push(Argument::from(expr.clone_in(ast.allocator)));
            }
            return ast.expression_call(
                gen_span,
                callee,
                None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                args,
                false,
            );
        }

        if result.exprs.len() == 1 {
            return result.exprs[0].clone_in(ast.allocator);
        }

        let mut exprs = ast.vec_with_capacity(result.exprs.len());
        for expr in &result.exprs {
            exprs.push(expr.clone_in(ast.allocator));
        }
        return ast.expression_sequence(gen_span, exprs);
    }

    // Fallback: empty string literal (matches previous parse-fallback behavior for empty output)
    ast.expression_string_literal(gen_span, ast.allocator.alloc_str(""), None)
}
