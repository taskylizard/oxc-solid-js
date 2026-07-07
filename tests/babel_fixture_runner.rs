//! Fixture runner for babel-plugin-jsx-dom-expressions tests.
//!
//! Run all suites (ignored by default):
//!   cargo test --test babel_fixture_runner -- --ignored
//!
//! Run a single suite:
//!   cargo test --test babel_fixture_runner dom_fixtures -- --ignored
//!
//! To include suites that rely on unimplemented options, set:
//!   RUN_UNSUPPORTED_FIXTURES=1
//!

use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
};

use rustc_hash::FxHashSet;

use common::{GenerateMode, RendererConfig};
use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use oxc_parser::Parser;
use oxc_solid_js_compiler::{transform, TransformOptions};
use oxc_span::SourceType;

const BUILT_INS: [&str; 2] = ["For", "Show"];
const DYNAMIC_ELEMENTS: [&str; 33] = [
    "table",
    "tbody",
    "div",
    "h1",
    "span",
    "header",
    "footer",
    "slot",
    "my-el",
    "my-element",
    "module",
    "input",
    "img",
    "iframe",
    "button",
    "a",
    "svg",
    "rect",
    "x",
    "y",
    "linearGradient",
    "stop",
    "style",
    "li",
    "ul",
    "label",
    "text",
    "namespace:tag",
    "path",
    "noscript",
    "select",
    "option",
    "video",
];
const DYNAMIC_DOM_ELEMENTS: &[&str] = &[
    "table",
    "tbody",
    "div",
    "h1",
    "span",
    "header",
    "footer",
    "slot",
    "my-el",
    "my-element",
    "module",
    "input",
    "img",
    "iframe",
    "button",
    "a",
    "svg",
    "rect",
    "x",
    "y",
    "linearGradient",
    "stop",
    "style",
    "li",
    "ul",
    "label",
    "text",
    "namespace:tag",
    "p",
    "noscript",
    "select",
    "option",
    "video",
    "math",
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
const DYNAMIC_HYDRATABLE_ELEMENTS: &[&str] = &[
    "table",
    "tbody",
    "div",
    "h1",
    "span",
    "header",
    "footer",
    "slot",
    "my-el",
    "my-element",
    "module",
    "input",
    "img",
    "iframe",
    "button",
    "a",
    "svg",
    "rect",
    "x",
    "y",
    "linearGradient",
    "stop",
    "style",
    "li",
    "ul",
    "label",
    "text",
    "namespace:tag",
    "html",
    "head",
    "body",
    "title",
    "meta",
    "link",
    "footer",
    "script",
    "noscript",
    "select",
    "video",
    "option",
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
const DYNAMIC_WRAPPERLESS_ELEMENTS: &[&str] = &[
    "table",
    "tbody",
    "div",
    "h1",
    "span",
    "header",
    "footer",
    "slot",
    "my-el",
    "my-element",
    "module",
    "input",
    "img",
    "iframe",
    "button",
    "a",
    "svg",
    "rect",
    "x",
    "y",
    "linearGradient",
    "stop",
    "style",
    "li",
    "ul",
    "label",
    "text",
    "namespace:tag",
    "html",
    "head",
    "body",
    "title",
    "meta",
    "link",
    "footer",
    "script",
    "noscript",
    "select",
    "option",
    "video",
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

#[derive(Clone, Copy)]
struct SuiteRenderer {
    name: &'static str,
    module_name: &'static str,
    elements: &'static [&'static str],
}

const DYNAMIC_RENDERERS: [SuiteRenderer; 1] = [SuiteRenderer {
    name: "dom",
    module_name: "r-dom",
    elements: &DYNAMIC_ELEMENTS,
}];
const DYNAMIC_DOM_RENDERERS: [SuiteRenderer; 1] = [SuiteRenderer {
    name: "dom",
    module_name: "r-dom",
    elements: DYNAMIC_DOM_ELEMENTS,
}];
const DYNAMIC_HYDRATABLE_RENDERERS: [SuiteRenderer; 1] = [SuiteRenderer {
    name: "dom",
    module_name: "r-dom",
    elements: DYNAMIC_HYDRATABLE_ELEMENTS,
}];
const DYNAMIC_WRAPPERLESS_RENDERERS: [SuiteRenderer; 1] = [SuiteRenderer {
    name: "dom",
    module_name: "r-dom",
    elements: DYNAMIC_WRAPPERLESS_ELEMENTS,
}];

#[derive(Clone, Copy)]
struct SuiteOptions {
    module_name: &'static str,
    generate: GenerateMode,
    renderers: &'static [SuiteRenderer],
    hydratable: bool,
    delegate_events: bool,
    wrap_conditionals: bool,
    omit_nested_closing_tags: bool,
    omit_last_closing_tag: bool,
    omit_quotes: bool,
    inline_styles: bool,
    context_to_custom_elements: bool,
    built_ins: &'static [&'static str],
    effect_wrapper: &'static str,
    memo_wrapper: &'static str,
    static_marker: &'static str,
    require_import_source: Option<&'static str>,
    validate: bool,
}

#[derive(Clone, Copy)]
struct FixtureSuite {
    name: &'static str,
    fixture_dir: &'static str,
    options: SuiteOptions,
    supported: bool,
    unsupported_reason: Option<&'static str>,
}

struct FixtureCase {
    name: String,
    code_path: PathBuf,
    output_path: PathBuf,
}

impl FixtureSuite {
    fn supported(name: &'static str, fixture_dir: &'static str, options: SuiteOptions) -> Self {
        Self {
            name,
            fixture_dir,
            options,
            supported: true,
            unsupported_reason: None,
        }
    }
}

macro_rules! fixture_test {
    ($fn_name:ident, $suite_name:literal) => {
        #[test]
        #[ignore = "Babel fixture parity tests (run with --ignored)"]
        fn $fn_name() {
            run_named_suite($suite_name);
        }
    };
}

fixture_test!(dom_fixtures, "dom");
fixture_test!(dom_hydratable_fixtures, "dom-hydratable");
fixture_test!(dom_wrapperless_fixtures, "dom-wrapperless");
fixture_test!(dom_compatible_fixtures, "dom-compatible");
fixture_test!(dom_no_inline_styles_fixtures, "dom-no-inline-styles");
fixture_test!(
    dom_require_import_source_fixtures,
    "dom-require-import-source"
);
fixture_test!(dynamic_fixtures, "dynamic");
fixture_test!(dynamic_dom_fixtures, "dynamic-dom");
fixture_test!(dynamic_hydratable_fixtures, "dynamic-hydratable");
fixture_test!(dynamic_wrapperless_fixtures, "dynamic-wrapperless");
fixture_test!(dynamic_universal_fixtures, "dynamic-universal");
fixture_test!(ssr_fixtures, "ssr");
fixture_test!(ssr_hydratable_fixtures, "ssr-hydratable");
fixture_test!(universal_fixtures, "universal");

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("submodules")
        .join("dom-expressions")
        .join("packages")
        .join("babel-plugin-jsx")
        .join("test")
}

fn fixture_suites() -> Vec<FixtureSuite> {
    let dom = base_options("r-dom", GenerateMode::Dom);
    let ssr = base_options("r-server", GenerateMode::Ssr);
    let universal = base_options("r-custom", GenerateMode::Universal);
    let dynamic_dom = base_options("r-dom", GenerateMode::Dynamic);
    let dynamic_custom = base_options("r-custom", GenerateMode::Dynamic);

    vec![
        FixtureSuite::supported("dom", "__dom_fixtures__", dom),
        FixtureSuite::supported(
            "dom-hydratable",
            "__dom_hydratable_fixtures__",
            SuiteOptions {
                hydratable: true,
                ..dom
            },
        ),
        FixtureSuite::supported(
            "dom-wrapperless",
            "__dom_wrapperless_fixtures__",
            SuiteOptions {
                wrap_conditionals: false,
                delegate_events: false,
                effect_wrapper: "",
                memo_wrapper: "",
                ..dom
            },
        ),
        FixtureSuite::supported(
            "dom-compatible",
            "__dom_compatible_fixtures__",
            SuiteOptions {
                omit_last_closing_tag: false,
                omit_quotes: false,
                ..dom
            },
        ),
        FixtureSuite::supported(
            "dom-no-inline-styles",
            "__dom_no_inline_styles_fixtures__",
            SuiteOptions {
                inline_styles: false,
                ..dom
            },
        ),
        FixtureSuite::supported(
            "dom-require-import-source",
            "__dom_require_import_source_fixtures__",
            SuiteOptions {
                require_import_source: Some("r-dom"),
                ..dom
            },
        ),
        FixtureSuite::supported(
            "dynamic",
            "__dynamic_fixtures__",
            SuiteOptions {
                renderers: &DYNAMIC_RENDERERS,
                ..dynamic_custom
            },
        ),
        FixtureSuite::supported(
            "dynamic-dom",
            "__dom_fixtures__",
            SuiteOptions {
                renderers: &DYNAMIC_DOM_RENDERERS,
                ..dynamic_dom
            },
        ),
        FixtureSuite::supported(
            "dynamic-hydratable",
            "__dom_hydratable_fixtures__",
            SuiteOptions {
                hydratable: true,
                renderers: &DYNAMIC_HYDRATABLE_RENDERERS,
                ..dynamic_dom
            },
        ),
        FixtureSuite::supported(
            "dynamic-wrapperless",
            "__dom_wrapperless_fixtures__",
            SuiteOptions {
                wrap_conditionals: false,
                delegate_events: false,
                effect_wrapper: "",
                memo_wrapper: "",
                renderers: &DYNAMIC_WRAPPERLESS_RENDERERS,
                ..dynamic_dom
            },
        ),
        FixtureSuite::supported(
            "dynamic-universal",
            "__universal_fixtures__",
            dynamic_custom,
        ),
        FixtureSuite::supported("ssr", "__ssr_fixtures__", ssr),
        FixtureSuite::supported(
            "ssr-hydratable",
            "__ssr_hydratable_fixtures__",
            SuiteOptions {
                hydratable: true,
                ..ssr
            },
        ),
        FixtureSuite::supported("universal", "__universal_fixtures__", universal),
    ]
}

fn run_named_suite(name: &str) {
    let run_unsupported = std::env::var("RUN_UNSUPPORTED_FIXTURES").is_ok();
    let fixture_root = fixture_root();

    let suite = fixture_suites()
        .into_iter()
        .find(|suite| suite.name == name)
        .unwrap_or_else(|| panic!("Unknown fixture suite: {name}"));

    if !suite.supported && !run_unsupported {
        if let Some(reason) = suite.unsupported_reason {
            eprintln!("Skipped fixture suite '{}': {reason}", suite.name);
        } else {
            eprintln!("Skipped fixture suite '{}'", suite.name);
        }
        return;
    }

    if let Err(err) = run_suite(&fixture_root, suite) {
        panic!("{err}");
    }
}

fn base_options(module_name: &'static str, generate: GenerateMode) -> SuiteOptions {
    SuiteOptions {
        module_name,
        generate,
        renderers: &[],
        hydratable: false,
        delegate_events: true,
        wrap_conditionals: true,
        omit_nested_closing_tags: false,
        omit_last_closing_tag: true,
        omit_quotes: true,
        inline_styles: true,
        context_to_custom_elements: true,
        built_ins: &BUILT_INS,
        effect_wrapper: "effect",
        memo_wrapper: "memo",
        static_marker: "@once",
        require_import_source: None,
        validate: true,
    }
}

fn run_suite(root: &Path, suite: FixtureSuite) -> Result<(), String> {
    let suite_dir = root.join(suite.fixture_dir);

    if !suite_dir.exists() {
        return Err(format!(
            "Suite '{}' missing fixture dir: {}",
            suite.name,
            suite_dir.display()
        ));
    }

    let cases = collect_cases(root, &suite, &suite_dir)?;
    if cases.is_empty() {
        return Err(format!(
            "Suite '{}' had no fixtures in {}",
            suite.name,
            suite_dir.display()
        ));
    }

    let mut failures = Vec::new();
    for case in cases {
        if let Err(err) = run_case(&suite, &case) {
            failures.push(err);
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Suite '{}' failures:\n{}",
            suite.name,
            failures.join("\n\n")
        ))
    }
}

fn collect_cases(
    root: &Path,
    suite: &FixtureSuite,
    suite_dir: &Path,
) -> Result<Vec<FixtureCase>, String> {
    let mut cases = Vec::new();
    // Optional local-debug filter: restrict fixture collection to one case name.
    let case_filter = std::env::var("FIXTURE_CASE").ok();
    let entries = fs::read_dir(suite_dir)
        .map_err(|err| format!("Failed to read fixtures in {}: {err}", suite_dir.display()))?;

    for entry in entries {
        let entry = entry.map_err(|err| format!("Failed to read entry: {err}"))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let case_name = entry.file_name().to_string_lossy().into_owned();
        if case_filter
            .as_ref()
            .is_some_and(|filter| filter != &case_name)
        {
            continue;
        }

        let code_path = path.join("code.js");
        let mut output_path = path.join("output.js");

        // Upstream currently omits __ssr_hydratable_fixtures__/attributeExpressions/output.js.
        // Fall back to the SSR fixture output for the same case name.
        if suite.name == "ssr-hydratable" && !output_path.exists() {
            let fallback = root
                .join("__ssr_fixtures__")
                .join(&case_name)
                .join("output.js");
            if fallback.exists() {
                output_path = fallback;
            }
        }

        if !code_path.exists() || !output_path.exists() {
            continue;
        }

        cases.push(FixtureCase {
            name: case_name,
            code_path,
            output_path,
        });
    }

    cases.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(cases)
}

fn run_case(suite: &FixtureSuite, case: &FixtureCase) -> Result<(), String> {
    let source = fs::read_to_string(&case.code_path).map_err(|err| {
        format!(
            "{}:{}: failed to read code.js: {err}",
            suite.name, case.name
        )
    })?;
    let expected_raw = fs::read_to_string(&case.output_path).map_err(|err| {
        format!(
            "{}:{}: failed to read output.js: {err}",
            suite.name, case.name
        )
    })?;

    let filename = case
        .code_path
        .to_str()
        .ok_or_else(|| format!("{}:{}: invalid code path", suite.name, case.name))?;

    let options = build_options(&suite.options, filename);
    let actual = transform(&source, Some(options));

    let expected_raw = preprocess_expected_output(suite.name, &expected_raw);
    let expected = normalize_code(&expected_raw, &case.output_path)?;
    let actual = normalize_code(&actual.code, &case.output_path)?;

    if expected != actual {
        return Err(format!(
            "{}:{} mismatch\nExpected:\n{}\n\nActual:\n{}",
            suite.name, case.name, expected, actual
        ));
    }

    Ok(())
}

fn preprocess_expected_output(suite_name: &str, expected_raw: &str) -> String {
    if suite_name == "dom-require-import-source" {
        strip_trailing_fin_statement(expected_raw)
    } else {
        expected_raw.to_string()
    }
}

fn strip_trailing_fin_statement(code: &str) -> String {
    let source = code.replace("\r\n", "\n");
    let trimmed = source.trim_end();
    let stripped = ["(\"fin\");", "('fin');", "\"fin\";", "'fin';"]
        .into_iter()
        .find_map(|suffix| trimmed.strip_suffix(suffix));

    match stripped {
        Some(prefix) => prefix.trim_end().to_string(),
        None => source,
    }
}

fn build_options<'a>(suite: &SuiteOptions, filename: &'a str) -> TransformOptions<'a> {
    TransformOptions {
        module_name: suite.module_name,
        renderers: suite
            .renderers
            .iter()
            .map(|renderer| RendererConfig {
                name: renderer.name,
                module_name: renderer.module_name,
                elements: renderer.elements.to_vec(),
            })
            .collect(),
        require_import_source: suite.require_import_source,
        generate: suite.generate,
        hydratable: suite.hydratable,
        delegate_events: suite.delegate_events,
        delegated_events: vec![],
        wrap_conditionals: suite.wrap_conditionals,
        omit_nested_closing_tags: suite.omit_nested_closing_tags,
        omit_last_closing_tag: suite.omit_last_closing_tag,
        omit_quotes: suite.omit_quotes,
        inline_styles: suite.inline_styles,
        context_to_custom_elements: suite.context_to_custom_elements,
        built_ins: suite.built_ins.to_vec(),
        effect_wrapper: suite.effect_wrapper,
        memo_wrapper: suite.memo_wrapper,
        filename,
        source_type: SourceType::from_path(filename)
            .unwrap_or(SourceType::tsx())
            .with_jsx(true),
        source_map: false,
        static_marker: suite.static_marker,
        validate: suite.validate,
        hmr: false,
        hmr_bundler: "standard",
        hmr_granular: true,
        hmr_jsx: true,
        hmr_fix_render: true,
        templates: RefCell::new(Vec::new()),
        helpers: RefCell::new(FxHashSet::default()),
        delegates: RefCell::new(FxHashSet::default()),
    }
}

fn normalize_code(code: &str, filename: &Path) -> Result<String, String> {
    let allocator = Allocator::default();
    let source = code.replace("\r\n", "\n");
    let source_type = SourceType::from_path(filename)
        .unwrap_or(SourceType::tsx())
        .with_jsx(true);

    let parsed = Parser::new(&allocator, &source, source_type).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        let errors = parsed
            .diagnostics
            .iter()
            .map(|err| err.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "Failed to parse {}:\n{}",
            filename.display(),
            errors
        ));
    }

    let output = Codegen::new()
        .with_options(CodegenOptions {
            indent_width: 2,
            indent_char: IndentChar::Space,
            ..CodegenOptions::default()
        })
        .build(&parsed.program)
        .code;

    Ok(output
        .trim()
        .replace("/* @__PURE__ */", "/*#__PURE__*/")
        .to_string())
}
