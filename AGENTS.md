# AGENTS

Port **@dom-expressions/packages/babel-plugin-jsx** fully to OXC. The goal is output parity with the Babel plugin for DOM, SSR, and eventual hydratable/universal/dynamic modes.

## Source of Truth (parity targets)

- Upstream implementation: `dom-expressions/packages/babel-plugin-jsx/src`
- Upstream fixtures: `dom-expressions/packages/babel-plugin-jsx/test`
- Local parity runner: `tests/babel_fixture_runner.rs`
- Local behavior tests: `tests/transform_tests.rs`

## Code Map

- `src/lib.rs` – public API (`transform`, NAPI bindings), routes to DOM/SSR transforms.
- `crates/common` – shared helpers, constants, and `TransformOptions`.
- `crates/dom` – DOM transform pipeline (element/component handling, template output, helper imports).
- `crates/ssr` – SSR transform pipeline (template string + escape output).
- `crates/linter` – lints (not central to transform parity).
- `submodules/dom-expressions/` – vendored upstream repo for reference and fixtures.

- Maintain DOM/SSR helper import logic parity with Babel (see `crates/dom/src/transform.rs` and `crates/ssr/src/transform.rs`).
- Register helper imports and delegated events through `TransformOptions` (`register_helper`, `register_delegate`).
- Keep template collection (`templates`) and output paths consistent with Babel fixtures.
- When adding new output features (hydration, wrapperless, universal, dynamic), mirror Babel option semantics and update fixture coverage.

## Conventions

- Use `oxc_allocator` for memory management
- Follow rustfmt config in `.cargo/rustfmt.toml`
- Performance-critical: avoid unnecessary allocations

## Tests & Commands

Rust (primary):

- `cargo test` – fast unit tests.
- `cargo test --test transform_tests` – direct DOM/SSR behavior checks.
- `cargo test --test babel_fixture_runner -- --ignored` – Babel fixture parity suites.
- `RUN_UNSUPPORTED_FIXTURES=1 cargo test --test babel_fixture_runner -- --ignored` – include currently unsupported fixture suites.

Node/NAPI:

- `pnpm build` – build native addon.

Formatting:

- `cargo fmt`
- `pnpm format` (prettier + taplo)

## When Changing Behavior

- Add/adjust fixtures in the upstream `dom-expressions` subtree **or** add targeted tests in `tests/transform_tests.rs`.
- Confirm parity with the Babel plugin output before landing changes.
