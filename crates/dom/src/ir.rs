//! Intermediate Representation for Solid JSX transforms
//! This IR is used to collect information during traversal
//! and then generate code in a second pass.

use indexmap::IndexSet;
use oxc_allocator::{Allocator, CloneIn};
use oxc_ast::ast::{BindingPattern, Expression, JSXChild, Statement};
use oxc_ast::AstBuilder;
use oxc_span::{GetSpanMut, Span, SPAN};
use oxc_syntax::symbol::SymbolId;
use rustc_hash::{FxBuildHasher, FxHashMap};
use std::cell::RefCell;

/// Function type for transforming child JSX elements
pub type ChildTransformer<'a, 'b> = &'b dyn Fn(&JSXChild<'a>) -> Option<TransformResult<'a>>;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    #[default]
    Dom,
    Universal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperSource {
    Base,
    Dom,
    Universal,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct HelperImport {
    pub imported: String,
    pub local: String,
    pub module: String,
}

/// The result of transforming a JSX node
#[derive(Default)]
pub struct TransformResult<'a> {
    /// Source span of the originating JSX node
    pub span: Span,

    /// The HTML template string
    pub template: String,

    /// Template with all closing tags (for SSR)
    pub template_with_closing_tags: String,

    /// Variable declarations needed
    pub declarations: Vec<Declaration<'a>>,

    /// Expressions to execute (effects, inserts, etc.)
    pub exprs: Vec<Expression<'a>>,

    /// Additional statements to execute after declarations and before expressions.
    pub statements: Vec<Statement<'a>>,

    /// Dynamic attribute bindings
    pub dynamics: Vec<DynamicBinding<'a>>,

    /// Post-expressions (run after main effects)
    pub post_exprs: Vec<Expression<'a>>,

    /// Whether this is SVG (for attribute semantics)
    pub is_svg: bool,

    /// Whether this template needs an SVG wrapper
    pub template_is_svg: bool,

    /// Whether this contains custom elements
    pub has_custom_element: bool,

    /// Whether this template should use importNode cloning
    pub is_import_node: bool,

    /// Whether this tree contains a hydratable delegated event
    pub has_hydratable_event: bool,

    /// The tag name (for native elements)
    pub tag_name: Option<String>,

    /// Output strategy for this result.
    pub output_kind: OutputKind,

    /// Whether to skip template generation
    pub skip_template: bool,

    /// The generated element ID
    pub id: Option<String>,

    /// Whether this result is just text
    pub text: bool,

    /// Whether this result needs memo() wrapping (for fragment expressions)
    pub needs_memo: bool,

    /// Individual child codes for fragments (when children need to be in an array)
    pub child_results: Vec<TransformResult<'a>>,
}

impl<'a> TransformResult<'a> {
    pub fn uses_universal_output(&self) -> bool {
        matches!(self.output_kind, OutputKind::Universal)
    }
}

/// A variable declaration
pub struct Declaration<'a> {
    pub pattern: BindingPattern<'a>,
    pub init: Expression<'a>,
}

/// A dynamic attribute binding that needs effect wrapping
pub struct DynamicBinding<'a> {
    pub elem: String,
    pub key: String,
    pub value: Expression<'a>,
    pub is_svg: bool,
    pub is_ce: bool,
    pub tag_name: String,
}

#[derive(Clone, Debug)]
pub enum StaticTextValue {
    String(String),
    Number(f64),
    Boolean(bool),
}

impl StaticTextValue {
    pub fn as_text(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Number(value) => {
                if *value == 0.0 {
                    "0".to_string()
                } else {
                    value.to_string()
                }
            }
            Self::Boolean(value) => value.to_string(),
        }
    }
}

/// Context for the current block being transformed
pub struct BlockContext<'a> {
    /// Current template string being built
    pub template: RefCell<String>,

    /// Templates collected at the file level
    pub templates: RefCell<Vec<TemplateInfo>>,

    /// Helper names needed (imported-name set, used for behavior checks)
    pub helpers: RefCell<IndexSet<String, FxBuildHasher>>,

    /// Concrete helper imports (module/imported/local) in registration order
    pub helper_imports: RefCell<IndexSet<HelperImport, FxBuildHasher>>,

    /// Helper local alias lookup keyed by (module, imported)
    helper_aliases: RefCell<FxHashMap<(String, String), String>>,

    /// Next suffix counter per imported helper name
    helper_alias_counters: RefCell<FxHashMap<String, usize>>,

    /// Delegated events
    pub delegates: RefCell<IndexSet<String, FxBuildHasher>>,

    /// Whether DOM output should hydrate existing DOM
    pub hydratable: bool,

    /// Variable counters for unique names, keyed by prefix.
    pub var_counter: RefCell<FxHashMap<String, usize>>,

    /// Compile-time constant values for identifiers (used by static child folding).
    pub constant_text_values: RefCell<FxHashMap<SymbolId, StaticTextValue>>,

    /// Whether effect-wrapper codegen is enabled.
    pub effect_wrapper_enabled: bool,

    /// Base runtime module (`options.module_name`).
    base_module_name: &'a str,

    /// DOM renderer runtime module (in dynamic mode this may differ from base).
    dom_module_name: &'a str,

    /// Universal renderer runtime module (currently same as base).
    universal_module_name: &'a str,

    /// Original source text (used for static marker comment detection).
    source_text: &'a str,

    allocator: &'a Allocator,
}

pub struct TemplateInfo {
    pub content: String,
    pub validation_content: String,
    pub is_svg: bool,
    pub use_import_node: bool,
    pub is_math_ml: bool,
    pub span: Span,
}

pub fn template_var_name(index: usize) -> String {
    if index == 0 {
        "_tmpl$".to_string()
    } else {
        format!("_tmpl${}", index + 1)
    }
}

pub fn helper_local_name(name: &str) -> String {
    format!("_${}", name)
}

pub fn helper_ident_expr<'a>(ast: AstBuilder<'a>, span: Span, name: &str) -> Expression<'a> {
    let local = helper_local_name(name);
    ast.expression_identifier(span, ast.allocator.alloc_str(&local))
}

const DOM_RUNTIME_HELPERS: &[&str] = &[
    "NoHydration",
    "addEventListener",
    "classList",
    "className",
    "delegateEvents",
    "getNextElement",
    "getNextMarker",
    "getNextMatch",
    "getOwner",
    "insert",
    "runHydrationEvents",
    "setAttribute",
    "setAttributeNS",
    "setBoolAttribute",
    "setProperty",
    "setStyleProperty",
    "spread",
    "style",
    "template",
    "use",
];

impl<'a> BlockContext<'a> {
    pub fn new(
        allocator: &'a Allocator,
        hydratable: bool,
        source_text: &'a str,
        effect_wrapper_enabled: bool,
        base_module_name: &'a str,
        dom_module_name: &'a str,
        universal_module_name: &'a str,
    ) -> Self {
        Self {
            template: RefCell::new(String::new()),
            templates: RefCell::new(Vec::new()),
            helpers: RefCell::new(IndexSet::with_hasher(FxBuildHasher)),
            helper_imports: RefCell::new(IndexSet::with_hasher(FxBuildHasher)),
            helper_aliases: RefCell::new(FxHashMap::default()),
            helper_alias_counters: RefCell::new(FxHashMap::default()),
            delegates: RefCell::new(IndexSet::with_hasher(FxBuildHasher)),
            hydratable,
            var_counter: RefCell::new(FxHashMap::default()),
            constant_text_values: RefCell::new(FxHashMap::default()),
            effect_wrapper_enabled,
            base_module_name,
            dom_module_name,
            universal_module_name,
            source_text,
            allocator,
        }
    }

    /// Generate a unique variable name using Babel-style numbering.
    ///
    /// For a given prefix, the first UID is `_{prefix}`, then `_{prefix}2`,
    /// `_{prefix}3`, and so on.
    pub fn generate_uid(&self, prefix: &str) -> String {
        let mut counters = self.var_counter.borrow_mut();
        let count = counters.entry(prefix.to_string()).or_insert(0);
        *count += 1;

        if *count == 1 {
            format!("_{}", prefix)
        } else if *count < 10 {
            format!("_{}{}", prefix, *count)
        } else if *count < 12 {
            format!("_{}{}", prefix, *count - 10)
        } else {
            format!("_{}{}", prefix, *count - 2)
        }
    }

    fn helper_module_for_source(&self, source: HelperSource) -> &'a str {
        match source {
            HelperSource::Base => self.base_module_name,
            HelperSource::Dom => self.dom_module_name,
            HelperSource::Universal => self.universal_module_name,
        }
    }

    fn default_source_for_helper(&self, name: &str) -> HelperSource {
        if self.dom_module_name != self.base_module_name && DOM_RUNTIME_HELPERS.contains(&name) {
            HelperSource::Dom
        } else {
            HelperSource::Base
        }
    }

    fn allocate_helper_local(&self, imported: &str) -> String {
        let mut counters = self.helper_alias_counters.borrow_mut();
        let entry = counters.entry(imported.to_string()).or_insert(0);
        *entry += 1;
        if *entry == 1 {
            format!("_${}", imported)
        } else {
            format!("_${}{}", imported, *entry)
        }
    }

    pub fn register_helper_with_source(&self, name: &str, source: HelperSource) -> String {
        let module = self.helper_module_for_source(source).to_string();
        let key = (module.clone(), name.to_string());
        if let Some(local) = self.helper_aliases.borrow().get(&key) {
            return local.clone();
        }

        let local = self.allocate_helper_local(name);
        self.helpers.borrow_mut().insert(name.to_string());
        self.helper_aliases.borrow_mut().insert(key, local.clone());
        self.helper_imports.borrow_mut().insert(HelperImport {
            imported: name.to_string(),
            local: local.clone(),
            module,
        });
        local
    }

    /// Register a helper import on the default source for current mode.
    pub fn register_helper(&self, name: &str) {
        let source = self.default_source_for_helper(name);
        let _ = self.register_helper_with_source(name, source);
    }

    pub fn register_dom_helper(&self, name: &str) {
        let _ = self.register_helper_with_source(name, HelperSource::Dom);
    }

    pub fn register_universal_helper(&self, name: &str) {
        let _ = self.register_helper_with_source(name, HelperSource::Universal);
    }

    pub fn helper_ident_expr_with_source(
        &self,
        ast: AstBuilder<'a>,
        span: Span,
        name: &str,
        source: HelperSource,
    ) -> Expression<'a> {
        let local = self.register_helper_with_source(name, source);
        ast.expression_identifier(span, ast.allocator.alloc_str(&local))
    }

    pub fn helper_imports_vec(&self) -> Vec<HelperImport> {
        self.helper_imports.borrow().iter().cloned().collect()
    }

    /// Register a delegated event
    pub fn register_delegate(&self, event: &str) {
        self.delegates.borrow_mut().insert(event.to_string());
    }

    /// Push a template and return its index
    pub fn push_template(
        &self,
        content: String,
        validation_content: String,
        is_svg: bool,
        use_import_node: bool,
        is_math_ml: bool,
        span: Span,
    ) -> usize {
        self.register_helper("template");
        let mut templates = self.templates.borrow_mut();

        if let Some(index) = templates.iter().position(|template| {
            template.content == content
                && template.is_svg == is_svg
                && template.is_math_ml == is_math_ml
        }) {
            if use_import_node {
                templates[index].use_import_node = true;
            }
            return index;
        }

        let index = templates.len();
        templates.push(TemplateInfo {
            content,
            validation_content,
            is_svg,
            use_import_node,
            is_math_ml,
            span,
        });
        index
    }

    pub fn ast(&self) -> AstBuilder<'a> {
        AstBuilder::new(self.allocator)
    }

    pub fn clone_expr(&self, expr: &Expression<'a>) -> Expression<'a> {
        expr.clone_in(self.allocator)
    }

    /// Clone an expression while dropping source trivia attachment.
    ///
    /// This is used for static-marker (`/*@static*/`) paths where we want the
    /// static semantics but should not re-emit the marker comment in runtime calls.
    pub fn clone_expr_without_trivia(&self, expr: &Expression<'a>) -> Expression<'a> {
        let mut cloned = expr.clone_in(self.allocator);
        *cloned.span_mut() = SPAN;
        cloned
    }

    pub fn set_constant_text_value(&self, symbol_id: SymbolId, value: StaticTextValue) {
        self.constant_text_values
            .borrow_mut()
            .insert(symbol_id, value);
    }

    pub fn get_constant_text_value(&self, symbol_id: SymbolId) -> Option<StaticTextValue> {
        self.constant_text_values.borrow().get(&symbol_id).cloned()
    }

    pub fn has_static_marker_comment(&self, span: Span, marker: &str) -> bool {
        let start = span.start as usize;
        let end = span.end as usize;

        if start >= end || end > self.source_text.len() {
            return false;
        }

        let snippet = &self.source_text[start..end];
        if !snippet.contains(marker) {
            return false;
        }

        // Match only a *leading* marker comment (`/*@static*/`) for the spanned expression.
        // This avoids treating nested markers (e.g. one property inside an object literal)
        // as if they marked the whole container.
        let bytes = snippet.as_bytes();
        let mut index = 0usize;

        loop {
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }

            if index >= bytes.len() {
                return false;
            }

            match bytes[index] {
                // JSX expression containers/spreads often include leading punctuation in span.
                b'{' | b'(' => {
                    index += 1;
                    continue;
                }
                _ => break,
            }
        }

        let rest = &snippet[index..];
        let Some(after_open) = rest.strip_prefix("/*") else {
            return false;
        };
        let Some(comment_end) = after_open.find("*/") else {
            return false;
        };

        after_open[..comment_end].trim() == marker
    }

    pub fn has_static_marker_comment_anywhere(&self, span: Span, marker: &str) -> bool {
        let start = span.start as usize;
        let end = span.end as usize;

        if start >= end || end > self.source_text.len() {
            return false;
        }

        let snippet = &self.source_text[start..end];
        if !snippet.contains(marker) {
            return false;
        }

        let mut rest = snippet;
        while let Some(comment_start) = rest.find("/*") {
            rest = &rest[comment_start + 2..];
            let Some(comment_end) = rest.find("*/") else {
                break;
            };

            if rest[..comment_end].trim() == marker {
                return true;
            }
            rest = &rest[comment_end + 2..];
        }

        false
    }
}
