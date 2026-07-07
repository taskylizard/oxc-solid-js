//! Port of babel-plugin-jsx-dom-expressions to OXC.

pub use common::{RendererConfig, TransformOptions};

use common::JSX_MEMBER_DASH_SENTINEL;

#[cfg(feature = "napi")]
use napi_derive::napi;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use oxc_parser::Parser;

use std::path::PathBuf;

use dom::SolidTransform;
use solid_refresh::{
    Options as RefreshOptions, RuntimeType as RefreshRuntimeType, SolidRefreshTransform,
};
use ssr::SSRTransform;

/// Owned result of a transform operation.
pub struct OwnedTransformResult {
    /// The transformed code.
    pub code: String,
    /// Source map JSON, if source maps were enabled.
    pub map: Option<String>,
}

/// Result of a transform operation
#[cfg(feature = "napi")]
#[napi(object)]
pub struct TransformResult {
    /// The transformed code
    pub code: String,
    /// Source map (if enabled)
    pub map: Option<String>,
}

#[cfg(feature = "napi")]
#[napi(object)]
#[derive(Default)]
pub struct JsRendererConfig {
    pub name: String,
    pub module_name: String,
    pub elements: Vec<String>,
}

/// Transform options exposed to JavaScript
#[cfg(feature = "napi")]
#[napi(object)]
#[derive(Default)]
pub struct JsTransformOptions {
    /// The module to import runtime helpers from
    /// @default "solid-js/web"
    pub module_name: Option<String>,

    /// Multi-renderer configuration used by `generate: "dynamic"`
    pub renderers: Option<Vec<JsRendererConfig>>,

    /// Generate mode: "dom", "ssr", "universal", or "dynamic"
    /// @default "dom"
    pub generate: Option<String>,

    /// Whether to enable hydration support
    /// @default false
    pub hydratable: Option<bool>,

    /// Whether to delegate events
    /// @default true
    pub delegate_events: Option<bool>,

    /// Whether to wrap conditionals
    /// @default true
    pub wrap_conditionals: Option<bool>,

    /// Whether nested closing tags may be omitted in template output
    /// @default false
    pub omit_nested_closing_tags: Option<bool>,

    /// Whether the last closing tag may be omitted in template output
    /// @default true
    pub omit_last_closing_tag: Option<bool>,

    /// Whether static attributes may omit quotes when safe
    /// @default true
    pub omit_quotes: Option<bool>,

    /// Whether static styles may be inlined into template HTML
    /// @default true
    pub inline_styles: Option<bool>,

    /// Whether to pass context to custom elements
    /// @default true
    pub context_to_custom_elements: Option<bool>,

    /// Source filename
    /// @default "input.jsx"
    pub filename: Option<String>,

    /// Whether to generate source maps
    /// @default false
    pub source_map: Option<bool>,

    /// Only transform files containing a matching `@jsxImportSource <value>` comment
    /// @default undefined
    pub require_import_source: Option<String>,

    /// Whether to validate generated HTML templates for browser rewrites
    /// @default true
    pub validate: Option<bool>,

    /// Enable HMR (solid-refresh) transform
    /// @default false
    pub hmr: Option<bool>,

    /// HMR bundler type: "esm", "vite", "standard", "webpack5", "rspack-esm"
    /// @default "standard"
    pub hmr_bundler: Option<String>,

    /// Enable granular HMR (dependency tracking + signatures)
    /// @default true
    pub hmr_granular: Option<bool>,

    /// Enable JSX expression extraction for HMR
    /// @default true
    pub hmr_jsx: Option<bool>,

    /// Fix render() calls for HMR (wrap with cleanup + dispose)
    /// @default true
    pub hmr_fix_render: Option<bool>,
}

/// Transform JSX source code
#[cfg(feature = "napi")]
#[napi]
pub fn transform_jsx(source: String, options: Option<JsTransformOptions>) -> TransformResult {
    let js_options = options.unwrap_or_default();

    // Convert JS options to internal options
    let generate = match js_options.generate.as_deref() {
        Some("ssr") => common::GenerateMode::Ssr,
        Some("universal") => common::GenerateMode::Universal,
        Some("dynamic") => common::GenerateMode::Dynamic,
        _ => common::GenerateMode::Dom,
    };

    let renderers = js_options
        .renderers
        .as_ref()
        .map(|renderers| {
            renderers
                .iter()
                .map(|renderer| common::RendererConfig {
                    name: renderer.name.as_str(),
                    module_name: renderer.module_name.as_str(),
                    elements: renderer
                        .elements
                        .iter()
                        .map(|element| element.as_str())
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default();

    let options = TransformOptions {
        generate,
        renderers,
        hydratable: js_options.hydratable.unwrap_or(false),
        delegate_events: js_options.delegate_events.unwrap_or(true),
        wrap_conditionals: js_options.wrap_conditionals.unwrap_or(true),
        omit_nested_closing_tags: js_options.omit_nested_closing_tags.unwrap_or(false),
        omit_last_closing_tag: js_options.omit_last_closing_tag.unwrap_or(true),
        omit_quotes: js_options.omit_quotes.unwrap_or(true),
        inline_styles: js_options.inline_styles.unwrap_or(true),
        context_to_custom_elements: js_options.context_to_custom_elements.unwrap_or(true),
        filename: js_options.filename.as_deref().unwrap_or("input.jsx"),
        source_map: js_options.source_map.unwrap_or(false),
        require_import_source: js_options.require_import_source.as_deref(),
        validate: js_options.validate.unwrap_or(true),
        hmr: js_options.hmr.unwrap_or(false),
        hmr_bundler: js_options.hmr_bundler.as_deref().unwrap_or("standard"),
        hmr_granular: js_options.hmr_granular.unwrap_or(true),
        hmr_jsx: js_options.hmr_jsx.unwrap_or(true),
        hmr_fix_render: js_options.hmr_fix_render.unwrap_or(true),
        ..TransformOptions::solid_defaults()
    };

    let result = transform_internal(&source, &options);

    TransformResult {
        code: result.code,
        map: result.map,
    }
}

/// Internal transform function
pub fn transform(source: &str, options: Option<TransformOptions>) -> OwnedTransformResult {
    let options = options.unwrap_or_else(TransformOptions::solid_defaults);
    transform_internal(source, &options)
}

const JSX_MEMBER_HYPHEN_ERROR: &str = "Identifiers in JSX cannot contain hyphens";

fn transform_internal(source: &str, options: &TransformOptions) -> OwnedTransformResult {
    let allocator = Allocator::default();
    let source_type = options.source_type;

    // Parse the source. OXC currently errors on hyphenated JSX member identifiers
    // (`<module.a-b />`). When detected, rewrite the reported identifier span(s)
    // and reparse until all such identifiers are recovered.
    let mut current_source = source.to_string();
    let mut parsed = Parser::new(&allocator, &current_source, source_type).parse();

    for _ in 0..8 {
        if !parsed.panicked {
            break;
        }

        let mut spans: Vec<(usize, usize)> = Vec::new();

        for error in &parsed.diagnostics {
            if !error.to_string().contains(JSX_MEMBER_HYPHEN_ERROR) {
                continue;
            }

            for label in &error.labels {
                let offset = label.offset() as usize;
                let len = label.len() as usize;
                if len == 0 || offset.saturating_add(len) > current_source.len() {
                    continue;
                }

                let segment = &current_source[offset..offset + len];
                if segment.contains('-') {
                    spans.push((offset, len));
                }
            }
        }

        if spans.is_empty() {
            break;
        }

        spans.sort_unstable_by_key(|(offset, _)| *offset);
        spans.dedup();

        let mut rewritten = String::with_capacity(current_source.len());
        let mut cursor = 0usize;

        for (offset, len) in spans {
            if offset < cursor {
                continue;
            }

            rewritten.push_str(&current_source[cursor..offset]);
            let segment = &current_source[offset..offset + len];
            rewritten.push_str(&segment.replace('-', JSX_MEMBER_DASH_SENTINEL));
            cursor = offset + len;
        }

        rewritten.push_str(&current_source[cursor..]);

        if rewritten == current_source {
            break;
        }

        drop(parsed);
        current_source = rewritten;
        parsed = Parser::new(&allocator, &current_source, source_type).parse();
    }

    let mut program = parsed.program;

    let should_transform = if let Some(required_import_source) = options.require_import_source {
        program.comments.iter().any(|comment| {
            let comment_text = comment.content_span().source_text(&current_source);
            let pieces: Vec<&str> = comment_text.split("@jsxImportSource").collect();
            pieces.len() == 2 && pieces[1].trim() == required_import_source
        })
    } else {
        true
    };

    if should_transform {
        // Run the appropriate transform based on generate mode
        // SAFETY: We create a raw pointer to `options` and dereference it to get a reference
        // with an independent lifetime. This is safe because:
        // 1. `options` is borrowed for the entire duration of this function
        // 2. The reference is only used within this function's scope
        // 3. The transformers don't outlive this function
        // This pattern is used to work around Rust's borrow checker limitations with
        // multiple mutable borrows needed during AST traversal.
        let options_ref = unsafe { &*(options as *const TransformOptions) };

        match options.generate {
            common::GenerateMode::Dom => {
                let transformer = SolidTransform::new(&allocator, options_ref, &current_source);
                transformer.transform(&mut program);
            }
            common::GenerateMode::Ssr => {
                let transformer = SSRTransform::new(&allocator, options_ref, &current_source);
                transformer.transform(&mut program);
            }
            common::GenerateMode::Universal | common::GenerateMode::Dynamic => {
                let transformer = SolidTransform::new(&allocator, options_ref, &current_source);
                transformer.transform(&mut program);
            }
        }

        // Solid-refresh HMR transform (post-pass)
        if options.hmr {
            let bundler = match options.hmr_bundler {
                "esm" => RefreshRuntimeType::Esm,
                "vite" => RefreshRuntimeType::Vite,
                "webpack5" => RefreshRuntimeType::Webpack5,
                "rspack-esm" => RefreshRuntimeType::RspackEsm,
                _ => RefreshRuntimeType::Standard,
            };
            let refresh_opts = RefreshOptions {
                granular: options.hmr_granular,
                jsx: options.hmr_jsx,
                bundler,
                fix_render: options.hmr_fix_render,
                extra_create_context: Vec::new(),
                extra_render: Vec::new(),
            };
            SolidRefreshTransform::new(
                &allocator,
                &refresh_opts,
                Some(options.filename),
                &current_source,
            )
            .transform(&mut program);
        }
    }

    // Generate code
    let generated = Codegen::new()
        .with_options(CodegenOptions {
            source_map_path: if options.source_map {
                Some(PathBuf::from(options.filename))
            } else {
                None
            },
            indent_width: 2,
            indent_char: IndentChar::Space,
            ..CodegenOptions::default()
        })
        .build(&program);

    OwnedTransformResult {
        code: generated.code,
        map: generated.map.map(|map| map.to_json_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_element() {
        let source = r#"<div class="hello">world</div>"#;
        let result = transform(source, None);
        // The transform should produce valid code
        assert!(!result.code.is_empty());
    }

    #[test]
    fn test_dynamic_attribute() {
        let source = r#"<div class={style()}>content</div>"#;
        let result = transform(source, None);
        assert!(!result.code.is_empty());
    }

    #[test]
    fn test_component() {
        let source = r#"<Button onClick={handler}>Click me</Button>"#;
        let result = transform(source, None);
        assert!(!result.code.is_empty());
    }

    #[test]
    fn test_for_loop() {
        let source = r#"<For each={items}>{item => <div>{item}</div>}</For>"#;
        let result = transform(source, None);
        assert!(!result.code.is_empty());
    }

    #[test]
    fn test_ssr_basic_element() {
        let source = r#"<div class="hello">world</div>"#;
        let options = TransformOptions {
            generate: common::GenerateMode::Ssr,
            ..TransformOptions::solid_defaults()
        };
        let result = transform(source, Some(options));
        assert!(!result.code.is_empty());
    }

    #[test]
    fn test_ssr_dynamic_attribute() {
        let source = r#"<div class={style()}>content</div>"#;
        let options = TransformOptions {
            generate: common::GenerateMode::Ssr,
            ..TransformOptions::solid_defaults()
        };
        let result = transform(source, Some(options));
        assert!(!result.code.is_empty());
    }

    #[test]
    fn test_ssr_component() {
        let source = r#"<Button onClick={handler}>Click me</Button>"#;
        let options = TransformOptions {
            generate: common::GenerateMode::Ssr,
            ..TransformOptions::solid_defaults()
        };
        let result = transform(source, Some(options));
        assert!(!result.code.is_empty());
    }

    #[test]
    fn test_ssr_output_preview() {
        // Test various SSR outputs
        let cases = [
            (r#"<div class="hello">world</div>"#, "basic element"),
            (r#"<div class={style()}>content</div>"#, "dynamic class"),
            (r#"<div>{count()}</div>"#, "dynamic child"),
            (
                r#"<For each={items}>{item => <li>{item}</li>}</For>"#,
                "For loop",
            ),
            (
                r#"<Show when={visible}><div>shown</div></Show>"#,
                "Show conditional",
            ),
            (
                r#"<Button><span>icon</span> Click</Button>"#,
                "component with JSX child",
            ),
            (
                r#"<Show when={visible}><div class="content">shown</div></Show>"#,
                "Show with JSX child",
            ),
        ];

        for (source, label) in cases {
            let options = TransformOptions {
                generate: common::GenerateMode::Ssr,
                ..TransformOptions::solid_defaults()
            };
            let result = transform(source, Some(options));
            println!(
                "\n=== {} ===\nInput:  {}\nOutput: {}",
                label, source, result.code
            );
        }
    }

    #[test]
    fn test_dom_output_preview() {
        // Test various DOM outputs
        let cases = [
            (r#"<div class="hello">world</div>"#, "basic element"),
            (r#"<div class={style()}>content</div>"#, "dynamic class"),
            (r#"<div>{count()}</div>"#, "dynamic child"),
            (r#"<div onClick={handler}>click</div>"#, "event handler"),
            (
                r#"<Button onClick={handler}>Click me</Button>"#,
                "component",
            ),
            (
                r#"<Button><span>icon</span> Click</Button>"#,
                "component with JSX child",
            ),
            (
                r#"<Show when={visible}><div class="content">shown</div></Show>"#,
                "Show with JSX child",
            ),
            (
                r#"<div><span class={style()}>nested dynamic</span></div>"#,
                "nested dynamic element",
            ),
            (
                r#"<div><span onClick={handler}>nested event</span></div>"#,
                "nested event handler",
            ),
            (
                r#"<div style={{ color: 'red', fontSize: 14 }}>styled</div>"#,
                "style object",
            ),
            (
                r#"<div style={dynamicStyle()}>dynamic style</div>"#,
                "dynamic style",
            ),
            (r#"<div innerHTML={html} />"#, "innerHTML"),
        ];

        for (source, label) in cases {
            // DOM mode is the default
            let result = transform(source, None);
            println!(
                "\n=== DOM: {} ===\nInput:  {}\nOutput: {}",
                label, source, result.code
            );
        }
    }
}
