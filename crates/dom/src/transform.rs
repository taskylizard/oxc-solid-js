//! Main JSX transform logic
//! This implements the Traverse trait to walk the AST and transform JSX

use oxc_allocator::{Allocator, CloneIn};
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionExpression, BindingPattern, CallExpression,
    Class, ClassElement, Expression, FormalParameterKind, Function, JSXChild, JSXElement,
    JSXElementName, JSXExpressionContainer, JSXFragment, JSXText, ObjectProperty, Program,
    PropertyKind, Statement, SwitchCase, TemplateElementValue, VariableDeclarationKind,
    VariableDeclarator,
};
use oxc_ast::NONE;
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::SPAN;
use oxc_syntax::scope::ScopeFlags;
use oxc_traverse::{Ancestor, Traverse, TraverseCtx};

use std::collections::HashSet;

use common::{
    build_named_value_import_statement, collect_value_import_local_names, get_tag_name,
    is_component, prepend_program_statements, traverse_program_with_semantic, GenerateMode,
    TransformOptions,
};

use crate::component::transform_component;
use crate::conditional::{is_condition_expression, transform_condition_inline_expr};
use crate::element::{evaluate_static_text_expression, transform_element};
use crate::ir::{
    helper_ident_expr, helper_local_name, template_var_name, BlockContext, TransformResult,
};
use crate::output::build_dom_output_expr;
use crate::universal_element::transform_element as transform_universal_element;
use crate::universal_output::build_universal_output_expr;
use crate::validate::is_invalid_markup;

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

fn memo_wrapper_enabled(options: &TransformOptions<'_>) -> bool {
    !options.memo_wrapper.is_empty()
}

fn jsx_element_name_is_component(name: &JSXElementName<'_>) -> bool {
    match name {
        JSXElementName::Identifier(id) => is_component(id.name.as_str()),
        JSXElementName::IdentifierReference(id) => is_component(id.name.as_str()),
        JSXElementName::MemberExpression(_) | JSXElementName::ThisExpression(_) => true,
        JSXElementName::NamespacedName(_) => false,
    }
}

fn enclosing_jsx_context_is_component(ctx: &TraverseCtx<'_, ()>) -> Option<bool> {
    for ancestor in ctx.ancestors() {
        match ancestor {
            Ancestor::JSXOpeningElementAttributes(opening) => {
                return Some(jsx_element_name_is_component(opening.name()));
            }
            Ancestor::JSXElementChildren(element) => {
                return Some(jsx_element_name_is_component(
                    &element.opening_element().name,
                ));
            }
            _ => {}
        }
    }

    None
}

fn should_defer_nested_attribute_jsx(ctx: &TraverseCtx<'_, ()>) -> bool {
    let inside_jsx_expression = ctx
        .ancestors()
        .any(|ancestor| ancestor.is_parent_of_jsx_expression());
    if !inside_jsx_expression {
        return false;
    }

    !matches!(enclosing_jsx_context_is_component(ctx), Some(true))
}

struct DeferredJsxResolver<'tr, 'ctx, 'a> {
    transform: &'tr SolidTransform<'a>,
    ctx: &'ctx TraverseCtx<'a, ()>,
}

impl<'a, 'tr, 'ctx> VisitMut<'a> for DeferredJsxResolver<'tr, 'ctx, 'a> {
    fn visit_expression(&mut self, node: &mut Expression<'a>) {
        match node {
            Expression::JSXElement(element) => {
                let result = self.transform.transform_jsx_element(
                    element,
                    &TransformInfo {
                        top_level: true,
                        last_element: true,
                        ..Default::default()
                    },
                    self.ctx,
                );
                *node = self.transform.build_output_expr(&result);
                walk_mut::walk_expression(self, node);
            }
            Expression::JSXFragment(fragment) => {
                let result = self.transform.transform_fragment(
                    fragment,
                    &TransformInfo {
                        top_level: true,
                        ..Default::default()
                    },
                    self.ctx,
                );
                *node = self.transform.build_output_expr(&result);
                walk_mut::walk_expression(self, node);
            }
            _ => walk_mut::walk_expression(self, node),
        }
    }
}

/// The main Solid JSX transformer
pub struct SolidTransform<'a> {
    allocator: &'a Allocator,
    options: &'a TransformOptions<'a>,
    context: BlockContext<'a>,
}

impl<'a> SolidTransform<'a> {
    pub fn new(
        allocator: &'a Allocator,
        options: &'a TransformOptions<'a>,
        source_text: &'a str,
    ) -> Self {
        let dom_module_name = if matches!(options.generate, GenerateMode::Dynamic) {
            options
                .dynamic_dom_renderer_module_name()
                .unwrap_or(options.module_name)
        } else {
            options.module_name
        };

        Self {
            allocator,
            options,
            context: BlockContext::new(
                allocator,
                options.hydratable,
                source_text,
                !options.effect_wrapper.is_empty(),
                options.module_name,
                dom_module_name,
                options.module_name,
            ),
        }
    }

    /// Run the transform on a program
    pub fn transform(mut self, program: &mut Program<'a>) {
        let allocator = self.allocator;
        traverse_program_with_semantic(&mut self, allocator, program);
    }

    /// Transform a JSX node and return the result
    fn transform_node(
        &self,
        node: &JSXChild<'a>,
        info: &TransformInfo,
        ctx: &TraverseCtx<'a, ()>,
    ) -> Option<TransformResult<'a>> {
        match node {
            JSXChild::Element(element) => Some(self.transform_jsx_element(element, info, ctx)),
            JSXChild::Fragment(fragment) => Some(self.transform_fragment(fragment, info, ctx)),
            JSXChild::Text(text) => self.transform_text(text, info),
            JSXChild::ExpressionContainer(container) => {
                self.transform_expression_container(container, info)
            }
            JSXChild::Spread(spread) => {
                let expr = &spread.expression;
                if common::is_dynamic(expr) {
                    let ast = self.context.ast();
                    let wrapped = if let Expression::CallExpression(call) = expr {
                        if call.arguments.is_empty()
                            && !matches!(
                                call.callee,
                                Expression::CallExpression(_)
                                    | Expression::StaticMemberExpression(_)
                                    | Expression::ComputedMemberExpression(_)
                            )
                        {
                            self.context.clone_expr(&call.callee)
                        } else {
                            let params = ast.alloc_formal_parameters(
                                SPAN,
                                FormalParameterKind::ArrowFormalParameters,
                                ast.vec(),
                                NONE,
                            );
                            let mut statements = ast.vec_with_capacity(1);
                            statements.push(Statement::ExpressionStatement(
                                ast.alloc_expression_statement(
                                    SPAN,
                                    self.context.clone_expr(&spread.expression),
                                ),
                            ));
                            let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
                            ast.expression_arrow_function(
                                SPAN, true, false, NONE, params, NONE, body,
                            )
                        }
                    } else {
                        let params = ast.alloc_formal_parameters(
                            SPAN,
                            FormalParameterKind::ArrowFormalParameters,
                            ast.vec(),
                            NONE,
                        );
                        let mut statements = ast.vec_with_capacity(1);
                        statements.push(Statement::ExpressionStatement(
                            ast.alloc_expression_statement(SPAN, self.context.clone_expr(expr)),
                        ));
                        let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
                        ast.expression_arrow_function(SPAN, true, false, NONE, params, NONE, body)
                    };

                    Some(TransformResult {
                        span: spread.span,
                        exprs: vec![wrapped],
                        ..Default::default()
                    })
                } else {
                    Some(TransformResult {
                        span: spread.span,
                        exprs: vec![self.context.clone_expr(expr)],
                        ..Default::default()
                    })
                }
            }
        }
    }

    /// Transform a JSX element
    fn transform_jsx_element(
        &self,
        element: &JSXElement<'a>,
        info: &TransformInfo,
        ctx: &TraverseCtx<'a, ()>,
    ) -> TransformResult<'a> {
        let tag_name = get_tag_name(element);

        // Create child transformer closure that can recursively transform children
        let child_transformer = |child: &JSXChild<'a>| -> Option<TransformResult<'a>> {
            self.transform_node(child, info, ctx)
        };

        if is_component(&tag_name) {
            transform_component(
                element,
                &tag_name,
                &self.context,
                self.options,
                &child_transformer,
                ctx,
            )
        } else if self.options.should_use_universal_for_intrinsic(&tag_name) {
            transform_universal_element(
                element,
                &tag_name,
                info,
                &self.context,
                self.options,
                &child_transformer,
                ctx,
            )
        } else {
            transform_element(
                element,
                &tag_name,
                info,
                &self.context,
                self.options,
                &child_transformer,
                ctx,
            )
        }
    }

    fn build_output_expr(&self, result: &TransformResult<'a>) -> Expression<'a> {
        match self.options.generate {
            GenerateMode::Universal => build_universal_output_expr(result, &self.context),
            GenerateMode::Dynamic if result.uses_universal_output() => {
                build_universal_output_expr(result, &self.context)
            }
            _ => build_dom_output_expr(result, &self.context),
        }
    }

    fn resolve_deferred_jsx_in_expression(
        &self,
        expr: &mut Expression<'a>,
        ctx: &TraverseCtx<'a, ()>,
    ) {
        let mut resolver = DeferredJsxResolver {
            transform: self,
            ctx,
        };
        resolver.visit_expression(expr);
    }

    /// Transform a JSX fragment
    fn transform_fragment(
        &self,
        fragment: &JSXFragment<'a>,
        info: &TransformInfo,
        ctx: &TraverseCtx<'a, ()>,
    ) -> TransformResult<'a> {
        let ast = self.context.ast();
        let mut result = TransformResult {
            span: fragment.span,
            ..Default::default()
        };
        let mut child_results: Vec<TransformResult<'a>> = Vec::new();
        let mut dynamic_expression_children: Vec<bool> = Vec::new();

        for child in &fragment.children {
            // Babel fragment semantics: each child is transformed as an independent
            // top-level + last-element unit.
            let fragment_child_info = TransformInfo {
                top_level: true,
                last_element: true,
                fragment_child: true,
                forced_id: None,
                ..info.clone()
            };

            if let Some(child_result) = self.transform_node(child, &fragment_child_info, ctx) {
                let is_dynamic_expression_child = match child {
                    JSXChild::ExpressionContainer(container) => container
                        .expression
                        .as_expression()
                        .is_some_and(common::is_dynamic),
                    JSXChild::Spread(spread) => common::is_dynamic(&spread.expression),
                    _ => false,
                };

                dynamic_expression_children.push(is_dynamic_expression_child);
                child_results.push(child_result);
            }
        }

        // Handle different fragment scenarios
        if child_results.is_empty() {
            // Empty fragment
            result.exprs.push(ast.expression_array(SPAN, ast.vec()));
            return result;
        }

        if child_results.len() == 1 {
            let mut single_result = child_results.pop().unwrap();
            let should_memo = dynamic_expression_children.pop().unwrap_or(false);

            if memo_wrapper_enabled(self.options)
                && should_memo
                && single_result.template.is_empty()
                && !single_result.exprs.is_empty()
            {
                single_result.needs_memo = true;
            }
            return single_result;
        }

        let has_hydratable_event = child_results.iter().any(|r| r.has_hydratable_event);

        // Multiple children:
        // `template()` only returns the first root node, so fragments with more than one root
        // must be emitted as arrays of child outputs.
        //
        // The only safe merge is for plain text, which can be concatenated into a single
        // string expression.
        let all_text_children = child_results.iter().all(|r| r.text);
        if all_text_children {
            result.text = true;
            for child_result in child_results {
                result.template.push_str(&child_result.template);
            }
        } else {
            for (child_result, should_memo) in child_results
                .iter_mut()
                .zip(dynamic_expression_children.into_iter())
            {
                if memo_wrapper_enabled(self.options)
                    && should_memo
                    && child_result.template.is_empty()
                    && !child_result.exprs.is_empty()
                {
                    child_result.needs_memo = true;
                }
            }
            result.child_results = child_results;
        }

        result.has_hydratable_event = has_hydratable_event;
        result
    }

    /// Transform JSX text.
    ///
    /// Native-element template text preserves authored entities (`&nbsp;`) in template HTML.
    /// Fragment/component string-literal children decode entities (`&nbsp;` -> `\u{00A0}`).
    fn transform_text(
        &self,
        text: &JSXText<'a>,
        info: &TransformInfo,
    ) -> Option<TransformResult<'a>> {
        let content = common::expression::normalize_jsx_text(text);
        if content.is_empty() {
            return None;
        }

        let template = if info.component_child || info.fragment_child {
            common::expression::decode_html_entities(&content)
        } else {
            content.into_owned()
        };

        Some(TransformResult {
            span: text.span,
            template,
            text: true,
            ..Default::default()
        })
    }

    /// Transform a JSX expression container
    fn transform_expression_container(
        &self,
        container: &JSXExpressionContainer<'a>,
        info: &TransformInfo,
    ) -> Option<TransformResult<'a>> {
        // Use as_expression() to get the expression if it exists
        if let Some(expr) = container.expression.as_expression() {
            if common::is_dynamic(expr) {
                // Match Babel behavior: normalize simple getter calls like `value()` to `value`
                // so fragment memo wrapping becomes `memo(value)` rather than
                // `memo(() => value())`.
                //
                // This normalization should not run for component children.
                if !info.component_child {
                    if let Expression::CallExpression(call) = expr {
                        if call.arguments.is_empty()
                            && !matches!(
                                call.callee,
                                Expression::CallExpression(_)
                                    | Expression::StaticMemberExpression(_)
                                    | Expression::ComputedMemberExpression(_)
                            )
                        {
                            return Some(TransformResult {
                                span: container.span,
                                exprs: vec![self.context.clone_expr(&call.callee)],
                                ..Default::default()
                            });
                        }
                    }
                }

                // Wrap in arrow function for reactivity
                let ast = self.context.ast();
                let span = SPAN;
                let wrapped_expr = if self.options.wrap_conditionals
                    && memo_wrapper_enabled(self.options)
                    && is_condition_expression(expr)
                {
                    transform_condition_inline_expr(self.context.clone_expr(expr), &self.context)
                } else {
                    self.context.clone_expr(expr)
                };

                let params = ast.alloc_formal_parameters(
                    span,
                    oxc_ast::ast::FormalParameterKind::ArrowFormalParameters,
                    ast.vec(),
                    NONE,
                );
                let mut statements = ast.vec_with_capacity(1);
                statements.push(Statement::ExpressionStatement(
                    ast.alloc_expression_statement(span, wrapped_expr),
                ));
                let body = ast.alloc_function_body(span, ast.vec(), statements);
                let arrow =
                    ast.expression_arrow_function(span, true, false, NONE, params, NONE, body);
                Some(TransformResult {
                    span: container.span,
                    exprs: vec![arrow],
                    ..Default::default()
                })
            } else {
                // Static expression
                Some(TransformResult {
                    span: container.span,
                    exprs: vec![self.context.clone_expr(expr)],
                    ..Default::default()
                })
            }
        } else {
            // Empty expression
            None
        }
    }
}

/// Additional info passed during transform
#[derive(Default, Clone)]
pub struct TransformInfo {
    pub top_level: bool,
    pub last_element: bool,
    pub skip_id: bool,
    pub component_child: bool,
    pub fragment_child: bool,
    /// Path from root element to this element (e.g., ["firstChild", "nextSibling"])
    pub path: Vec<String>,
    /// The root element variable name (e.g., "_el$1")
    pub root_id: Option<String>,
    /// Preallocated element id for walker ordering parity.
    pub forced_id: Option<String>,
    /// Tags that should stay closed while walking nested omission rules
    pub to_be_closed: Option<HashSet<String>>,
    /// When hydrating <html>, use getNextMatch for this element tag
    pub match_tag: Option<String>,
}

fn peel_wrapped_expression<'a, 'b>(expr: &'b Expression<'a>) -> &'b Expression<'a> {
    match expr {
        Expression::ParenthesizedExpression(paren) => peel_wrapped_expression(&paren.expression),
        Expression::TSAsExpression(ts) => peel_wrapped_expression(&ts.expression),
        Expression::TSSatisfiesExpression(ts) => peel_wrapped_expression(&ts.expression),
        Expression::TSNonNullExpression(ts) => peel_wrapped_expression(&ts.expression),
        Expression::TSTypeAssertion(ts) => peel_wrapped_expression(&ts.expression),
        _ => expr,
    }
}

fn peel_wrapped_expression_mut<'a, 'b>(expr: &'b mut Expression<'a>) -> &'b mut Expression<'a> {
    match expr {
        Expression::ParenthesizedExpression(paren) => {
            peel_wrapped_expression_mut(&mut paren.expression)
        }
        Expression::TSAsExpression(ts) => peel_wrapped_expression_mut(&mut ts.expression),
        Expression::TSSatisfiesExpression(ts) => peel_wrapped_expression_mut(&mut ts.expression),
        Expression::TSNonNullExpression(ts) => peel_wrapped_expression_mut(&mut ts.expression),
        Expression::TSTypeAssertion(ts) => peel_wrapped_expression_mut(&mut ts.expression),
        _ => expr,
    }
}

fn is_create_component_callee<'a>(expr: &Expression<'a>) -> bool {
    matches!(
        peel_wrapped_expression(expr),
        Expression::Identifier(ident) if ident.name.as_str() == "_$createComponent"
    )
}

fn is_root_create_component_expr<'a>(expr: &Expression<'a>) -> bool {
    let Expression::CallExpression(call) = peel_wrapped_expression(expr) else {
        return false;
    };
    is_create_component_callee(&call.callee)
}

fn this_alias_declaration_statement<'a>(alias: &str, context: &BlockContext<'a>) -> Statement<'a> {
    let ast = context.ast();
    let declarator = ast.variable_declarator(
        SPAN,
        VariableDeclarationKind::Const,
        ast.binding_pattern_binding_identifier(SPAN, ast.allocator.alloc_str(alias)),
        NONE,
        Some(ast.expression_this(SPAN)),
        false,
    );

    Statement::VariableDeclaration(ast.alloc_variable_declaration(
        SPAN,
        VariableDeclarationKind::Const,
        ast.vec1(declarator),
        false,
    ))
}

fn wrap_expr_with_this_alias_iife<'a>(
    expr: Expression<'a>,
    alias: &str,
    context: &BlockContext<'a>,
) -> Expression<'a> {
    let ast = context.ast();
    let mut statements = ast.vec_with_capacity(2);
    statements.push(this_alias_declaration_statement(alias, context));
    statements.push(Statement::ReturnStatement(
        ast.alloc_return_statement(SPAN, Some(expr)),
    ));

    let params = ast.alloc_formal_parameters(
        SPAN,
        FormalParameterKind::ArrowFormalParameters,
        ast.vec(),
        NONE,
    );
    let body = ast.alloc_function_body(SPAN, ast.vec(), statements);
    let arrow = ast.expression_arrow_function(SPAN, false, false, NONE, params, NONE, body);

    ast.expression_call(
        SPAN,
        arrow,
        None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
        ast.vec(),
        false,
    )
}

struct ComponentThisCaptureRewriter<'a, 'ctx> {
    context: &'ctx BlockContext<'a>,
    alias: Option<String>,
    skip_first_arg_on_root_create_component: bool,
    handled_root_create_component: bool,
    allow_function_body: bool,
}

impl<'a, 'ctx> ComponentThisCaptureRewriter<'a, 'ctx> {
    fn new(context: &'ctx BlockContext<'a>) -> Self {
        Self {
            context,
            alias: None,
            skip_first_arg_on_root_create_component: true,
            handled_root_create_component: false,
            allow_function_body: false,
        }
    }

    fn alias(mut self) -> Option<String> {
        self.alias.take()
    }

    fn alias_name(&mut self) -> String {
        if let Some(existing) = &self.alias {
            return existing.clone();
        }
        let generated = self.context.generate_uid("self$");
        self.alias = Some(generated.clone());
        generated
    }
}

impl<'a, 'ctx> VisitMut<'a> for ComponentThisCaptureRewriter<'a, 'ctx> {
    fn visit_expression(&mut self, expr: &mut Expression<'a>) {
        if matches!(expr, Expression::ThisExpression(_)) {
            let alias = self.alias_name();
            let ast = self.context.ast();
            *expr = ast.expression_identifier(SPAN, ast.allocator.alloc_str(&alias));
            return;
        }

        walk_mut::walk_expression(self, expr);
    }

    fn visit_call_expression(&mut self, call: &mut CallExpression<'a>) {
        self.visit_expression(&mut call.callee);

        let is_create_component = is_create_component_callee(&call.callee);
        let skip_first = is_create_component
            && self.skip_first_arg_on_root_create_component
            && !self.handled_root_create_component;

        if is_create_component && !self.handled_root_create_component {
            self.handled_root_create_component = true;
        }

        for (index, argument) in call.arguments.iter_mut().enumerate() {
            if skip_first && index == 0 {
                continue;
            }
            self.visit_argument(argument);
        }
    }

    fn visit_object_property(&mut self, property: &mut ObjectProperty<'a>) {
        if property.computed {
            self.visit_property_key(&mut property.key);
        }

        match property.kind {
            PropertyKind::Get | PropertyKind::Set => {
                let previous = self.allow_function_body;
                self.allow_function_body = true;
                self.visit_expression(&mut property.value);
                self.allow_function_body = previous;
            }
            PropertyKind::Init => {
                if matches!(property.value, Expression::FunctionExpression(_)) {
                    // Preserve `this` semantics for user-authored function props.
                    return;
                }
                self.visit_expression(&mut property.value);
            }
        }
    }

    fn visit_function(&mut self, function: &mut Function<'a>, flags: ScopeFlags) {
        if !self.allow_function_body {
            return;
        }

        let previous = self.allow_function_body;
        // Nested non-arrow functions establish their own `this` binding.
        self.allow_function_body = false;
        walk_mut::walk_function(self, function, flags);
        self.allow_function_body = previous;
    }
}

fn rewrite_component_expr_this_capture<'a>(
    expr: &mut Expression<'a>,
    context: &BlockContext<'a>,
) -> Option<String> {
    let mut rewriter = ComponentThisCaptureRewriter::new(context);
    rewriter.visit_expression(expr);
    rewriter.alias()
}

fn capture_this_in_statement_list<'a>(
    statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    context: &BlockContext<'a>,
) {
    let ast = context.ast();
    let old_statements = std::mem::replace(statements, ast.vec());
    let mut new_statements = ast.vec_with_capacity(old_statements.len());

    for mut statement in old_statements {
        let target_expr = match &mut statement {
            Statement::ExpressionStatement(expr_stmt) => Some(&mut expr_stmt.expression),
            Statement::ReturnStatement(return_stmt) => return_stmt.argument.as_mut(),
            Statement::BlockStatement(block) => {
                capture_this_in_statement_list(&mut block.body, context);
                None
            }
            _ => None,
        };

        if let Some(expr) = target_expr {
            if is_root_create_component_expr(expr) {
                if let Some(alias) = rewrite_component_expr_this_capture(expr, context) {
                    new_statements.push(this_alias_declaration_statement(&alias, context));
                }
            }
        }

        new_statements.push(statement);
    }

    *statements = new_statements;
}

fn process_class_property_value<'a>(value: &mut Expression<'a>, context: &BlockContext<'a>) {
    match peel_wrapped_expression_mut(value) {
        Expression::ArrowFunctionExpression(arrow) => {
            process_arrow_property_value(arrow, context);
        }
        Expression::FunctionExpression(function) => {
            if let Some(body) = function.body.as_mut() {
                capture_this_in_statement_list(&mut body.statements, context);
            }
        }
        expr => {
            if !is_root_create_component_expr(expr) {
                return;
            }

            let Some(alias) = rewrite_component_expr_this_capture(expr, context) else {
                return;
            };

            let ast = context.ast();
            let rewritten_expr =
                std::mem::replace(expr, ast.expression_identifier(SPAN, "undefined"));
            *expr = wrap_expr_with_this_alias_iife(rewritten_expr, &alias, context);
        }
    }
}

fn process_arrow_property_value<'a>(
    arrow: &mut ArrowFunctionExpression<'a>,
    context: &BlockContext<'a>,
) {
    if !arrow.expression {
        capture_this_in_statement_list(&mut arrow.body.statements, context);
        return;
    }

    let Some(Statement::ExpressionStatement(expr_stmt)) = arrow.body.statements.first_mut() else {
        return;
    };

    if !is_root_create_component_expr(&expr_stmt.expression) {
        return;
    }

    let Some(alias) = rewrite_component_expr_this_capture(&mut expr_stmt.expression, context)
    else {
        return;
    };

    let ast = context.ast();
    let returned_expr = std::mem::replace(
        &mut expr_stmt.expression,
        ast.expression_identifier(SPAN, "undefined"),
    );

    let mut statements = ast.vec_with_capacity(2);
    statements.push(this_alias_declaration_statement(&alias, context));
    statements.push(Statement::ReturnStatement(
        ast.alloc_return_statement(SPAN, Some(returned_expr)),
    ));

    arrow.body = ast.alloc_function_body(SPAN, ast.vec(), statements);
    arrow.expression = false;
}

fn capture_class_component_this<'a>(class: &mut Class<'a>, context: &BlockContext<'a>) {
    for element in &mut class.body.body {
        match element {
            ClassElement::MethodDefinition(method) => {
                if let Some(body) = method.value.body.as_mut() {
                    capture_this_in_statement_list(&mut body.statements, context);
                }
            }
            ClassElement::PropertyDefinition(property) => {
                if let Some(value) = property.value.as_mut() {
                    process_class_property_value(value, context);
                }
            }
            _ => {}
        }
    }
}

fn extract_statement_position_iife<'a>(
    expr: &Expression<'a>,
    context: &BlockContext<'a>,
) -> Option<(Vec<Statement<'a>>, Expression<'a>)> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    if !call.arguments.is_empty() {
        return None;
    }

    let Expression::ArrowFunctionExpression(arrow) = &call.callee else {
        return None;
    };
    if arrow.expression || !arrow.params.items.is_empty() || arrow.params.rest.is_some() {
        return None;
    }

    let Some(Statement::ReturnStatement(return_stmt)) = arrow.body.statements.last() else {
        return None;
    };
    let Some(return_expr) = &return_stmt.argument else {
        return None;
    };

    let ast = context.ast();
    let mut prefix = Vec::with_capacity(arrow.body.statements.len().saturating_sub(1));
    for stmt in arrow
        .body
        .statements
        .iter()
        .take(arrow.body.statements.len().saturating_sub(1))
    {
        prefix.push(stmt.clone_in(ast.allocator));
    }

    Some((prefix, return_expr.clone_in(ast.allocator)))
}

fn flatten_statement_position_iifes_in_list<'a>(
    statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    context: &BlockContext<'a>,
) {
    let ast = context.ast();
    let old_statements = std::mem::replace(statements, ast.vec());
    let mut flattened = ast.vec_with_capacity(old_statements.len());

    for mut statement in old_statements {
        let mut prefix_statements = Vec::new();

        match &mut statement {
            Statement::ReturnStatement(return_stmt) => {
                if let Some(argument) = &return_stmt.argument {
                    if let Some((prefix, return_expr)) =
                        extract_statement_position_iife(argument, context)
                    {
                        prefix_statements = prefix;
                        return_stmt.argument = Some(return_expr);
                    }
                }
            }
            Statement::VariableDeclaration(var_decl) => {
                for declarator in &mut var_decl.declarations {
                    let Some(init) = &declarator.init else {
                        continue;
                    };
                    if let Some((prefix, return_expr)) =
                        extract_statement_position_iife(init, context)
                    {
                        prefix_statements.extend(prefix);
                        declarator.init = Some(return_expr);
                    }
                }
            }
            _ => {}
        }

        for prefix in prefix_statements {
            flattened.push(prefix);
        }
        flattened.push(statement);
    }

    *statements = flattened;
}

struct StatementPositionIifeFlattener<'ctx, 'a> {
    context: &'ctx BlockContext<'a>,
}

impl<'a> VisitMut<'a> for StatementPositionIifeFlattener<'_, 'a> {
    fn visit_program(&mut self, program: &mut Program<'a>) {
        flatten_statement_position_iifes_in_list(&mut program.body, self.context);
        walk_mut::walk_program(self, program);
    }

    fn visit_function_body(&mut self, body: &mut oxc_ast::ast::FunctionBody<'a>) {
        flatten_statement_position_iifes_in_list(&mut body.statements, self.context);
        walk_mut::walk_function_body(self, body);
    }

    fn visit_block_statement(&mut self, block: &mut oxc_ast::ast::BlockStatement<'a>) {
        flatten_statement_position_iifes_in_list(&mut block.body, self.context);
        walk_mut::walk_block_statement(self, block);
    }

    fn visit_switch_case(&mut self, case: &mut SwitchCase<'a>) {
        flatten_statement_position_iifes_in_list(&mut case.consequent, self.context);
        walk_mut::walk_switch_case(self, case);
    }
}

fn flatten_statement_position_iifes<'a>(program: &mut Program<'a>, context: &BlockContext<'a>) {
    let mut flattener = StatementPositionIifeFlattener { context };
    flattener.visit_program(program);
}

fn apply_class_component_this_capture<'a>(program: &mut Program<'a>, context: &BlockContext<'a>) {
    for statement in &mut program.body {
        if let Statement::ClassDeclaration(class_decl) = statement {
            capture_class_component_this(class_decl, context);
        }
    }
}

impl<'a> Traverse<'a, ()> for SolidTransform<'a> {
    // Use exit_expression instead of enter_expression to avoid
    // oxc_traverse walking into our newly created nodes (which lack scope info)
    fn exit_expression(&mut self, node: &mut Expression<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        let new_expr = match node {
            Expression::JSXElement(element) => {
                if should_defer_nested_attribute_jsx(ctx) {
                    None
                } else {
                    let result = self.transform_jsx_element(
                        element,
                        &TransformInfo {
                            top_level: true,
                            last_element: true,
                            ..Default::default()
                        },
                        ctx,
                    );
                    let mut expr = self.build_output_expr(&result);
                    self.resolve_deferred_jsx_in_expression(&mut expr, ctx);
                    Some(expr)
                }
            }
            Expression::JSXFragment(fragment) => {
                if should_defer_nested_attribute_jsx(ctx) {
                    None
                } else {
                    let result = self.transform_fragment(
                        fragment,
                        &TransformInfo {
                            top_level: true,
                            ..Default::default()
                        },
                        ctx,
                    );
                    let mut expr = self.build_output_expr(&result);
                    self.resolve_deferred_jsx_in_expression(&mut expr, ctx);
                    Some(expr)
                }
            }
            _ => None,
        };

        if let Some(expr) = new_expr {
            *node = expr;
        }
    }

    fn exit_variable_declarator(
        &mut self,
        node: &mut VariableDeclarator<'a>,
        ctx: &mut TraverseCtx<'a, ()>,
    ) {
        let BindingPattern::BindingIdentifier(binding_ident) = &node.id else {
            return;
        };
        let Some(symbol_id) = binding_ident.symbol_id.get() else {
            return;
        };

        // Keep this conservative: if the symbol can be mutated anywhere, don't fold via binding.
        if ctx.scoping().symbol_is_mutated(symbol_id) {
            return;
        }

        let Some(init) = node.init.as_ref() else {
            return;
        };

        if let Some(value) = evaluate_static_text_expression(init, &self.context, ctx) {
            self.context.set_constant_text_value(symbol_id, value);
        }
    }

    fn exit_program(&mut self, program: &mut Program<'a>, ctx: &mut TraverseCtx<'a, ()>) {
        let should_capture_component_this =
            self.context.helpers.borrow().contains("createComponent");
        if should_capture_component_this {
            apply_class_component_this_capture(program, &self.context);
        }
        flatten_statement_position_iifes(program, &self.context);

        let templates = self.context.templates.borrow();
        let delegates = self.context.delegates.borrow();
        let has_helpers = !self.context.helpers.borrow().is_empty();

        if !has_helpers && templates.is_empty() && delegates.is_empty() {
            return;
        }

        let ast = ctx.ast;
        let span = SPAN;

        if self.options.validate {
            for template in templates.iter() {
                if let Some(result) = is_invalid_markup(&template.validation_content) {
                    eprintln!(
                        "\nThe HTML provided is malformed and will yield unexpected output when evaluated by a browser.\n"
                    );
                    eprintln!("User HTML:\n{}", result.html);
                    eprintln!("Browser HTML:\n{}", result.browser);
                    eprintln!("Original HTML:\n{}", template.validation_content);
                }
            }
        }

        // Insert delegateEvents call if needed
        if !delegates.is_empty() {
            self.context.register_helper("delegateEvents");

            let mut elements = ast.vec_with_capacity(delegates.len());
            for event in delegates.iter() {
                elements.push(ArrayExpressionElement::from(ast.expression_string_literal(
                    span,
                    ast.allocator.alloc_str(event),
                    None,
                )));
            }
            let array = ast.expression_array(span, elements);
            let callee = helper_ident_expr(ast, span, "delegateEvents");
            let call = ast.expression_call(
                span,
                callee,
                None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                ast.vec1(Argument::from(array)),
                false,
            );
            program.body.push(Statement::ExpressionStatement(
                ast.alloc_expression_statement(span, call),
            ));
        }

        let mut prepend = Vec::new();

        // Avoid duplicate helper aliases when they are already imported.
        let mut existing_import_locals = collect_value_import_local_names(program);

        if matches!(self.options.generate, GenerateMode::Dynamic) {
            let helper_imports = self.context.helper_imports_vec();
            let mut ordered_helpers = Vec::with_capacity(helper_imports.len());

            for helper in helper_imports
                .iter()
                .filter(|helper| helper.imported == "template")
            {
                ordered_helpers.push(helper);
            }

            let mut non_template_helpers: Vec<_> = helper_imports
                .iter()
                .filter(|helper| helper.imported != "template")
                .collect();
            non_template_helpers.reverse();
            ordered_helpers.extend(non_template_helpers);

            let mut reordered_helpers = ordered_helpers;

            let has_insert_collision = helper_imports
                .iter()
                .filter(|helper| helper.imported == "insert")
                .map(|helper| helper.module.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 1;

            if has_insert_collision {
                let dom_module_name = self
                    .options
                    .dynamic_dom_renderer_module_name()
                    .unwrap_or(self.options.module_name);
                let mut moved_dom_insert_helpers = Vec::new();
                let mut reordered_with_tail = Vec::with_capacity(reordered_helpers.len());

                for helper in reordered_helpers {
                    let move_to_tail = helper.module == dom_module_name
                        && helper.module != self.options.module_name
                        && matches!(helper.imported.as_str(), "insert" | "use");
                    if move_to_tail {
                        moved_dom_insert_helpers.push(helper);
                    } else {
                        reordered_with_tail.push(helper);
                    }
                }
                reordered_with_tail.extend(moved_dom_insert_helpers);
                reordered_helpers = reordered_with_tail;

                if let (Some(memo_index), Some(insert_node_index)) = (
                    reordered_helpers
                        .iter()
                        .position(|helper| helper.imported == "memo"),
                    reordered_helpers
                        .iter()
                        .position(|helper| helper.imported == "insertNode"),
                ) {
                    if memo_index < insert_node_index {
                        reordered_helpers.swap(memo_index, insert_node_index);
                    }
                }
            }

            let should_move_set_attr_effect_block = reordered_helpers
                .iter()
                .any(|helper| matches!(helper.imported.as_str(), "setProp" | "createElement"))
                && reordered_helpers
                    .iter()
                    .any(|helper| helper.imported == "setAttribute")
                && reordered_helpers
                    .iter()
                    .any(|helper| helper.imported == "effect")
                && !reordered_helpers.iter().any(|helper| {
                    matches!(
                        helper.imported.as_str(),
                        "style"
                            | "setStyleProperty"
                            | "className"
                            | "addEventListener"
                            | "delegateEvents"
                    )
                });

            if should_move_set_attr_effect_block {
                let mut moved_tail = Vec::new();
                reordered_helpers.retain(|helper| {
                    if matches!(helper.imported.as_str(), "setAttribute" | "effect") {
                        moved_tail.push(*helper);
                        false
                    } else {
                        true
                    }
                });

                if !moved_tail.is_empty() {
                    let insert_at = reordered_helpers
                        .iter()
                        .rposition(|helper| {
                            matches!(helper.imported.as_str(), "setProp" | "createElement")
                        })
                        .map(|index| index + 1)
                        .unwrap_or(reordered_helpers.len());
                    reordered_helpers.splice(insert_at..insert_at, moved_tail);
                }
            }

            if reordered_helpers
                .iter()
                .any(|helper| helper.imported == "getNextMatch")
            {
                let mut reordered = Vec::with_capacity(reordered_helpers.len());
                if let Some(template_helper) = reordered_helpers
                    .first()
                    .copied()
                    .filter(|helper| helper.imported == "template")
                {
                    reordered.push(template_helper);
                }

                let mut remaining: Vec<_> = reordered_helpers
                    .into_iter()
                    .filter(|helper| helper.imported != "template")
                    .collect();

                for helper_name in [
                    "getNextElement",
                    "getNextMatch",
                    "NoHydration",
                    "getNextMarker",
                ] {
                    if let Some(index) = remaining
                        .iter()
                        .position(|candidate| candidate.imported == helper_name)
                    {
                        reordered.push(remaining.remove(index));
                    }
                }

                reordered.extend(remaining);
                reordered_helpers = reordered;
            }

            for helper in reordered_helpers {
                if existing_import_locals.contains(&helper.local) {
                    continue;
                }

                prepend.push(build_named_value_import_statement(
                    ast,
                    span,
                    &helper.module,
                    &helper.imported,
                    &helper.local,
                ));
                existing_import_locals.insert(helper.local.clone());
            }
        } else {
            let helpers = self.context.helpers.borrow();

            // Emit one import declaration per helper:
            // import { template as _$template } from "solid-js/web";
            if !helpers.is_empty() {
                let module_name = self.options.module_name;

                // Babel import insertion is prepend-like for non-template helpers.
                // Mirror this by keeping `template` first and reversing the registration
                // order for the remaining helpers.
                let is_universal = matches!(self.options.generate, GenerateMode::Universal);
                let mut helper_names: Vec<&str> = helpers.iter().map(|h| h.as_str()).collect();
                if is_universal {
                    debug_assert!(
                        !helper_names.iter().any(|helper| *helper == "template"),
                        "universal mode should not register template helper"
                    );
                    helper_names.retain(|helper| *helper != "template");
                }
                let mut ordered_helpers = Vec::with_capacity(helper_names.len());

                if let Some(template_index) =
                    helper_names.iter().position(|helper| *helper == "template")
                {
                    helper_names.remove(template_index);
                    ordered_helpers.push("template");
                }

                helper_names.reverse();
                ordered_helpers.extend(helper_names);

                // Babel parity quirk: when only `createComponent` + `insert` are present
                // (besides `template`), `createComponent` import is emitted before `insert`.
                if ordered_helpers.len() == 3
                    && ordered_helpers.first() == Some(&"template")
                    && ordered_helpers.contains(&"insert")
                    && ordered_helpers.contains(&"createComponent")
                {
                    if ordered_helpers[1] == "insert" && ordered_helpers[2] == "createComponent" {
                        ordered_helpers.swap(1, 2);
                    }
                }

                if ordered_helpers.contains(&"getNextMatch") {
                    let mut reordered = Vec::with_capacity(ordered_helpers.len());
                    if ordered_helpers.first() == Some(&"template") {
                        reordered.push("template");
                    }
                    let mut remaining: Vec<&str> = ordered_helpers
                        .into_iter()
                        .filter(|helper| *helper != "template")
                        .collect();
                    for helper in [
                        "getNextElement",
                        "getNextMatch",
                        "NoHydration",
                        "getNextMarker",
                    ] {
                        if let Some(index) =
                            remaining.iter().position(|candidate| *candidate == helper)
                        {
                            reordered.push(helper);
                            remaining.remove(index);
                        }
                    }
                    reordered.extend(remaining);
                    ordered_helpers = reordered;
                }

                for helper in ordered_helpers {
                    let local_name = helper_local_name(helper);
                    if existing_import_locals.contains(&local_name) {
                        continue;
                    }

                    prepend.push(build_named_value_import_statement(
                        ast,
                        span,
                        module_name,
                        helper,
                        &local_name,
                    ));
                    existing_import_locals.insert(local_name);
                }
            }
        }

        // Insert template declarations as a single var statement.
        // Universal mode must never emit DOM template declarations.
        if matches!(self.options.generate, GenerateMode::Universal) {
            debug_assert!(
                templates.is_empty(),
                "universal mode should not register template declarations"
            );
        } else if !templates.is_empty() {
            let mut declarators = ast.vec_with_capacity(templates.len());
            for (i, tmpl) in templates.iter().enumerate() {
                let tmpl_span = tmpl.span;
                let tmpl_var = template_var_name(i);

                let mut quasis = ast.vec_with_capacity(1);
                let cooked_str = ast.allocator.alloc_str(&tmpl.content);
                let raw_template = escape_string_for_template(&tmpl.content);
                let raw_str = ast.allocator.alloc_str(&raw_template);
                let value = TemplateElementValue {
                    raw: ast.str(raw_str),
                    cooked: Some(ast.str(cooked_str)),
                };
                quasis.push(ast.template_element(tmpl_span, value, true));
                let template_lit = ast.template_literal(tmpl_span, quasis, ast.vec());
                let template_expr = Expression::TemplateLiteral(ast.alloc(template_lit));

                let needs_flags = tmpl.is_svg || tmpl.use_import_node || tmpl.is_math_ml;
                let mut args = ast.vec_with_capacity(if needs_flags { 4 } else { 1 });
                args.push(Argument::from(template_expr));
                if needs_flags {
                    args.push(Argument::from(
                        ast.expression_boolean_literal(tmpl_span, tmpl.use_import_node),
                    ));
                    args.push(Argument::from(
                        ast.expression_boolean_literal(tmpl_span, tmpl.is_svg),
                    ));
                    args.push(Argument::from(
                        ast.expression_boolean_literal(tmpl_span, tmpl.is_math_ml),
                    ));
                }

                let mut call = ast.expression_call(
                    tmpl_span,
                    helper_ident_expr(ast, tmpl_span, "template"),
                    None::<oxc_ast::ast::TSTypeParameterInstantiation<'a>>,
                    args,
                    false,
                );
                if let Expression::CallExpression(call_expr) = &mut call {
                    call_expr.pure = true;
                }

                declarators.push(ast.variable_declarator(
                    tmpl_span,
                    VariableDeclarationKind::Var,
                    ast.binding_pattern_binding_identifier(
                        tmpl_span,
                        ast.allocator.alloc_str(&tmpl_var),
                    ),
                    NONE,
                    Some(call),
                    false,
                ));
            }

            prepend.push(Statement::VariableDeclaration(
                ast.alloc_variable_declaration(
                    SPAN,
                    VariableDeclarationKind::Var,
                    declarators,
                    false,
                ),
            ));
        }

        // Prepend statements in correct order
        prepend_program_statements(program, prepend);
    }
}
