//! These tests verify the OXC compiler output matches expected jsx-dom-expressions patterns.

use common::{GenerateMode, RendererConfig};
use oxc_solid_js_compiler::{transform, TransformOptions};
use oxc_span::SourceType;

/// Helper to normalize whitespace for comparison
fn normalize(s: &str) -> String {
    s.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn transform_dom(source: &str) -> String {
    let result = transform(source, None);
    normalize(&result.code)
}

fn transform_dom_with_options(source: &str, options: TransformOptions) -> String {
    let result = transform(source, Some(options));
    normalize(&result.code)
}

fn transform_ssr(source: &str) -> String {
    let options = TransformOptions {
        generate: GenerateMode::Ssr,
        ..TransformOptions::solid_defaults()
    };
    let result = transform(source, Some(options));
    normalize(&result.code)
}

fn transform_universal(source: &str) -> String {
    let options = TransformOptions {
        generate: GenerateMode::Universal,
        module_name: "r-custom",
        ..TransformOptions::solid_defaults()
    };
    let result = transform(source, Some(options));
    normalize(&result.code)
}

fn transform_dynamic_with_dom_elements(source: &str, dom_elements: &[&'static str]) -> String {
    let options = TransformOptions {
        generate: GenerateMode::Dynamic,
        module_name: "r-custom",
        renderers: vec![RendererConfig {
            name: "dom",
            module_name: "r-dom",
            elements: dom_elements.to_vec(),
        }],
        ..TransformOptions::solid_defaults()
    };
    let result = transform(source, Some(options));
    normalize(&result.code)
}

fn wrapperless_dom_options() -> TransformOptions<'static> {
    TransformOptions {
        wrap_conditionals: false,
        delegate_events: false,
        effect_wrapper: "",
        memo_wrapper: "",
        ..TransformOptions::solid_defaults()
    }
}

fn require_import_source_dom_options() -> TransformOptions<'static> {
    TransformOptions {
        require_import_source: Some("r-dom"),
        ..TransformOptions::solid_defaults()
    }
}

fn no_inline_styles_dom_options() -> TransformOptions<'static> {
    TransformOptions {
        inline_styles: false,
        ..TransformOptions::solid_defaults()
    }
}

fn assert_has_transformed_template_patterns(code: &str) {
    assert!(
        code.contains("template as _$template") || code.contains("_$template(`<div>Hello"),
        "Expected transformed template helper import/call patterns, got:\n{code}"
    );
    assert!(
        code.contains("_$template(`<div>Hello") || code.contains("_tmpl$"),
        "Expected transformed template declarations/usage patterns, got:\n{code}"
    );
    assert!(
        !code.contains("<div>Hello</div>"),
        "Transformed output should not keep raw JSX element syntax, got:\n{code}"
    );
}

fn assert_keeps_raw_jsx_patterns(code: &str) {
    assert!(
        code.contains("<div>Hello</div>"),
        "Expected JSX syntax to remain when transform is skipped, got:\n{code}"
    );
    assert!(
        !code.contains("template as _$template")
            && !code.contains("_$template(`<div>Hello")
            && !code.contains("_tmpl$"),
        "Skipped transform should not emit template helper/template declarations, got:\n{code}"
    );
}

fn transform_ssr_hydratable(source: &str) -> String {
    let options = TransformOptions {
        generate: GenerateMode::Ssr,
        hydratable: true,
        ..TransformOptions::solid_defaults()
    };
    let result = transform(source, Some(options));
    normalize(&result.code)
}

fn transform_dom_hydratable(source: &str) -> String {
    let options = TransformOptions {
        generate: GenerateMode::Dom,
        hydratable: true,
        ..TransformOptions::solid_defaults()
    };
    let result = transform(source, Some(options));
    normalize(&result.code)
}

fn has_set_property_call(code: &str, property: &str) -> bool {
    code.contains("setProperty(") && code.contains(&format!("\"{property}\""))
}

fn assert_substring_order(code: &str, before: &str, after: &str, label: &str) {
    let before_idx = code
        .find(before)
        .unwrap_or_else(|| panic!("Missing `{before}` in {label}. Output:\n{code}"));
    let after_idx = code
        .find(after)
        .unwrap_or_else(|| panic!("Missing `{after}` in {label}. Output:\n{code}"));

    assert!(
        before_idx < after_idx,
        "Expected `{before}` before `{after}` in {label}. Output:\n{code}"
    );
}

#[test]
fn test_dom_static_element() {
    let code = transform_dom(r#"<div class="hello">world</div>"#);
    assert!(
        code.contains("template(`<div class=hello>world")
            || code.contains("template(`<div class=\"hello\">world")
    );
    assert!(
        code.contains("_tmpl$()"),
        "Static elements should instantiate with direct _tmpl$() call, got:\n{code}"
    );
    assert!(
        !code.contains("(() => {"),
        "Static elements should not be wrapped in an IIFE, got:\n{code}"
    );
    assert!(
        !code.contains("cloneNode(true)"),
        "Static elements should not clone via cloneNode(true), got:\n{code}"
    );
}

#[test]
fn test_dom_static_folded_expression_stays_direct_template_call() {
    let code = transform_dom(r#"<div><span>{0}</span></div>"#);
    assert!(
        code.contains("template(`<div><span>0"),
        "Static expression should be folded into template HTML, got:\n{code}"
    );
    assert!(
        code.contains("_tmpl$()"),
        "Static folded elements should instantiate with direct _tmpl$() call, got:\n{code}"
    );
    assert!(
        !code.contains("(() => {"),
        "Static folded elements should not be wrapped in an IIFE, got:\n{code}"
    );
    assert!(
        !code.contains("cloneNode(true)"),
        "Static folded elements should not clone via cloneNode(true), got:\n{code}"
    );
}

#[test]
fn test_dom_plan_1_12_child_ternary_true_folds_without_insert() {
    let code = transform_dom(r#"<span>{true ? "A" : "B"}</span>"#);
    assert!(
        code.contains("template(`<span>A"),
        "Expected ternary child expression to fold into template HTML, got:\n{code}"
    );
    assert!(
        !code.contains("insert as _$insert") && !code.contains("_$insert("),
        "Folded ternary child should not emit runtime insert helper usage, got:\n{code}"
    );
}

#[test]
fn test_dom_plan_1_12_child_logical_or_folds_without_insert() {
    let code = transform_dom(r#"<span>{false || "A"}</span>"#);
    assert!(
        code.contains("template(`<span>A"),
        "Expected logical-or child expression to fold into template HTML, got:\n{code}"
    );
    assert!(
        !code.contains("insert as _$insert") && !code.contains("_$insert("),
        "Folded logical-or child should not emit runtime insert helper usage, got:\n{code}"
    );
}

#[test]
fn test_dom_plan_1_12_child_string_concat_folds_without_insert() {
    let code = transform_dom(r#"<span>{("A" + "B")}</span>"#);
    assert!(
        code.contains("template(`<span>AB"),
        "Expected string concatenation child expression to fold into template HTML, got:\n{code}"
    );
    assert!(
        !code.contains("insert as _$insert") && !code.contains("_$insert("),
        "Folded string concatenation child should not emit runtime insert helper usage, got:\n{code}"
    );
}

#[test]
fn test_dom_plan_1_12_attr_logical_or_folds_without_set_attribute_helper() {
    let code = transform_dom(r#"<div id={false || "ab"} />"#);
    assert!(
        code.contains("template(`<div id=ab>`)")
            || code.contains("template(`<div id=ab></div>`)")
            || code.contains("template(`<div id=\"ab\">`)")
            || code.contains("template(`<div id=\"ab\"></div>`)"),
        "Expected logical-or id attribute expression to fold into template HTML, got:\n{code}"
    );
    assert!(
        !code.contains("setAttribute as _$setAttribute")
            && !code.contains("_$setAttribute(")
            && !code.contains("setAttribute("),
        "Folded id attribute should not emit runtime setAttribute helper usage, got:\n{code}"
    );
}

#[test]
fn test_dom_dynamic_attribute_uses_iife_with_template_call_no_clone_node() {
    let code = transform_dom(r#"<div id={value()}>world</div>"#);
    assert!(
        code.contains("(() => {"),
        "Dynamic element path should keep IIFE output shape, got:\n{code}"
    );
    assert!(
        code.contains("_tmpl$()"),
        "Dynamic element path should instantiate with _tmpl$(), got:\n{code}"
    );
    assert!(
        !code.contains("cloneNode(true)"),
        "Dynamic element path should not use cloneNode(true), got:\n{code}"
    );
}

#[test]
fn test_dom_nested_elements() {
    let code = transform_dom(r#"<div><span>hello</span><p>world</p></div>"#);
    assert!(code.contains("template(`<div><span>hello</span><p>world"));
}

#[test]
fn test_dom_void_element() {
    let code = transform_dom(r#"<input type="text" />"#);
    assert!(
        code.contains("template(`<input type=text>`)")
            || code.contains("template(`<input type=\"text\">`)")
    );
    // Void elements don't have closing tags
    assert!(!code.contains("</input>"));
}

#[test]
fn test_dom_self_closing() {
    let code = transform_dom(r#"<div />"#);
    assert!(code.contains("template(`<div>`)") || code.contains("template(`<div></div>`)"));
}

#[test]
fn test_dynamic_mode_known_intrinsic_routes_to_template_path() {
    let code = transform_dynamic_with_dom_elements(r#"<div />"#, &["div"]);
    assert!(
        code.contains("template as _$template") || code.contains("_$template(`<div"),
        "expected known dynamic intrinsic to use DOM template helper path, got:\n{code}"
    );
    assert!(
        !code.contains("createElement as _$createElement") && !code.contains("_$createElement("),
        "known DOM-routed dynamic intrinsic should not use universal createElement path, got:\n{code}"
    );
}

#[test]
fn test_dynamic_mode_unknown_intrinsic_routes_to_create_element_path() {
    let code = transform_dynamic_with_dom_elements(r#"<mesh />"#, &["div"]);
    assert!(
        code.contains("createElement as _$createElement") || code.contains("_$createElement("),
        "expected unknown dynamic intrinsic to use universal createElement path, got:\n{code}"
    );
    assert!(
        !code.contains("template as _$template") && !code.contains("_tmpl$"),
        "unknown dynamic intrinsic should not emit DOM template helper/declarations, got:\n{code}"
    );
}

#[test]
fn test_dynamic_mode_uses_renderer_and_base_modules() {
    let code = transform_dynamic_with_dom_elements(
        r#"const a = <div>{name}</div>; const b = <mesh>{name}</mesh>;"#,
        &["div"],
    );
    assert!(
        code.contains("from \"r-dom\";") && code.contains("from \"r-custom\";"),
        "expected dynamic mode to import helpers from both renderer and base modules, got:\n{code}"
    );
    assert!(
        code.contains("template as _$template")
            && code.contains("createElement as _$createElement"),
        "expected DOM template + universal createElement imports in dynamic mode, got:\n{code}"
    );
}

#[test]
fn test_dynamic_mode_suffixes_insert_helper_collisions() {
    let code = transform_dynamic_with_dom_elements(
        r#"const a = <div>{name}</div>; const b = <mesh>{name}</mesh>;"#,
        &["div"],
    );

    let dom_primary = code.contains("import { insert as _$insert } from \"r-dom\";")
        && code.contains("import { insert as _$insert2 } from \"r-custom\";");
    let base_primary = code.contains("import { insert as _$insert } from \"r-custom\";")
        && code.contains("import { insert as _$insert2 } from \"r-dom\";");

    assert!(
        dom_primary || base_primary,
        "expected insert helper collision to be imported with numeric suffix across modules, got:\n{code}"
    );
    assert!(
        code.contains("_$insert(") && code.contains("_$insert2("),
        "expected both insert aliases to be used in output, got:\n{code}"
    );
}

#[test]
fn test_dynamic_mode_suffixes_spread_helper_collisions() {
    let code = transform_dynamic_with_dom_elements(
        r#"const a = <div {...props} />; const b = <mesh {...props} />;"#,
        &["div"],
    );

    let dom_primary = code.contains("import { spread as _$spread } from \"r-dom\";")
        && code.contains("import { spread as _$spread2 } from \"r-custom\";");
    let base_primary = code.contains("import { spread as _$spread } from \"r-custom\";")
        && code.contains("import { spread as _$spread2 } from \"r-dom\";");

    assert!(
        dom_primary || base_primary,
        "expected spread helper collision to be imported with numeric suffix across modules, got:\n{code}"
    );
    assert!(
        code.contains("_$spread(") && code.contains("_$spread2("),
        "expected both spread aliases to be used in output, got:\n{code}"
    );
}

#[test]
fn test_universal_static_element_uses_create_element_not_template() {
    let code = transform_universal(r#"<div />"#);
    assert!(
        code.contains("createElement as _$createElement") || code.contains("_$createElement("),
        "expected universal createElement helper usage, got:\n{code}"
    );
    assert!(
        !code.contains("template as _$template") && !code.contains("_tmpl$"),
        "universal mode should not emit template helpers, got:\n{code}"
    );
}

#[test]
fn test_universal_text_child_uses_create_text_node_and_insert_node() {
    let code = transform_universal(r#"<div>Hello</div>"#);
    assert!(
        code.contains("createTextNode") && code.contains("insertNode"),
        "expected createTextNode + insertNode for static text children, got:\n{code}"
    );
}

#[test]
fn test_universal_dynamic_child_uses_insert_helper() {
    let code = transform_universal(r#"<div>{value()}</div>"#);
    assert!(
        code.contains("insert as _$insert") || code.contains("_$insert("),
        "expected universal insert helper for dynamic children, got:\n{code}"
    );
}

#[test]
fn test_universal_static_attribute_uses_set_prop() {
    let code = transform_universal(r#"<div id="main" />"#);
    assert!(
        code.contains("setProp as _$setProp") || code.contains("_$setProp("),
        "expected setProp helper usage in universal mode, got:\n{code}"
    );
    assert!(
        code.contains("\"id\"") && code.contains("\"main\""),
        "expected static attribute key/value to be passed to setProp, got:\n{code}"
    );
}

#[test]
fn test_universal_spread_attribute_uses_spread_helper() {
    let code = transform_universal(r#"<div {...props} />"#);
    assert!(
        code.contains("spread as _$spread") || code.contains("_$spread("),
        "expected spread helper usage in universal mode, got:\n{code}"
    );
}

#[test]
fn test_universal_use_directive_and_ref_use_use_helper() {
    let use_code = transform_universal(r#"<div use:something />"#);
    assert!(
        use_code.contains("use as _$use") || use_code.contains("_$use("),
        "expected use:directive to register/use use helper, got:\n{use_code}"
    );

    let ref_code = transform_universal(r#"let r; <div ref={r} />"#);
    assert!(
        ref_code.contains("use as _$use") || ref_code.contains("_$use("),
        "expected ref lowering to include use helper path, got:\n{ref_code}"
    );
}

#[test]
fn test_universal_component_children_emit_create_element_output() {
    let code = transform_universal(r#"<Comp><div>Hello</div></Comp>"#);
    assert!(
        code.contains("createComponent as _$createComponent")
            || code.contains("_$createComponent("),
        "expected component lowering, got:\n{code}"
    );
    assert!(
        code.contains("createElement") && code.contains("Hello"),
        "expected intrinsic child to lower through universal createElement path, got:\n{code}"
    );
    assert!(
        !code.contains("template as _$template") && !code.contains("_tmpl$"),
        "universal component child lowering should avoid DOM templates, got:\n{code}"
    );
}

#[test]
fn test_universal_fragment_never_emits_template_artifacts() {
    let code = transform_universal(r#"<><div id="main">Hi</div><span>{name}</span></>"#);
    assert!(
        code.contains("createElement as _$createElement") || code.contains("_$createElement("),
        "expected universal fragment children to use createElement lowering, got:\n{code}"
    );
    assert!(
        !code.contains("template as _$template")
            && !code.contains("_$template(")
            && !code.contains("_tmpl$"),
        "universal fragments should never emit template helper/declarations, got:\n{code}"
    );
}

#[test]
fn test_universal_single_dynamic_attribute_uses_effect_prev_signature() {
    let code = transform_universal(r#"<div id={state.id} />"#);
    assert!(
        code.contains("effect as _$effect") || code.contains("_$effect("),
        "expected effect helper usage for universal dynamic attribute, got:\n{code}"
    );
    assert!(
        code.contains("() => state.id")
            && code.contains("(_v$, _$p) =>")
            && code.contains("\"id\", _v$, _$p"),
        "expected single-dynamic effect callback shape with prev param, got:\n{code}"
    );
}

#[test]
fn test_universal_multi_dynamic_attributes_batch_into_single_effect() {
    let code = transform_universal(r#"<div id={state.id} title={state.title} />"#);
    let effect_count = code.matches("_$effect(").count();
    assert_eq!(
        effect_count, 1,
        "expected one batched effect call for multi-dynamic attrs, got {} in:\n{}",
        effect_count, code
    );
    assert!(
        code.contains("() => ({")
            && code.contains("e: state.id")
            && code.contains("t: state.title")
            && code.contains("({ e, t }, _p$) =>")
            && code.contains("\"id\", e, _p$.e")
            && code.contains("\"title\", t, _p$.t"),
        "expected Babel-style batched effect object/callback shape, got:\n{code}"
    );
}

#[test]
fn test_universal_effect_wrapper_disabled_sets_dynamic_attrs_directly() {
    let code = transform_dom_with_options(
        r#"<div id={state.id} />"#,
        TransformOptions {
            generate: GenerateMode::Universal,
            module_name: "r-custom",
            effect_wrapper: "",
            ..TransformOptions::solid_defaults()
        },
    );
    assert!(
        !code.contains("effect as _$effect") && !code.contains("_$effect("),
        "expected no effect helper when universal effect wrapper is disabled, got:\n{code}"
    );
    assert!(
        code.contains("setProp as _$setProp")
            && code.contains("_$setProp(")
            && code.contains("\"id\"")
            && code.contains("state.id"),
        "expected direct setProp write for dynamic attr without effect wrapper, got:\n{code}"
    );
}

#[test]
fn test_dom_noscript_children_are_skipped() {
    let code = transform_dom(
        r#"<div><noscript>No JS!!<style>{"div { color: red; }"}</style></noscript></div>"#,
    );
    assert!(
        code.contains("template(`<div><noscript>`)") || code.contains("template(`<div><noscript>")
    );
    assert!(
        !code.contains("No JS!!"),
        "noscript children should not be emitted into template: {code}"
    );
    assert!(
        !code.contains("<style>"),
        "noscript child elements should not be transformed: {code}"
    );
}

#[test]
fn test_dom_noscript_closing_tags_still_emit_with_option() {
    let code = transform_dom_with_options(
        r#"<div><noscript>{msg()}</noscript></div>"#,
        TransformOptions {
            omit_last_closing_tag: false,
            ..TransformOptions::solid_defaults()
        },
    );
    assert!(
        code.contains("<noscript></noscript></div>"),
        "expected closing tags to remain when omission is disabled: {code}"
    );
    assert!(
        !code.contains("msg()"),
        "noscript expression children should be skipped: {code}"
    );
}

#[test]
fn test_dom_dynamic_class() {
    let code = transform_dom(r#"<div class={style()}>content</div>"#);
    assert!(code.contains("effect"));
    assert!(code.contains("className"));
    // Babel-style output may normalize `style()` to `style` as an effect source accessor.
    assert!(code.contains("style()") || code.contains("effect(style"));
}

#[test]
fn test_dom_dynamic_multiple_attrs() {
    let code = transform_dom(r#"<div class={cls()} id={id()}>content</div>"#);
    assert!(code.contains("cls()") || code.contains("effect(cls"));
    assert!(code.contains("id()") || code.contains("effect(id"));
}

#[test]
fn test_dom_mixed_static_dynamic() {
    let code = transform_dom(r#"<div class="static" id={dynamic()}>content</div>"#);
    assert!(code.contains("class=static") || code.contains("class=\"static\""));
    assert!(code.contains("dynamic()") || code.contains("effect(dynamic"));
}

#[test]
fn test_dom_boolean_attribute() {
    let code = transform_dom(r#"<input disabled />"#);
    assert!(code.contains("disabled"));
}

#[test]
fn test_dom_class_namespace_binding() {
    let code = transform_dom(r#"<div class:my-class={props.active} />"#);
    assert!(code.contains("classList.toggle"));
    assert!(code.contains("\"my-class\""));
    assert!(code.contains("props.active"));
}

#[test]
fn test_dom_style_namespace_binding() {
    let code = transform_dom(r#"<div style:padding-top={props.top} />"#);
    assert!(code.contains("setStyleProperty"));
    assert!(code.contains("\"padding-top\""));
    assert!(code.contains("props.top"));
}

#[test]
fn test_dom_classname_not_aliased_to_class() {
    let code = transform_dom(r#"<div className={c()} />"#);
    assert!(code.contains("\"className\""));
    assert!(
        !code.contains("className(_el$"),
        "className prop should not use class helper: {code}"
    );
}

#[test]
fn test_dom_static_class_and_classname_stay_separate() {
    let code = transform_dom(r#"<div class="a" className="b" />"#);
    assert!(
        code.contains("template(`<div class=a className=b>`)")
            || code.contains("template(`<div class=\"a\" className=\"b\">`)")
            || code.contains("template(`<div class=a className=\"b\">`)")
    );
    assert!(
        !code.contains("class=\"a b\"") && !code.contains("class=a b"),
        "static className should not be merged into class: {code}"
    );
}

#[test]
fn test_dom_dynamic_textcontent_keeps_space_placeholder_text_node() {
    let code = transform_dom(r#"<div><div textContent={row.label} /></div>"#);
    assert!(
        code.contains("<div><div> "),
        "expected dynamic textContent placeholder space in template: {code}"
    );
    assert!(
        code.contains(".data = _v$") || code.contains(".data=_v$"),
        "expected dynamic textContent updates to target text node data: {code}"
    );
}

#[test]
fn test_dom_event_string_literal_is_static_attribute() {
    let code = transform_dom(r#"<div onclick="console.log('hi')" />"#);
    assert!(
        code.contains("onclick=\"console.log('hi')\""),
        "expected inline static onclick attribute: {code}"
    );
    assert!(
        !code.contains("addEventListener"),
        "string-literal onclick should not become runtime event wiring: {code}"
    );
}

#[test]
fn test_dom_use_directive_without_value_defaults_to_true() {
    let code = transform_dom(r#"<div use:something />"#);
    assert!(
        code.contains("() => true"),
        "use:directive without value should default to () => true: {code}"
    );
}

#[test]
fn test_dom_attr_namespace_not_special_cased() {
    let code = transform_dom(r#"<div attr:role={role()} />"#);
    assert!(code.contains("\"attr:role\""));
    assert!(
        !code.contains("\"role\""),
        "attr: namespace should not be stripped: {code}"
    );
}

#[test]
fn test_dom_oncapture_namespace_not_event_handler() {
    let code = transform_dom(r#"<div oncapture:foo={handler} />"#);
    assert!(code.contains("\"oncapture:foo\""));
    assert!(
        !code.contains("addEventListener") && !code.contains("$$foo"),
        "oncapture: namespace should not generate event wiring: {code}"
    );
}

#[test]
fn test_dom_onclick_delegated_resolvable_handler() {
    let code = transform_dom(
        r#"
        const hoisted = () => {};
        <button onClick={hoisted}>click</button>
        "#,
    );
    // Resolvable handlers use delegated $$eventName assignment
    assert!(code.contains("$$click"), "Output was:\n{code}");
    assert!(code.contains("delegateEvents"), "Output was:\n{code}");
}

#[test]
fn test_dom_onclick_unresolvable_handler_uses_listener_helper() {
    let code = transform_dom(r#"<button onClick={handler}>click</button>"#);
    // Non-resolvable delegated handlers fall back to helper addEvent(..., true)
    assert!(
        code.contains("addEvent") && code.contains("true"),
        "Output was:\n{code}"
    );
    assert!(
        !code.contains("$$click = handler"),
        "Unresolvable handler should not be delegated directly. Output was:\n{code}"
    );
}

#[test]
fn test_dom_oncapture_not_delegated() {
    let code = transform_dom(r#"<button onClickCapture={handler}>click</button>"#);
    // Capture events are not delegated and use native addEventListener with capture=true
    assert!(code.contains("addEventListener"), "Output was:\n{code}");
    assert!(code.contains("true"), "Output was:\n{code}");
    assert!(
        !code.contains("addEventListener(_el$1, _el$1"),
        "Should not pass the element twice. Output was:\n{code}"
    );
}

#[test]
fn test_dom_on_namespace_uses_listener_helper() {
    let code = transform_dom(r#"<button on:click={handler}>click</button>"#);
    assert!(code.contains("addEventListener"), "Output was:\n{code}");
    assert!(
        !code.contains("$$click"),
        "on:click should bypass delegation. Output was:\n{code}"
    );
}

#[test]
fn test_dom_on_namespace_reverse_emission_order() {
    let code = transform_dom(r#"<button on:first={a} on:second={b} on:third={c}>click</button>"#);

    let third = code
        .find("\"third\"")
        .expect("missing on:third listener emission");
    let second = code
        .find("\"second\"")
        .expect("missing on:second listener emission");
    let first = code
        .find("\"first\"")
        .expect("missing on:first listener emission");

    assert!(
        third < second && second < first,
        "Expected reverse emission order for on: namespace listeners, got:\n{code}"
    );
}

#[test]
fn test_dom_delegated_array_event_assigns_handler_before_data() {
    let code = transform_dom(r#"<button onClick={[handler, rowId]}>click</button>"#);
    let handler = code
        .find("$$click = handler")
        .expect("missing delegated handler assignment");
    let data = code
        .find("$$clickData = rowId")
        .expect("missing delegated handler data assignment");
    assert!(
        handler < data,
        "Expected $$click assignment before $$clickData, got:\n{code}"
    );
}

#[test]
fn test_dom_onscroll_not_delegated() {
    let code = transform_dom(r#"<div onScroll={handler}>scroll</div>"#);
    // scroll is not delegated by default
    assert!(code.contains("addEvent") || code.contains("onscroll"));
}

#[test]
fn test_dom_compound_event_name_lowercase() {
    // Compound event names like onMouseDown should become "mousedown" (all lowercase)
    let code = transform_dom(r#"<div onMouseDown={handler}>test</div>"#);
    assert!(
        code.contains("\"mousedown\""),
        "onMouseDown should produce lowercase 'mousedown', got: {}",
        code
    );
}

#[test]
fn test_dom_dynamic_text_child() {
    let code = transform_dom(r#"<div>{count()}</div>"#);
    assert!(code.contains("insert"));
    assert!(
        code.contains(", count)") || code.contains(", count,"),
        "Expected normalized accessor identifier insert, got:\n{code}"
    );
}

#[test]
fn test_dom_uid_numbering_matches_babel_style() {
    let code = transform_dom(r#"<div>Hello {name()}!</div>"#);
    assert!(
        code.contains("_el$ ="),
        "Expected first generated element UID without numeric suffix, got:\n{code}"
    );
    assert!(
        code.contains("_el$2"),
        "Expected second generated element UID with '2' suffix, got:\n{code}"
    );
    assert!(
        !code.contains("_el$1"),
        "Should not emit '$1' suffix for first generated UID, got:\n{code}"
    );
}

#[test]
fn test_dom_multiple_dynamic_children() {
    let code = transform_dom(r#"<div>{a()}{b()}</div>"#);
    assert!(
        code.contains(", a)") || code.contains(", a,"),
        "Expected normalized first accessor identifier insert, got:\n{code}"
    );
    assert!(
        code.contains(", b)") || code.contains(", b,"),
        "Expected normalized second accessor identifier insert, got:\n{code}"
    );
}

#[test]
fn test_dom_insert_wrap_conditionals_with_non_inline_memo() {
    let code = transform_dom(r#"<div>{state.dynamic ? good() : bad}</div>"#);

    assert!(
        code.contains("memo(() => !!state.dynamic)"),
        "Expected conditional test memoization for insert() child, got:\n{code}"
    );
    assert!(
        code.contains("var _c$") && code.contains("return () => _c$"),
        "Expected non-inline conditional wrapper shape for insert() child, got:\n{code}"
    );
}

#[test]
fn test_dom_component_getter_wrap_conditionals_inline() {
    let code = transform_dom(r#"<Comp render={state.dynamic ? good() : bad} />"#);

    assert!(
        code.contains("memo(() => !!state.dynamic)() ? good() : bad"),
        "Expected inline conditional memoization inside component getter, got:\n{code}"
    );
}

#[test]
fn test_dom_wrap_conditionals_option_false_skips_conditional_memo_wrapping() {
    let code = transform_dom_with_options(
        r#"<div>{state.dynamic ? good() : bad}</div>"#,
        TransformOptions {
            wrap_conditionals: false,
            ..TransformOptions::solid_defaults()
        },
    );

    assert!(
        !code.contains("memo(() => !!state.dynamic)"),
        "wrap_conditionals=false should skip conditional memoization, got:\n{code}"
    );
}

#[test]
fn test_dom_wrap_conditionals_empty_memo_wrapper_skips_insert_conditional_memo_wrapping() {
    let code = transform_dom_with_options(
        r#"<div>{state.dynamic ? good() : bad}</div>"#,
        TransformOptions {
            wrap_conditionals: true,
            memo_wrapper: "",
            ..TransformOptions::solid_defaults()
        },
    );

    assert!(
        !code.contains("memo(() => !!state.dynamic)")
            && !code.contains("_$memo(")
            && !code.contains("memo as _$memo"),
        "empty memo_wrapper should skip conditional memoization in insert path, got:\n{code}"
    );
    assert!(
        code.contains("state.dynamic ? good() : bad"),
        "conditional expression should remain in insert accessor when memo_wrapper is empty, got:\n{code}"
    );
}

#[test]
fn test_dom_wrap_conditionals_empty_memo_wrapper_skips_component_getter_conditional_memo_wrapping()
{
    let code = transform_dom_with_options(
        r#"<Comp render={state.dynamic ? good() : bad} />"#,
        TransformOptions {
            wrap_conditionals: true,
            memo_wrapper: "",
            ..TransformOptions::solid_defaults()
        },
    );

    assert!(
        !code.contains("memo(() => !!state.dynamic)")
            && !code.contains("_$memo(")
            && !code.contains("memo as _$memo"),
        "empty memo_wrapper should skip conditional memoization in component getter path, got:\n{code}"
    );
    assert!(
        code.contains("state.dynamic ? good() : bad"),
        "conditional expression should remain in component getter when memo_wrapper is empty, got:\n{code}"
    );
}

#[test]
fn test_dom_wrap_conditionals_wrapperless_innerhtml_omits_effect_wrapper() {
    let code = transform_dom_with_options(
        r#"<div innerHTML={state.dynamic ? good() : bad()} />"#,
        wrapperless_dom_options(),
    );

    assert!(
        !code.contains("import { effect as _$effect }"),
        "wrapperless options should not import effect helper, got:\n{code}"
    );
    assert!(
        !code.contains("_$effect("),
        "wrapperless options should not emit effect helper calls, got:\n{code}"
    );
}

#[test]
fn test_dom_wrap_conditionals_wrapperless_fragment_dynamic_expression_not_memo_wrapped() {
    let code = transform_dom_with_options(
        r#"<>{state.dynamic ? good() : bad}</>"#,
        wrapperless_dom_options(),
    );

    assert!(
        !code.contains("_$memo(") && !code.contains("memo("),
        "wrapperless options should not memo-wrap fragment dynamic expressions, got:\n{code}"
    );
    assert!(
        code.contains("state.dynamic") && code.contains("good()") && code.contains("bad"),
        "expected conditional fragment expression to remain present, got:\n{code}"
    );
}

#[test]
fn test_dom_require_import_source_matching_pragma_transforms_jsx() {
    let code = transform_dom_with_options(
        r#"
        /** @jsxImportSource r-dom */
        const template = <div>Hello</div>;
        "#,
        require_import_source_dom_options(),
    );

    assert_has_transformed_template_patterns(&code);
}

#[test]
fn test_dom_require_import_source_without_pragma_skips_transform() {
    let code = transform_dom_with_options(
        r#"
        const template = <div>Hello</div>;
        "#,
        require_import_source_dom_options(),
    );

    assert_keeps_raw_jsx_patterns(&code);
}

#[test]
fn test_dom_require_import_source_non_matching_pragma_skips_transform() {
    let code = transform_dom_with_options(
        r#"
        /** @jsxImportSource jsx-dom */
        const template = <div>Hello</div>;
        "#,
        require_import_source_dom_options(),
    );

    assert_keeps_raw_jsx_patterns(&code);
}

#[test]
fn test_dom_require_import_source_inexact_substring_pragma_skips_transform() {
    let code = transform_dom_with_options(
        r#"
        /** @jsxImportSource xxr-domxx */
        const template = <div>Hello</div>;
        "#,
        require_import_source_dom_options(),
    );

    assert_keeps_raw_jsx_patterns(&code);
}

#[test]
fn test_dom_insert_wrap_conditionals_preserves_or_fallback_shape() {
    let code = transform_dom(r#"<div>{(state.dynamic && good()) || bad}</div>"#);

    assert!(
        code.contains("memo(() => !!state.dynamic)"),
        "Expected logical-AND predicate memoization in (a && b) || c form, got:\n{code}"
    );
    assert!(
        code.contains("(_c$() ? good() : state.dynamic) || bad")
            || code.contains("(_c$2() ? good() : state.dynamic) || bad"),
        "Expected wrapped && branch to preserve || fallback value shape, got:\n{code}"
    );
}

#[test]
fn test_dom_insert_wrap_conditionals_preserves_nullish_chain_shape() {
    let code = transform_dom(r#"<div>{(thing() && thing1()) ?? thing2() ?? thing3()}</div>"#);

    assert!(
        code.contains("memo(() => !!thing())"),
        "Expected nullish-chain left && predicate memoization, got:\n{code}"
    );
    assert!(
        code.contains("(_c$() ? thing1() : thing()) ?? thing2() ?? thing3()")
            || code.contains("(_c$2() ? thing1() : thing()) ?? thing2() ?? thing3()"),
        "Expected wrapped && segment to preserve fallback value inside ?? chain, got:\n{code}"
    );
}

#[test]
fn test_dom_component_wrap_conditionals_skips_plain_or_expression() {
    let code = transform_dom(r#"<Comp render={state.dynamic || good()} />"#);

    assert!(
        !code.contains("memo(() => !!state.dynamic)"),
        "Plain || expression should not memoize predicate under wrapConditionals parity, got:\n{code}"
    );
    assert!(
        code.contains("return state.dynamic || good()")
            || code.contains("return state.dynamic || good"),
        "Expected component getter to keep plain || shape, got:\n{code}"
    );
}

#[test]
fn test_dom_component_wrap_conditionals_nested_ternary_memoizes_nested_tests() {
    let code = transform_dom(
        r#"<Comp value={state.a ? "a" : state.b ? "b" : state.c ? "c" : "fallback"} />"#,
    );

    assert!(
        code.contains("memo(() => !!state.a)()"),
        "Expected outer ternary predicate memoization, got:\n{code}"
    );
    assert!(
        code.contains("memo(() => !!state.b)() ? \"b\"")
            || code.contains("memo(() => !!state.b)() ? \"b\" : state.c ? \"c\" : \"fallback\""),
        "Expected nested ternary predicate memoization for state.b, got:\n{code}"
    );
}

#[test]
fn test_dom_mixed_children() {
    let code = transform_dom(r#"<div>Hello {name()}!</div>"#);
    // Static text in template, dynamic inserted
    assert!(code.contains("insert"));
    assert!(
        code.contains(", name)") || code.contains(", name,"),
        "Expected normalized accessor identifier insert, got:\n{code}"
    );
}

#[test]
fn test_dom_native_element_unwraps_simple_identifier_call_child() {
    let code = transform_dom(r#"<module>{children()}</module>"#);
    assert!(code.contains("insert"), "Output was:\n{code}");
    assert!(
        code.contains(", children)"),
        "Expected children identifier insert argument, got:\n{code}"
    );
    assert!(
        !code.contains("() => children()"),
        "Simple call child should be normalized to identifier, got:\n{code}"
    );
}

#[test]
fn test_dom_native_element_keeps_member_call_wrapped() {
    let code = transform_dom(r#"<module>{state.children()}</module>"#);
    assert!(
        code.contains("() => state.children()"),
        "Member call child should stay wrapped, got:\n{code}"
    );
}

#[test]
fn test_dom_component_mixed_children_memo_wraps_dynamic_entry() {
    let code = transform_dom(r#"<Child><div />{state.dynamic}</Child>"#);
    assert!(code.contains("get children()"), "Output was:\n{code}");
    assert!(
        code.contains("memo(() => state.dynamic)"),
        "Expected mixed component children to memo-wrap dynamic entry, got:\n{code}"
    );
}

#[test]
fn test_dom_component_explicit_children_prop_is_overridden_by_jsx_children() {
    let code = transform_dom(r#"<Child children={props.child}>From JSX</Child>"#);
    assert!(
        !code.contains("props.child"),
        "Explicit children prop should be ignored when JSX children are present, got:\n{code}"
    );
    assert!(
        code.contains("children: \"From JSX\"") || code.contains("return \"From JSX\""),
        "Expected JSX children payload to win, got:\n{code}"
    );
}

#[test]
fn test_dom_fragment_single_dynamic_call_uses_direct_identifier_memo() {
    let code = transform_dom(r#"<>{inserted()}</>"#);
    assert!(
        code.contains("memo(inserted)"),
        "Expected fragment call child to normalize to memo(identifier), got:\n{code}"
    );
    assert!(
        !code.contains("memo(() => inserted())"),
        "Fragment call child should not keep wrapped call inside memo, got:\n{code}"
    );
}

#[test]
fn test_dom_fragment_single_static_expression_not_memoized() {
    let code = transform_dom(r#"<>{inserted}</>"#);
    assert!(
        !code.contains("memo("),
        "Static fragment expression should not be memoized, got:\n{code}"
    );
}

#[test]
fn test_dom_fragment_multi_child_memo_wraps_dynamic_expression_entries() {
    let code = transform_dom(r#"<><div />{state.inserted}</>"#);
    assert!(
        code.contains("memo(() => state.inserted)"),
        "Expected fragment array dynamic entry to be memo-wrapped, got:\n{code}"
    );
}

#[test]
fn test_dom_insert_children_static_marker_keeps_direct_value_forms() {
    let code = transform_dom(
        r#"
        const template11 = <module children={/*@static*/ state.children} />;
        const template12 = <Module children={/*@static*/ state.children} />;
        "#,
    );
    assert!(
        code.contains("insert") && code.contains("state.children"),
        "Expected native children insert to use direct member expression, got:\n{code}"
    );
    assert!(
        !code.contains("() => state.children"),
        "Static marker children should not be wrapped in accessors, got:\n{code}"
    );
    assert!(
        code.contains("children: state.children"),
        "Expected component children prop to stay as value form, got:\n{code}"
    );
}

#[test]
fn test_dom_trailing_dynamic_call_child_uses_null_marker_insert() {
    let code = transform_dom(r#"<div><div></div>{expr()}</div>"#);
    assert!(
        code.contains(", expr, null)"),
        "Expected trailing dynamic insert to use null sentinel, got:\n{code}"
    );
    assert!(
        !code.contains("() => expr()"),
        "Trailing simple call child should be normalized to identifier, got:\n{code}"
    );
}

#[test]
fn test_dom_mixed_text_inserts_before_marker() {
    let code = transform_dom(r#"<div>Hello {name()}!</div>"#);
    // Whitespace is preserved: "Hello " keeps trailing space
    assert!(
        code.contains("<div>Hello <!>!"),
        "Template should preserve space before marker, got: {}",
        code
    );
    assert!(
        code.contains(", name, _el$"),
        "Should insert normalized accessor before marker, got: {}",
        code
    );
}

#[test]
fn test_dom_nested_element_after_text_walks_next_sibling() {
    let code = transform_dom(r#"<div>Hello <span class={style()}>world</span></div>"#);
    assert!(
        code.contains(".firstChild") && code.contains(".nextSibling"),
        "Expected nested element to walk via nextSibling, got:\n{code}"
    );
    assert!(code.contains("style()") || code.contains("effect(style"));
}

#[test]
fn test_dom_component_between_elements_inserts_before_next_sibling() {
    let code = transform_dom(r#"<div><span>text</span><Counter /><p>more</p></div>"#);
    assert!(code.contains("<span>text</span>"), "Output was:\n{code}");
    assert!(
        code.contains("<p>more") || code.contains("<p>more</p>"),
        "Output was:\n{code}"
    );
    assert!(code.contains("insert("), "Output was:\n{code}");
    assert!(
        code.contains("createComponent(Counter") && !code.contains("<Counter>"),
        "Output was:\n{code}"
    );
}

#[test]
fn test_dom_namespace_elements_member_and_namespaced_tags() {
    let code = transform_dom(
        r#"
        const template = <module.A />;
        const template2 = <module.a.B />;
        const template3 = <module.A.B />;
        const template4 = <module.a-b />;
        const template5 = <module.a-b.c-d />;
        const template6 = <namespace:tag />;
        "#,
    );

    assert!(
        code.contains("createComponent(module.A"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("createComponent(module.a.B"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("createComponent(module.A.B"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("createComponent(module[\"a-b\"]"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("createComponent(module[\"a-b\"][\"c-d\"]"),
        "Output was:\n{code}"
    );
    assert!(code.contains("<namespace:tag>"), "Output was:\n{code}");
}

#[test]
fn test_dom_ref_variable() {
    let code = transform_dom(r#"<div ref={myRef}>content</div>"#);
    assert!(code.contains("myRef"));
}

#[test]
fn test_dom_ref_callback() {
    let code = transform_dom(r#"<div ref={el => setRef(el)}>content</div>"#);
    assert!(code.contains("setRef"));
}

#[test]
fn test_dom_ref_const_identifier_no_assignment_fallback() {
    let code = transform_dom(
        r#"
        const [header, setHeader] = createSignal();
        <div ref={setHeader}>content</div>
        "#,
    );
    assert!(code.contains("setHeader"), "Output was:\n{code}");
    assert!(!code.contains("setHeader=_el$"), "Output was:\n{code}");
}

#[test]
fn test_component_ref_const_identifier_passed_directly() {
    // Component refs for const bindings (signal setters) should be passed directly
    // without the typeof ternary wrapper, matching babel-plugin-jsx-dom-expressions.
    let code = transform_dom(
        r#"
        const [ref, setRef] = createSignal();
        const Child = (p) => p;
        <Child ref={setRef}>content</Child>
        "#,
    );
    eprintln!("=== Component ref const output ===\n{code}\n===");
    assert!(
        code.contains("ref: setRef"),
        "Expected direct ref pass, output was:\n{code}"
    );
    assert!(
        !code.contains("typeof"),
        "Should not have typeof check for const ref, output was:\n{code}"
    );
}

#[test]
fn test_component_ref_signal_setter_realistic() {
    // Realistic pattern from agents-sidebar.tsx
    let code = transform_dom(
        r#"
        import { createSignal } from "solid-js";
        const [searchInputRef, setSearchInputRef] = createSignal(null);
        const Input = (p) => p;
        <Input ref={setSearchInputRef} placeholder="Search" />
        "#,
    );
    eprintln!("=== Realistic component ref output ===\n{code}\n===");
    assert!(
        !code.contains("typeof"),
        "Should not have typeof check for const signal setter ref, output was:\n{code}"
    );
    assert!(
        !code.contains("setSearchInputRef ="),
        "Should not assign to const, output was:\n{code}"
    );
}

#[test]
fn test_component_ref_let_variable_gets_ternary() {
    // Component refs for let variables should still get the typeof ternary.
    let code = transform_dom(
        r#"
        let childRef;
        const Child = (p) => p;
        <Child ref={childRef}>content</Child>
        "#,
    );
    assert!(
        code.contains("typeof"),
        "Expected typeof check for let ref, output was:\n{code}"
    );
}

#[test]
fn test_component_ref_arrow_function_passed_directly() {
    // Arrow function refs should be passed directly.
    let code = transform_dom(
        r#"
        let el;
        const Child = (p) => p;
        <Child ref={e => el = e}>content</Child>
        "#,
    );
    assert!(
        !code.contains("typeof"),
        "Should not have typeof check for arrow function ref, output was:\n{code}"
    );
}

#[test]
fn test_dom_does_not_duplicate_existing_solid_web_imports() {
    let code = transform_dom(
        r#"
        import { mergeProps } from "solid-js/web";
        const props = {};
        const Comp = (p) => p;
        <Comp {...props} a={1} />
        "#,
    );

    assert!(
        code.contains("import { mergeProps } from \"solid-js/web\";"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("import { createComponent as _$createComponent } from \"solid-js/web\";"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("import { mergeProps as _$mergeProps } from \"solid-js/web\";"),
        "Output was:\n{code}"
    );
    assert_eq!(
        code.matches("import { mergeProps as _$mergeProps } from \"solid-js/web\";")
            .count(),
        1,
        "Output was:\n{code}"
    );
}

#[test]
fn test_dom_does_not_duplicate_mergeprops_from_solid_js() {
    // mergeProps can be imported from "solid-js" (re-export) instead of "solid-js/web".
    // Keep the existing import and add one aliased helper import for DOM helper usage.
    let code = transform_dom(
        r#"
        import { mergeProps } from "solid-js";
        const props = {};
        const Comp = (p) => p;
        <Comp {...props} a={1} />
        "#,
    );
    assert!(
        code.contains("import { mergeProps } from \"solid-js\";"),
        "Should preserve the existing mergeProps import from solid-js. Output was:\n{code}"
    );
    assert!(
        code.contains("import { mergeProps as _$mergeProps } from \"solid-js/web\";"),
        "Should add aliased helper import from solid-js/web. Output was:\n{code}"
    );
    assert_eq!(
        code.matches("import { mergeProps as _$mergeProps } from \"solid-js/web\";")
            .count(),
        1,
        "Should not duplicate aliased mergeProps helper import. Output was:\n{code}"
    );
}

#[test]
fn test_dom_namespace_import_from_solid_web_adds_separate_helper_import() {
    let code = transform_dom(
        r#"
        import * as Solid from "solid-js/web";
        <div>{count()}</div>
        "#,
    );

    assert!(
        !code.contains("* as Solid, {") && !code.contains("* as Solid , {"),
        "Should not merge named helpers into namespace import. Output was:\n{code}"
    );
    assert!(
        code.contains("import * as Solid from \"solid-js/web\";"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("import { template as _$template } from \"solid-js/web\";"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("import { insert as _$insert } from \"solid-js/web\";"),
        "Output was:\n{code}"
    );
    assert_eq!(
        code.matches("solid-js/web").count(),
        3,
        "Expected namespace import + one import per helper. Output was:\n{code}"
    );
}

#[test]
fn test_dom_style_string() {
    let code = transform_dom(r#"<div style="color: red">content</div>"#);
    assert!(code.contains("style=color:red") || code.contains("style=\"color: red\""));
}

#[test]
fn test_dom_style_object_static() {
    let code = transform_dom(r#"<div style={{ color: 'red', fontSize: 14 }}>content</div>"#);
    // Depending on optimization stage this can be inlined or emitted as style property setters.
    let inlined = (code.contains("color:red") || code.contains("color: red"))
        && (code.contains("font-size:14") || code.contains("font-size: 14"));
    let runtime_setters = code.contains("setStyleProperty")
        && (code.contains("\"color\"") || code.contains("color"))
        && (code.contains("\"fontSize\"") || code.contains("\"font-size\""));
    assert!(inlined || runtime_setters, "Output was:\n{code}");
}

#[test]
fn test_dom_style_object_dynamic() {
    let code = transform_dom(r#"<div style={styles()}>content</div>"#);
    assert!(code.contains("style("));
    assert!(code.contains("styles()") || code.contains("effect(styles"));
}

#[test]
fn test_dom_no_inline_styles_string_uses_runtime_style_helper() {
    let code = transform_dom_with_options(
        r#"<div style="color: red" />"#,
        no_inline_styles_dom_options(),
    );
    assert!(code.contains("template(`<div>"), "Output was:\n{code}");
    assert!(
        !code.contains("style=color:red")
            && !code.contains("style=\"color: red\"")
            && !code.contains("style=\"color:red\""),
        "style string should not be inlined when inline_styles=false. Output was:\n{code}"
    );
    assert!(
        code.contains("style as _$style") && code.contains("_$style("),
        "Expected runtime style helper usage. Output was:\n{code}"
    );
    assert!(
        code.contains("effect as _$effect") && code.contains("_$effect("),
        "Expected effect wrapper for no-inline-style runtime updates. Output was:\n{code}"
    );
}

#[test]
fn test_dom_no_inline_styles_static_object_uses_runtime_style_helper() {
    let code = transform_dom_with_options(
        r#"<div style={{ color: "red" }} />"#,
        no_inline_styles_dom_options(),
    );
    assert!(code.contains("template(`<div>"), "Output was:\n{code}");
    assert!(
        !code.contains("style=color:red")
            && !code.contains("style=\"color:red\"")
            && !code.contains("style=\"color: red\""),
        "static style object should not be inlined when inline_styles=false. Output was:\n{code}"
    );
    assert!(
        code.contains("style as _$style") && code.contains("_$style("),
        "Expected runtime style helper usage. Output was:\n{code}"
    );
    assert!(
        code.contains("effect as _$effect") && code.contains("_$effect("),
        "Expected effect wrapper for no-inline-style runtime updates. Output was:\n{code}"
    );
    assert!(
        !code.contains("setStyleProperty"),
        "inline_styles=false should bypass setStyleProperty splitting. Output was:\n{code}"
    );
}

#[test]
fn test_dom_hydratable_dynamic_innerhtml_uses_set_property_helper() {
    let code = transform_dom_hydratable(r#"<div innerHTML={html()} />"#);
    assert!(
        code.contains("setProperty as _$setProperty"),
        "expected setProperty helper import in hydratable mode: {code}"
    );
    assert!(
        code.contains("_$setProperty(") && code.contains("\"innerHTML\""),
        "expected hydratable dynamic innerHTML update to call _$setProperty(..., \"innerHTML\", ...): {code}"
    );
    assert!(
        !code.contains(".innerHTML =") && !code.contains(".innerHTML="),
        "hydratable dynamic innerHTML path should avoid direct .innerHTML assignment: {code}"
    );
}

#[test]
fn test_dom_hydratable_static_inner_content_uses_set_property_helper() {
    let code = transform_dom_hydratable(
        r#"<><div innerHTML="<span>hi</span>" /><div textContent="hello" /><div innerText="welcome" /></>"#,
    );
    assert!(
        code.contains("setProperty as _$setProperty"),
        "expected setProperty helper import in hydratable mode: {code}"
    );
    assert!(
        code.contains("_$setProperty(")
            && code.contains("\"innerHTML\"")
            && code.contains("\"textContent\"")
            && code.contains("\"innerText\""),
        "expected hydratable static innerHTML/textContent/innerText to use _$setProperty calls: {code}"
    );
    assert!(
        !code.contains(".innerHTML =")
            && !code.contains(".innerHTML=")
            && !code.contains(".textContent =")
            && !code.contains(".textContent=")
            && !code.contains(".innerText =")
            && !code.contains(".innerText="),
        "hydratable static inner content path should avoid direct property assignment: {code}"
    );
}

#[test]
fn test_dom_hydratable_run_hydration_events_for_delegated_event() {
    let code = transform_dom_hydratable(r#"<button onClick={handle} />"#);

    assert!(
        code.contains("runHydrationEvents as _$runHydrationEvents"),
        "expected runHydrationEvents helper import in hydratable mode: {code}"
    );
    assert!(
        code.contains("_$runHydrationEvents()"),
        "expected hydratable delegated event to trigger runHydrationEvents() call: {code}"
    );
}

#[test]
fn test_dom_hydratable_run_hydration_events_for_spread() {
    let code = transform_dom_hydratable(r#"<div {...props} />"#);

    assert!(
        code.contains("runHydrationEvents as _$runHydrationEvents"),
        "expected runHydrationEvents helper import for hydratable spread: {code}"
    );
    assert!(
        code.contains("_$runHydrationEvents()"),
        "expected hydratable spread to trigger runHydrationEvents() call: {code}"
    );
}

#[test]
fn test_dom_hydratable_dynamic_checked_and_value_use_set_property_in_effects() {
    let code = transform_dom_hydratable(
        r#"<div><input type="checkbox" checked={checked()} /><input value={value()} /></div>"#,
    );

    assert!(
        code.contains("effect("),
        "expected dynamic effects in output: {code}"
    );
    assert!(
        has_set_property_call(&code, "checked") && has_set_property_call(&code, "value"),
        "expected hydratable checked/value dynamic updates to use setProperty helper calls: {code}"
    );
    assert!(
        !code.contains(".checked =")
            && !code.contains(".checked=")
            && !code.contains(".value =")
            && !code.contains(".value="),
        "hydratable checked/value dynamic paths should avoid direct assignment in callbacks: {code}"
    );
}

#[test]
fn test_dom_hydratable_dynamic_selected_and_muted_use_set_property() {
    let code = transform_dom_hydratable(
        r#"<><option selected={isSelected()} /><video muted={isMuted()} /></>"#,
    );

    assert!(
        has_set_property_call(&code, "selected") && has_set_property_call(&code, "muted"),
        "expected hydratable selected/muted dynamic updates to use setProperty helper calls: {code}"
    );
    assert!(
        !code.contains(".selected =")
            && !code.contains(".selected=")
            && !code.contains(".muted =")
            && !code.contains(".muted="),
        "hydratable selected/muted dynamic paths should avoid direct assignment: {code}"
    );
}

#[test]
fn test_dom_hydratable_dynamic_textcontent_uses_set_property_data_semantics() {
    let code = transform_dom_hydratable(r#"<div><div textContent={row.label} /></div>"#);

    assert!(
        code.contains("effect("),
        "expected dynamic effect in output: {code}"
    );
    assert!(
        has_set_property_call(&code, "data"),
        "expected hydratable dynamic textContent update to target text node data via setProperty: {code}"
    );
    assert!(
        !code.contains(".data =")
            && !code.contains(".data=")
            && !code.contains(".textContent =")
            && !code.contains(".textContent="),
        "hydratable dynamic textContent path should avoid direct property assignment: {code}"
    );
}

#[test]
fn test_dom_hydratable_dynamic_innertext_uses_set_property_helper() {
    let code = transform_dom_hydratable(r#"<div innerText={label()} />"#);

    assert!(
        has_set_property_call(&code, "innerText"),
        "expected hydratable dynamic innerText update to use setProperty helper calls: {code}"
    );
    assert!(
        !code.contains(".innerText =") && !code.contains(".innerText="),
        "hydratable dynamic innerText path should avoid direct assignment: {code}"
    );
}

#[test]
fn test_dom_hydratable_plan_1_17_server_only_element_skips_template_and_uses_argless_get_next_element(
) {
    let code = transform_dom_hydratable(r#"<div $ServerOnly><h1>Hello</h1></div>"#);

    assert!(
        code.contains("_$getNextElement()"),
        "expected hydratable $ServerOnly element path to use argless getNextElement(): {code}"
    );
    assert!(
        !code.contains("template as _$template")
            && !code.contains("template(`")
            && !code.contains("_tmpl$"),
        "expected $ServerOnly element path to avoid template helper/declaration output: {code}"
    );
    assert!(
        !code.to_ascii_lowercase().contains("$serveronly"),
        "expected $ServerOnly flag to be consumed and omitted from emitted template/output: {code}"
    );
}

#[test]
fn test_dom_hydratable_plan_1_17_server_only_component_child_getter_uses_argless_get_next_element()
{
    let code = transform_dom_hydratable(r#"<Component><div $ServerOnly /></Component>"#);

    assert!(
        code.contains("get children()") && code.contains("return _$getNextElement();"),
        "expected component children getter to return argless getNextElement() for $ServerOnly child: {code}"
    );
    assert!(
        !code.contains("_$getNextElement(_tmpl$") && !code.contains("_tmpl$"),
        "expected $ServerOnly child getter path to avoid template-argument getNextElement(): {code}"
    );
}

#[test]
fn test_dom_hydratable_plan_4_5_server_only_component_children_array_gets_next_elements() {
    let code = transform_dom_hydratable(
        r#"<Component><div $ServerOnly /><span $ServerOnly /></Component>"#,
    );

    assert!(
        code.contains("[_$getNextElement(), _$getNextElement()]"),
        "expected component children getter to return array of argless getNextElement() calls for multiple $ServerOnly children: {code}"
    );
    assert!(
        !code.contains("_tmpl$"),
        "expected multiple $ServerOnly child outputs to avoid template declarations: {code}"
    );
}

#[test]
fn test_dom_hydratable_plan_4_5_server_only_fragment_uses_argless_get_next_element() {
    let code = transform_dom_hydratable(r#"<><div $ServerOnly /></>"#);

    assert!(
        code.contains("_$getNextElement()"),
        "expected $ServerOnly fragment output to use argless getNextElement(): {code}"
    );
    assert!(
        !code.contains("_tmpl$"),
        "expected $ServerOnly fragment output to avoid template declarations: {code}"
    );
}

#[test]
fn test_dom_non_hydratable_dynamic_checked_keeps_direct_assignment() {
    let code = transform_dom(r#"<input type="checkbox" checked={checked()} />"#);

    assert!(
        code.contains(".checked =") || code.contains(".checked="),
        "non-hydratable checked dynamic update should remain a direct assignment: {code}"
    );
    assert!(
        !has_set_property_call(&code, "checked"),
        "non-hydratable checked dynamic update should not use setProperty helper: {code}"
    );
}

#[test]
fn test_dom_non_hydratable_dynamic_innerhtml_keeps_direct_property_assignment() {
    let code = transform_dom(r#"<div innerHTML={html()} />"#);
    assert!(
        code.contains(".innerHTML"),
        "non-hydratable innerHTML should remain direct property assignment: {code}"
    );
    assert!(
        !code.contains("setProperty as _$setProperty") && !code.contains("_$setProperty("),
        "non-hydratable path should not import/use setProperty helper: {code}"
    );
}

#[test]
fn test_dom_innerhtml() {
    let code = transform_dom(r#"<div innerHTML={html} />"#);
    assert!(code.contains(".innerHTML"));
    assert!(code.contains("html"));
}

#[test]
fn test_dom_textcontent() {
    let code = transform_dom(r#"<div textContent={text} />"#);
    assert!(code.contains(".textContent"));
    assert!(code.contains("text"));
}

#[test]
fn test_dom_spread() {
    let code = transform_dom(r#"<div {...props}>content</div>"#);
    assert!(code.contains("spread"));
    assert!(code.contains("props"));
}

#[test]
fn test_dom_native_children_attribute_inserts_child_content() {
    let code = transform_dom(r#"<module children={children} />"#);
    assert!(code.contains("insert("), "Output was:\n{code}");
    assert!(
        !code.contains("setAttribute") || !code.contains("\"children\""),
        "children attr should not be emitted as a DOM attribute. Output was:\n{code}"
    );
}

#[test]
fn test_dom_native_children_attribute_is_ignored_when_real_children_exist() {
    let code = transform_dom(r#"<module children={children}>Hello</module>"#);
    assert!(code.contains("Hello"), "Output was:\n{code}");
    assert!(
        !code.contains("\"children\""),
        "children attr should be ignored when JSX children are present. Output was:\n{code}"
    );
}

#[test]
fn test_dom_nested_dynamic_element() {
    let code = transform_dom(r#"<div><span class={style()}>nested</span></div>"#);
    // Should walk to nested element
    assert!(code.contains("firstChild"));
    assert!(code.contains("style()") || code.contains("effect(style"));
}

#[test]
fn test_dom_deeply_nested() {
    let code = transform_dom(r#"<div><span><a href={url()}>link</a></span></div>"#);
    // Should walk: firstChild.firstChild
    assert!(code.contains("firstChild"));
    assert!(code.contains("url()") || code.contains("effect(url"));
}

#[test]
fn test_dom_component_basic() {
    let code = transform_dom(r#"<Button />"#);
    assert!(code.contains("createComponent"));
    assert!(code.contains("Button"));
}

#[test]
fn test_dom_component_with_props() {
    let code = transform_dom(r#"<Button onClick={handler} label="Click" />"#);
    assert!(code.contains("createComponent"));
    assert!(code.contains("onClick"));
    assert!(code.contains("handler"));
    assert!(code.contains("label"));
}

#[test]
fn test_dom_component_with_children() {
    let code = transform_dom(r#"<Button>Click me</Button>"#);
    assert!(code.contains("createComponent"));
    assert!(code.contains("children"));
    assert!(code.contains("Click me"));
}

#[test]
fn test_dom_component_with_jsx_children() {
    let code = transform_dom(r#"<Button><span>icon</span> Click</Button>"#);
    assert!(code.contains("createComponent"));
    // Children should include the span template
    assert!(code.contains("template"));
}

#[test]
fn test_dom_component_nested_in_element() {
    // This is the critical test - components inside native elements
    // should be transformed with insert() + createComponent()
    let code = transform_dom(r#"<main><Counter /></main>"#);

    // Should have a template for the parent element with a placeholder marker
    assert!(
        code.contains("template"),
        "Should create template for parent element"
    );
    assert!(
        code.contains("<main>"),
        "Template should contain main element"
    );

    // The component should be transformed with createComponent
    assert!(
        code.contains("createComponent"),
        "Should use createComponent for Counter"
    );
    assert!(
        code.contains("Counter"),
        "Should reference Counter component"
    );

    // Should use insert() to place the component in the DOM
    assert!(
        code.contains("insert("),
        "Should use insert() for dynamic component child"
    );

    // Should NOT have <Counter> as literal HTML in the template
    assert!(
        !code.contains("<Counter>"),
        "Counter should NOT be literal HTML in template"
    );
}

#[test]
fn test_dom_multiple_components_nested_in_element() {
    let code = transform_dom(r#"<div><Header /><Content /><Footer /></div>"#);

    // Should create template with placeholder
    assert!(code.contains("template"));
    assert!(code.contains("<div>"));

    // All components should be transformed
    assert!(code.contains("createComponent"));

    // Should have multiple insert calls
    let insert_count = code.matches("insert(").count();
    assert!(
        insert_count >= 3,
        "Should have insert() for each component, found {}",
        insert_count
    );

    // Components should NOT be literal HTML
    assert!(!code.contains("<Header>"));
    assert!(!code.contains("<Content>"));
    assert!(!code.contains("<Footer>"));
}

#[test]
fn test_dom_mixed_elements_and_components() {
    let code = transform_dom(r#"<div><span>text</span><Counter /><p>more</p></div>"#);

    // Native elements should be in template
    assert!(code.contains("<span>text</span>"));
    assert!(code.contains("<p>more") || code.contains("<p>more</p>"));

    // Component should use createComponent + insert
    assert!(code.contains("createComponent"));
    assert!(code.contains("Counter"));
    assert!(code.contains("insert("));

    // Counter should NOT be literal HTML
    assert!(!code.contains("<Counter>"));
}

#[test]
fn test_dom_deeply_nested_component() {
    // Component nested multiple levels deep
    let code = transform_dom(r#"<div><main><Counter /></main></div>"#);

    assert!(code.contains("<div><main>"), "Output was:\n{code}");
    assert!(
        code.contains("firstChild"),
        "Should walk to nested parent element"
    );

    // Should insert the component
    assert!(code.contains("createComponent"));
    assert!(code.contains("Counter"));
    assert!(code.contains("insert("));

    // Counter should NOT be literal HTML
    assert!(!code.contains("<Counter>"));
}

#[test]
fn test_dom_very_deeply_nested_component() {
    let code = transform_dom(r#"<div><section><article><MyComponent /></article></section></div>"#);

    assert!(
        code.contains("<div><section><article>"),
        "Template should include nested element chain"
    );

    // Should walk through nested elements
    assert!(
        code.contains("firstChild.firstChild"),
        "Should walk through multiple levels"
    );

    // Should use createComponent + insert
    assert!(code.contains("createComponent"));
    assert!(code.contains("MyComponent"));
    assert!(code.contains("insert("));

    // Component should NOT be literal HTML
    assert!(!code.contains("<MyComponent>"));
}

#[test]
fn test_dom_for() {
    let code = transform_dom(r#"<For each={items}>{item => <div>{item}</div>}</For>"#);
    assert!(code.contains("createComponent"));
    assert!(code.contains("For"));
    assert!(
        code.contains("get each()") || code.contains("each:"),
        "Output was:\n{code}"
    );
    assert!(code.contains("items"));
}

#[test]
fn test_dom_show() {
    let code = transform_dom(r#"<Show when={visible}><div>shown</div></Show>"#);
    assert!(code.contains("createComponent"));
    assert!(code.contains("Show"));
    assert!(
        code.contains("get when()") || code.contains("when:"),
        "Output was:\n{code}"
    );
    assert!(code.contains("visible"));
    assert!(
        code.contains("_tmpl$()"),
        "Show children should instantiate via _tmpl$(). Output was:\n{code}"
    );
    assert!(
        !code.contains("cloneNode(true)"),
        "Show children should not use cloneNode(true). Output was:\n{code}"
    );
}

#[test]
fn test_dom_show_with_fallback() {
    let code = transform_dom(
        r#"<Show when={visible} fallback={<div>hidden</div>}><div>shown</div></Show>"#,
    );
    assert!(code.contains("Show"));
    assert!(code.contains("get fallback()"));
    assert!(
        code.contains("_tmpl$()"),
        "Show fallback/children JSX should instantiate via _tmpl$(). Output was:\n{code}"
    );
    assert!(
        !code.contains("cloneNode(true)"),
        "Show fallback/children JSX should not use cloneNode(true). Output was:\n{code}"
    );
}

#[test]
fn test_dom_show_with_event_child() {
    let code =
        transform_dom(r#"<Show when={visible}><button onClick={handler}>ok</button></Show>"#);
    assert!(code.contains("Show"));
    assert!(
        code.contains("$$click") || code.contains("addEvent") && code.contains("\"click\""),
        "Event handler should compile to delegated assignment or delegated listener fallback. Output was:\n{code}"
    );
    assert!(
        code.contains("return _el$"),
        "Show child should return the created element (not just a side effect)"
    );
}

#[test]
fn test_dom_switch_match() {
    let code =
        transform_dom(r#"<Switch><Match when={a}>A</Match><Match when={b}>B</Match></Switch>"#);
    assert!(code.contains("Switch"));
    assert!(code.contains("Match"));
}

#[test]
fn test_dom_index() {
    let code = transform_dom(r#"<Index each={items}>{(item, i) => <div>{i()}</div>}</Index>"#);
    assert!(code.contains("Index"));
    assert!(
        code.contains("get each()") || code.contains("each:"),
        "Output was:\n{code}"
    );
}

#[test]
fn test_dom_suspense() {
    let code =
        transform_dom(r#"<Suspense fallback={<div>Loading...</div>}><Content /></Suspense>"#);
    assert!(code.contains("Suspense"));
    assert!(code.contains("get fallback()"));
}

#[test]
fn test_dom_error_boundary() {
    let code = transform_dom(
        r#"<ErrorBoundary fallback={err => <div>{err}</div>}><Content /></ErrorBoundary>"#,
    );
    assert!(code.contains("ErrorBoundary"));
}

#[test]
fn test_ssr_static_element() {
    let code = transform_ssr(r#"<div class="hello">world</div>"#);
    // SSR should output string or ssr template
    assert!(code.contains("<div") && code.contains("</div>"));
}

#[test]
fn test_ssr_dynamic_attribute() {
    let code = transform_ssr(r#"<div class={style()}>content</div>"#);
    assert!(code.contains("_$ssr("));
    assert!(
        code.contains("escape") || code.contains("ssrClassName") || code.contains("ssrAttribute"),
        "Output was:\n{code}"
    );
    assert!(code.contains("style()"));
}

#[test]
fn test_ssr_dynamic_child() {
    let code = transform_ssr(r#"<div>{count()}</div>"#);
    assert!(code.contains("_$ssr("));
    assert!(code.contains("escape"));
    assert!(code.contains("count()"));
}

#[test]
fn test_ssr_plan_3_3_dynamic_class_uses_ssr_class_name_helper() {
    let code = transform_ssr(r#"<div class={value()} />"#);

    assert!(
        code.contains("ssrClassName"),
        "dynamic class should route through ssrClassName helper: {code}"
    );
    assert!(
        !code.contains("ssrAttribute(\"class\""),
        "dynamic class should not route through generic ssrAttribute(class): {code}"
    );
}

#[test]
fn test_ssr_plan_3_3_dynamic_style_object_without_spread_uses_ssr_style_property_chain() {
    let code = transform_ssr(r#"<div style={{ color: color(), "font-size": size() }} />"#);

    assert!(
        code.contains("ssrStyleProperty(\"color:\""),
        "first style object key should route through ssrStyleProperty: {code}"
    );
    assert!(
        code.contains("ssrStyleProperty(\";font-size:\""),
        "subsequent style object keys should keep the leading `;` in ssrStyleProperty chain: {code}"
    );
    assert!(
        !code.contains("ssrStyle("),
        "style object (no spread) should not use ssrStyle helper fallback: {code}"
    );
}

#[test]
fn test_ssr_plan_3_3_dynamic_style_non_object_uses_ssr_style_helper() {
    let code = transform_ssr(r#"<div style={styles()} />"#);

    assert!(
        code.contains("ssrStyle("),
        "non-object style expression should route through ssrStyle helper: {code}"
    );
    assert!(
        code.contains("styles()"),
        "style helper input should include the original dynamic expression: {code}"
    );
    assert!(
        !code.contains("ssrStyleProperty("),
        "non-object style expression should not use ssrStyleProperty chain: {code}"
    );
}

#[test]
fn test_ssr_plan_3_3_dynamic_generic_attr_uses_ssr_attribute_with_escape_attr_flag() {
    let code = transform_ssr(r#"<div id={value()} />"#);

    assert!(
        code.contains("ssrAttribute(\"id\""),
        "dynamic generic attrs should route through ssrAttribute(name, value): {code}"
    );
    assert!(
        code.contains("escape(value(), true)") || code.contains("escape(value(),true)"),
        "dynamic generic attrs should escape with attr=true: {code}"
    );
}

#[test]
fn test_ssr_plan_3_3_dynamic_text_child_uses_escape_without_attr_flag() {
    let code = transform_ssr(r#"<div>{value()}</div>"#);

    assert!(
        code.contains("escape(value())"),
        "dynamic text child should escape without attr flag: {code}"
    );
    assert!(
        !code.contains("escape(value(), true)") && !code.contains("escape(value(),true)"),
        "dynamic text child should not use attr=true escaping: {code}"
    );
}

#[test]
fn test_ssr_plan_3_3_class_list_expression_routes_to_ssr_attribute_not_ssr_class_name() {
    let code = transform_ssr(r#"<div classList={value()} />"#);

    assert!(
        code.contains("ssrAttribute(\"classList\""),
        "classList expressions should route through generic ssrAttribute(classList, ...): {code}"
    );
    assert!(
        !code.contains("ssrClassName"),
        "classList expressions should not route through ssrClassName helper: {code}"
    );
}

#[test]
fn test_ssr_namespace_style_and_class_fold_into_canonical_attrs() {
    let code = transform_ssr(
        r#"<div style={{ "background-color": color(), ...props.style }} style:padding-top={props.top} class={{ "other-class2": undefVar }} class:my-class={props.active} class:other-class={undefVar} />"#,
    );

    assert!(
        !code.contains("style:padding-top"),
        "style namespace should be normalized before SSR emission: {code}"
    );
    assert!(
        !code.contains("class:my-class") && !code.contains("class:other-class"),
        "class namespaces should be normalized before SSR emission: {code}"
    );
    assert!(
        code.contains("\"padding-top\":") || code.contains("\"padding-top\" :"),
        "normalized style namespace key should be merged into style object: {code}"
    );
    assert!(
        code.contains("ssrClassName")
            && code.contains("\"my-class\"")
            && code.contains("\"other-class\""),
        "normalized class namespaces should flow through ssrClassName with merged keys: {code}"
    );
}

#[test]
fn test_ssr_namespace_mixed_order_matches_babel_attribute_fixture_shape() {
    let code = transform_ssr(
        r#"<div style={{ "background-color": color(), "margin-right": "40px", ...props.style }} style:padding-top={props.top} class:my-class={props.active} class:other-class={undefVar} class={{ "other-class2": undefVar }} />"#,
    );

    assert!(
        !code.contains("class:my-class") && !code.contains("class:other-class"),
        "reserved class namespace prefixes should be stripped in emitted attributes: {code}"
    );
    assert!(
        code.contains("ssrAttribute(\"other-class\""),
        "Babel fixture shape keeps trailing class namespace as ssrAttribute(other-class): {code}"
    );
    assert!(
        !code.contains("\"my-class\"") && !code.contains("\"other-class2\""),
        "mixed-order class namespace/object entries should not survive into ssrClassName in this fixture shape: {code}"
    );
    assert!(
        code.contains("\"padding-top\":") || code.contains("\"padding-top\" :"),
        "style namespace should still merge into the style object: {code}"
    );
}

#[test]
fn test_ssr_hydratable_spread_single_child_uses_function_payload() {
    let code = transform_ssr_hydratable(r#"<div start="Hi" middle={middle} {...spread}>Hi</div>"#);
    assert!(code.contains("ssrElement(\"div\""), "Output was:\n{code}");
    assert!(
        code.contains("() => \"Hi\""),
        "Hydratable spread children should be wrapped in a function slot: {code}"
    );
}

#[test]
fn test_ssr_hydratable_spread_multi_child_includes_markers_and_array_payload() {
    let code = transform_ssr_hydratable(
        r#"<label {...api()}><span {...api()}>Input is {api() ? "checked" : "unchecked"}</span><input {...api()} /><div {...api()} /></label>"#,
    );

    assert!(
        code.contains("ssrElement(\"label\", api()"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("() => ["),
        "Hydratable multi-children spread output should use array payload in a function slot: {code}"
    );
    assert!(
        code.contains("<!--$-->") && code.contains("<!--/-->"),
        "Dynamic child markers should be present in hydratable spread children: {code}"
    );
    assert!(
        code.contains("() => api() ? \"checked\" : \"unchecked\""),
        "Conditional spread child should preserve recursive escapeExpression shape: {code}"
    );
    assert!(
        !code.contains("() => _$escape(api() ? \"checked\" : \"unchecked\")"),
        "Conditional spread child should not be wrapped in blanket escape(...): {code}"
    );
}

#[test]
fn test_ssr_hydratable_spread_dynamic_child_and_spread_element_match_template25_shape() {
    let code = transform_ssr_hydratable(r#"<div>{props.children}<a {...props} something /></div>"#);

    assert!(
        code.contains("<!--$-->") && code.contains("<!--/-->"),
        "Hydratable spread+dynamic mixed children should emit marker template chunks: {code}"
    );
    assert!(
        code.contains("ssrRunInScope(() => _$escape(props.children))"),
        "Dynamic child in mixed spread payload should be hoisted through ssrRunInScope escape: {code}"
    );
    assert!(
        code.contains("ssrElement(\"a\"")
            || code.contains("ssrElement(\"a\", _$mergeProps(props, { something: true }), undefined, false)"),
        "Nested spread element child payload should retain ssrElement(..., undefined, false) shape: {code}"
    );
}

#[test]
fn test_ssr_escape_recursion_binary_logical_shape_matches_babel() {
    let code = transform_ssr(r#"<div disabled={"t" in test}>{"t" in test && "true"}</div>"#);

    assert!(
        code.contains("ssrRunInScope(() => \"t\" in test && \"true\")"),
        "Logical && child should preserve Babel escape recursion shape: {code}"
    );
    assert!(
        code.contains("ssrAttribute(\"disabled\", \"t\" in _$escape(test, true))"),
        "Binary `in` attribute should only escape RHS identifier recursively: {code}"
    );
    assert!(
        !code.contains("ssrRunInScope(() => _$escape(\"t\" in test && \"true\"))"),
        "Logical && child should not be blanket-wrapped in escape(...): {code}"
    );
}

#[test]
fn test_ssr_escape_recursion_unary_passthrough_shape_matches_babel() {
    let code = transform_ssr(r#"<div attribute={!!someValue}>{!!someValue}</div>"#);

    assert!(
        code.contains("ssrAttribute(\"attribute\", !!someValue)"),
        "Unary attribute values should pass through without blanket escaping: {code}"
    );
    assert!(
        !code.contains("_$escape(!!someValue"),
        "Unary expression should not be wrapped in escape(...): {code}"
    );
}

#[test]
fn test_ssr_component() {
    let code = transform_ssr(r#"<Button onClick={handler}>Click</Button>"#);
    assert!(code.contains("createComponent"));
    assert!(code.contains("Button"));
}

#[test]
fn test_ssr_for() {
    let code = transform_ssr(r#"<For each={items}>{item => <li>{item}</li>}</For>"#);
    assert!(code.contains("For"));
    assert!(
        code.contains("get each()") || code.contains("each:"),
        "Output was:\n{code}"
    );
}

#[test]
fn test_empty_fragment() {
    let code = transform_dom(r#"<></>"#);
    // Empty fragment should produce minimal output
    assert!(!code.is_empty());
}

#[test]
fn test_fragment_with_children() {
    let code = transform_dom(r#"<><div>a</div><div>b</div></>"#);
    assert!(code.contains("template"));
}

#[test]
fn test_fragment_single_dynamic_identifier_vs_expression_memo_parity() {
    let code = transform_dom(
        r#"
        const singleExpression = <>{inserted}</>;
        const singleDynamic = <>{inserted()}</>;
        "#,
    );

    assert!(
        code.contains("const singleExpression = inserted"),
        "single identifier fragment child should not be memoized, got:\n{code}"
    );
    assert!(
        code.contains("const singleDynamic = _$memo(inserted)"),
        "single call-expression fragment child should memoize callee identifier, got:\n{code}"
    );
}

#[test]
fn test_fragment_multiple_root_elements_declare_el_bindings() {
    // Regression: multi-root fragments must not merge into a single template output
    // (template() only returns the first root), and must not reference undeclared _el$ bindings.
    let code = transform_dom(r#"<><div class={a()}></div><div class={b()}></div></>"#);

    assert!(
        code.contains("a()") || code.contains("effect(a"),
        "Output was:\n{code}"
    );
    assert!(
        code.contains("b()") || code.contains("effect(b"),
        "Output was:\n{code}"
    );

    // Collect all _el$<n> references and ensure each has a corresponding `const _el$<n>`.
    let mut referenced: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let bytes = code.as_bytes();
    let mut i = 0usize;
    while i + 4 < bytes.len() {
        if bytes[i] == b'_' && bytes[i + 1] == b'e' && bytes[i + 2] == b'l' && bytes[i + 3] == b'$'
        {
            let mut j = i + 4;
            if j < bytes.len() && bytes[j].is_ascii_digit() {
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                referenced.insert(code[i..j].to_string());
                i = j;
                continue;
            }
        }
        i += 1;
    }

    for id in referenced {
        assert!(
            code.contains(&format!("const {id}")) || code.contains(&format!("var {id}")),
            "Expected declaration for {id} in output:\n{code}"
        );
    }
}

#[test]
fn test_svg_element() {
    let code = transform_dom(r#"<svg><circle cx="50" cy="50" r="40" /></svg>"#);
    assert!(code.contains("svg"));
    assert!(code.contains("circle"));
}

#[test]
fn test_custom_element() {
    let code = transform_dom(r#"<my-element attr="value">content</my-element>"#);
    assert!(code.contains("my-element"));
}

#[test]
fn test_custom_element_owner_assignment_after_static_attrs_and_prop() {
    let code = transform_dom(
        r#"<my-element some-attr={name} notProp={data} my-attr={data} prop:someProp={data} />"#,
    );

    assert_substring_order(
        &code,
        "_$setAttribute(_el$, \"some-attr\", name)",
        "_el$._$owner = _$getOwner()",
        "static custom-element some-attr before owner assignment",
    );
    assert_substring_order(
        &code,
        "_$setAttribute(_el$, \"notProp\", data)",
        "_el$._$owner = _$getOwner()",
        "static custom-element notProp before owner assignment",
    );
    assert_substring_order(
        &code,
        "_$setAttribute(_el$, \"my-attr\", data)",
        "_el$._$owner = _$getOwner()",
        "static custom-element my-attr before owner assignment",
    );
    assert_substring_order(
        &code,
        "_el$.someProp = data",
        "_el$._$owner = _$getOwner()",
        "static custom-element prop: assignment before owner assignment",
    );
}

#[test]
fn test_custom_element_owner_assignment_before_dynamic_effect_bindings() {
    let code = transform_dom(
        r#"<my-element some-attr={state.name} notProp={state.data} my-attr={state.data} prop:someProp={state.data} />"#,
    );

    assert_substring_order(
        &code,
        "_el$._$owner = _$getOwner()",
        "_$effect(",
        "dynamic custom-element owner assignment before effect binding",
    );
}

#[test]
fn test_namespaced_attribute() {
    let code = transform_dom(
        r##"<svg xmlns:xlink="http://www.w3.org/1999/xlink"><use xlink:href="#id" /></svg>"##,
    );
    assert!(code.contains("xlink:href"));
}

#[test]
fn test_whitespace_handling() {
    let code = transform_dom(
        r#"<div>
        hello
        world
    </div>"#,
    );
    // Should handle whitespace appropriately
    assert!(code.contains("hello"));
}

#[test]
fn test_special_characters() {
    let code = transform_dom(r#"<div>&amp; &lt; &gt;</div>"#);
    // HTML entities should be preserved or properly escaped
    assert!(!code.is_empty());
}

#[test]
fn test_dom_imports_template() {
    let code = transform_dom(r#"<div>hello</div>"#);
    assert!(code.contains("import"));
    assert!(code.contains("template"));
    assert!(code.contains("solid-js/web"));
}

#[test]
fn test_dom_imports_insert() {
    let code = transform_dom(r#"<div>{dynamic()}</div>"#);
    assert!(code.contains("insert"));
}

#[test]
fn test_dom_imports_effect() {
    let code = transform_dom(r#"<div class={dynamic()}>content</div>"#);
    assert!(code.contains("effect"));
}

#[test]
fn test_dom_imports_delegate_events() {
    let code = transform_dom(r#"<button onClick={handler}>click</button>"#);
    assert!(code.contains("delegateEvents"));
}

#[test]
fn test_ssr_imports() {
    let code = transform_ssr(r#"<div>{count()}</div>"#);
    assert!(code.contains("import"));
    assert!(code.contains("ssr"));
    assert!(code.contains("escape"));
}

#[test]
fn test_ssr_namespace_import_from_solid_web_adds_separate_helper_import() {
    let code = transform_ssr(
        r#"
        import * as Solid from "solid-js/web";
        <div>{count()}</div>
        "#,
    );

    assert!(
        !code.contains("* as Solid, {") && !code.contains("* as Solid , {"),
        "Should not merge named helpers into namespace import. Output was:\n{code}"
    );
    assert!(
        code.matches("solid-js/web").count() >= 2,
        "Expected namespace import + separate helper imports. Output was:\n{code}"
    );
    assert!(
        code.contains("import { ssr as _$ssr } from \"solid-js/web\""),
        "Expected separate SSR helper import. Output was:\n{code}"
    );
}

#[test]
fn test_ssr_namespace_import_with_member_usage_keeps_separate_helper_import() {
    let code = transform_ssr(
        r#"
        import * as Solid from "solid-js/web";
        const docType = Solid.ssr("<!DOCTYPE html>");
        const stream = () => <>{docType}{children()}</>;
        "#,
    );

    assert!(
        !code.contains("* as Solid, {") && !code.contains("* as Solid , {"),
        "Should not merge named helpers into namespace import. Output was:\n{code}"
    );
    assert!(
        code.matches("solid-js/web").count() >= 2,
        "Expected namespace import + separate helper imports. Output was:\n{code}"
    );
    assert!(
        code.contains("import { ssr as _$ssr } from \"solid-js/web\""),
        "Expected separate SSR helper import. Output was:\n{code}"
    );
}

#[test]
fn test_dom_source_map_generation() {
    let options = TransformOptions {
        filename: "input.jsx",
        source_map: true,
        ..TransformOptions::solid_defaults()
    };
    let result = transform(r#"<div>{x()}</div>"#, Some(options));
    assert!(result.map.is_some(), "expected source map to be generated");
}

#[test]
fn test_dom_respects_explicit_jsx_source_type_for_js_filename() {
    let options = TransformOptions {
        filename: "input.js",
        source_type: SourceType::jsx(),
        ..TransformOptions::solid_defaults()
    };
    let result = transform(r#"<div>{x()}</div>"#, Some(options));
    let code = normalize(&result.code);
    assert!(code.contains("template("), "Output was:\n{code}");
    assert!(code.contains("insert"), "Output was:\n{code}");
}

#[test]
fn test_ssr_source_map_generation() {
    let options = TransformOptions {
        generate: GenerateMode::Ssr,
        filename: "input.jsx",
        source_map: true,
        ..TransformOptions::solid_defaults()
    };
    let result = transform(r#"<div>{x()}</div>"#, Some(options));
    assert!(result.map.is_some(), "expected source map to be generated");
}

#[test]
fn test_dom_nested_dynamic_content() {
    // {x()} inside nested <span> should produce insert() without marker (single dynamic child)
    let code = transform_dom(r#"<div><span>{x()}</span></div>"#);

    // Template should have span without marker (single dynamic child optimization)
    assert!(
        code.contains("<span>") || code.contains("<span></span>"),
        "Template should have empty span (no marker for single dynamic child), got: {}",
        code
    );

    // Should walk to span element
    assert!(
        code.contains("firstChild"),
        "Should walk to span with firstChild, got: {}",
        code
    );

    // Should insert into span without marker argument
    assert!(
        code.contains("insert("),
        "Should have insert() call, got: {}",
        code
    );
    assert!(
        code.contains(", x)") || code.contains(", x,"),
        "Should reference normalized accessor x, got: {}",
        code
    );
}

#[test]
fn test_dom_two_siblings_with_events() {
    // Bug: second button should use firstChild.nextSibling not root.nextSibling
    let code = transform_dom(
        r#"<div><button onClick={() => 1}>A</button><button onClick={() => 2}>B</button></div>"#,
    );

    // Should have proper sibling traversal
    assert!(
        code.contains("firstChild"),
        "Should walk to first button, got: {}",
        code
    );
    // Second button should chain from the first child binding, not restart from root.
    assert!(
        code.contains("nextSibling"),
        "Should walk to second button via nextSibling, got: {}",
        code
    );
    assert!(
        !code.contains("firstChild.nextSibling"),
        "Should not walk second button from root path, got: {}",
        code
    );
}

#[test]
fn test_custom_element_children_attr_owner_before_insert() {
    let code = transform_dom(r#"<my-element children={children} />"#);
    assert_substring_order(
        &code,
        "_el$._$owner = _$getOwner()",
        "_$insert(_el$, children)",
        "custom element children attr owner-before-insert ordering",
    );
}

#[test]
fn test_slot_children_attr_owner_before_insert() {
    let code = transform_dom(r#"<slot children={children} />"#);
    assert_substring_order(
        &code,
        "_el$._$owner = _$getOwner()",
        "_$insert(_el$, children)",
        "slot children attr owner-before-insert ordering",
    );
}

#[test]
fn test_custom_element_children_attr_with_spread_orders_spread_owner_insert() {
    let code = transform_dom(r#"<my-element children={children} {...props} />"#);
    assert_substring_order(
        &code,
        "_$spread(_el$",
        "_el$._$owner = _$getOwner()",
        "custom element spread before owner ordering",
    );
    assert!(
        code.contains("children") && !code.contains("_$insert(_el$, children)"),
        "children attr with spread should be merged into spread props. Output:\n{code}"
    );
}

#[test]
fn test_dom_native_children_literal_with_spread_uses_runtime_setter_before_spread() {
    let code = transform_dom(r#"<module children="A" {...props} />"#);
    assert!(
        !code.contains("insert("),
        "literal children attr should not emit insert():\n{code}"
    );
    assert!(
        code.contains("children: \"A\"") && code.contains("mergeProps"),
        "literal children attr with spread should merge into spread props: \n{code}"
    );
}

#[test]
fn test_component_ref_ts_non_null_wrapper_is_stripped() {
    let code = transform_dom(
        r#"
        let childRef;
        const Child = (p) => p;
        <Child ref={childRef!}>content</Child>
        "#,
    );
    assert!(
        !code.contains("childRef!"),
        "TS non-null wrapper should be stripped from component ref output:\n{code}"
    );
    assert!(
        code.contains("= childRef"),
        "Expected normalized component ref target in generated ternary path:\n{code}"
    );
}

#[test]
fn test_component_ref_ts_as_wrapper_is_stripped() {
    let code = transform_dom(
        r#"
        let childRef;
        const Child = (p) => p;
        <Child ref={childRef as ((v: unknown) => unknown)}>content</Child>
        "#,
    );
    assert!(
        !code.contains("childRef as"),
        "TS as-wrapper should be stripped from component ref output:\n{code}"
    );
    assert!(
        code.contains("= childRef"),
        "Expected normalized component ref target in generated ternary path:\n{code}"
    );
}

#[test]
fn test_component_ref_ts_satisfies_wrapper_is_stripped() {
    let code = transform_dom(
        r#"
        let childRef;
        const Child = (p) => p;
        <Child ref={childRef satisfies ((v: unknown) => unknown)}>content</Child>
        "#,
    );
    assert!(
        !code.contains("childRef satisfies"),
        "TS satisfies-wrapper should be stripped from component ref output:\n{code}"
    );
    assert!(
        code.contains("= childRef"),
        "Expected normalized component ref target in generated ternary path:\n{code}"
    );
}

#[test]
fn test_dynamic_component_default_builtin_uses_create_component_dynamic_alias() {
    let code = transform_dom(r#"<Dynamic component={Comp} />"#);
    assert!(
        code.contains("Dynamic as _$Dynamic"),
        "Expected built-in Dynamic helper alias import, got:\n{code}"
    );
    assert!(
        code.contains("createComponent(_$Dynamic"),
        "Expected createComponent call with helper alias for Dynamic, got:\n{code}"
    );
    assert!(
        !code.contains("dynamicComponent("),
        "Dynamic parity should stay on createComponent path, got:\n{code}"
    );
}

#[test]
fn test_dynamic_component_when_not_builtin_uses_plain_identifier() {
    let code = transform_dom_with_options(
        r#"<Dynamic component={Comp} />"#,
        TransformOptions {
            built_ins: vec!["For", "Show"],
            ..TransformOptions::solid_defaults()
        },
    );

    assert!(
        !code.contains("Dynamic as _$Dynamic"),
        "Dynamic should not be imported as built-in when excluded from built_ins, got:\n{code}"
    );
    assert!(
        code.contains("createComponent(Dynamic"),
        "Expected plain Dynamic identifier when not in built_ins, got:\n{code}"
    );
    assert!(
        !code.contains("dynamicComponent("),
        "Dynamic parity should stay on createComponent path, got:\n{code}"
    );
}
