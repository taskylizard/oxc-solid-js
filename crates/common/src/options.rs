//! Transform options for the Solid JSX compiler

use oxc_span::SourceType;
use rustc_hash::FxHashSet;
use std::cell::RefCell;

#[derive(Clone, Default)]
pub struct RendererConfig<'a> {
    /// Renderer name (e.g. "dom")
    pub name: &'a str,

    /// Module to import renderer helpers from
    pub module_name: &'a str,

    /// Intrinsic tags handled by this renderer
    pub elements: Vec<&'a str>,
}

/// Configuration options for the JSX transform
#[derive(Default)]
pub struct TransformOptions<'a> {
    /// The module to import runtime helpers from
    pub module_name: &'a str,

    /// Optional multi-renderer config used by dynamic mode
    pub renderers: Vec<RendererConfig<'a>>,

    /// Require matching `@jsxImportSource <value>` pragma comment before transforming
    pub require_import_source: Option<&'a str>,

    /// Generate mode: "dom", "ssr", "universal", or "dynamic"
    pub generate: GenerateMode,

    /// Whether to enable hydration support
    pub hydratable: bool,

    /// Whether to delegate events
    pub delegate_events: bool,

    /// Custom delegated events
    pub delegated_events: Vec<&'a str>,

    /// Whether to wrap conditionals
    pub wrap_conditionals: bool,

    /// Whether nested closing tags can be omitted when parents stay open
    pub omit_nested_closing_tags: bool,

    /// Whether the last closing tag can be omitted when safe
    pub omit_last_closing_tag: bool,

    /// Whether static attribute values may omit surrounding quotes when safe
    pub omit_quotes: bool,

    /// Whether static style attributes/objects may be inlined into template HTML
    pub inline_styles: bool,

    /// Whether to pass context to custom elements
    pub context_to_custom_elements: bool,

    /// Built-in components (For, Show, etc.)
    pub built_ins: Vec<&'a str>,

    /// Effect wrapper function name
    pub effect_wrapper: &'a str,

    /// Memo wrapper function name
    pub memo_wrapper: &'a str,

    /// Source filename
    pub filename: &'a str,

    /// Source type (tsx, jsx, etc.)
    pub source_type: SourceType,

    /// Whether to generate source maps
    pub source_map: bool,

    /// Static marker comment
    pub static_marker: &'a str,

    /// Whether to validate generated HTML templates for browser rewrites
    pub validate: bool,

    /// Enable HMR (solid-refresh) transform.
    pub hmr: bool,
    /// HMR bundler type. String values: "esm", "vite", "standard", "webpack5", "rspack-esm".
    pub hmr_bundler: &'a str,
    /// Enable granular HMR (dependency tracking + signatures).
    pub hmr_granular: bool,
    /// Enable JSX expression extraction for HMR.
    pub hmr_jsx: bool,
    /// Fix render() calls for HMR (wrap with cleanup + dispose).
    pub hmr_fix_render: bool,

    /// Collected templates
    pub templates: RefCell<Vec<(String, bool)>>,

    /// Collected helper imports
    pub helpers: RefCell<FxHashSet<String>>,

    /// Collected delegated events
    pub delegates: RefCell<FxHashSet<String>>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum GenerateMode {
    #[default]
    Dom,
    Ssr,
    Universal,
    Dynamic,
}

impl<'a> TransformOptions<'a> {
    pub fn solid_defaults() -> Self {
        Self {
            module_name: "solid-js/web",
            renderers: vec![],
            require_import_source: None,
            generate: GenerateMode::Dom,
            hydratable: false,
            delegate_events: true,
            delegated_events: vec![],
            wrap_conditionals: true,
            omit_nested_closing_tags: false,
            omit_last_closing_tag: true,
            omit_quotes: true,
            inline_styles: true,
            context_to_custom_elements: true,
            built_ins: vec![
                "For",
                "Show",
                "Switch",
                "Match",
                "Suspense",
                "SuspenseList",
                "Portal",
                "Index",
                "Dynamic",
                "ErrorBoundary",
            ],
            effect_wrapper: "effect",
            memo_wrapper: "memo",
            filename: "input.jsx",
            source_type: SourceType::tsx(),
            source_map: false,
            static_marker: "@static",
            validate: true,
            hmr: false,
            hmr_bundler: "standard",
            hmr_granular: true,
            hmr_jsx: true,
            hmr_fix_render: true,
            templates: RefCell::new(vec![]),
            helpers: RefCell::new(FxHashSet::default()),
            delegates: RefCell::new(FxHashSet::default()),
        }
    }

    pub fn dynamic_dom_renderer_module_name(&self) -> Option<&'a str> {
        self.renderers
            .iter()
            .find(|renderer| renderer.name == "dom")
            .map(|renderer| renderer.module_name)
    }

    pub fn dynamic_uses_dom_renderer_for_tag(&self, tag_name: &str) -> bool {
        self.renderers.iter().any(|renderer| {
            renderer.name == "dom" && renderer.elements.iter().any(|element| *element == tag_name)
        })
    }

    pub fn should_use_universal_for_intrinsic(&self, tag_name: &str) -> bool {
        match self.generate {
            GenerateMode::Universal => true,
            GenerateMode::Dynamic => !self.dynamic_uses_dom_renderer_for_tag(tag_name),
            _ => false,
        }
    }

    /// Register a helper import
    pub fn register_helper(&self, name: &str) {
        self.helpers.borrow_mut().insert(name.to_string());
    }

    /// Register an event for delegation
    pub fn register_delegate(&self, event: &str) {
        self.delegates.borrow_mut().insert(event.to_string());
    }

    /// Push a template and return its index
    pub fn push_template(&self, template: String, is_svg: bool) -> usize {
        let mut templates = self.templates.borrow_mut();
        let index = templates.len();
        templates.push((template, is_svg));
        index
    }
}
