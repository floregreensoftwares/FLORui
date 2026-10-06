# Florui

Native Rust interfaces, with the expressiveness of CSS.

Florui is an early-stage UI framework exploring a familiar way to build native applications: composable Rust components, JSX-inspired markup, and CSS stylesheets.

Its goal is to reproduce the layout and visual behavior of the Web through a native rendering engine. Chromium serves as a reference for comparison tests; it is not embedded in the native application.

## Status

Florui is in early development. The repository currently provides a minimal native preview host, CSS fixture reload, and development diagnostics. It is not yet a usable application framework.

The preview renders a single element and reads a limited `body { background-color: ... }` fixture. It does not implement general CSS parsing, cascade, layout, or the planned component API. The bootstrap renderer uses CPU pixels; the production GPU pipeline is still planned.

## Intended authoring experience

Components own their styles and compose through ordinary Rust imports. The proposed declarative macro is `view!`.

**Design example — not implemented yet:**

```rust
use florui::prelude::*;

stylesheet!("./button.css");

#[component]
pub fn Button(label: String) -> Element {
    view! {
        <button class="button">{label}</button>
    }
}
```

```css
.button {
    padding: 10px 16px;
    border: none;
    border-radius: 8px;
    background: #42734f;
    color: white;
    font: inherit;
}

.button:hover {
    background: #345c3e;
}
```

The planned build integration collects component stylesheets automatically, so application entry points do not need to register every component's CSS.

## Run the development preview

Install Rust through rustup and the native build tools for your platform. The repository pins Rust 1.94.0 in `rust-toolchain.toml`. Initial development and CI focus on Windows; other platforms are not yet validated.

From the repository root:

```sh
cargo run -p florui-cli -- dev
```

Edit the color in [`fixtures/dev/app.css`](fixtures/dev/app.css) and save to update the preview. Reload failures are reported while the preview retains its last valid revision.

To use a different fixture:

```sh
cargo run -p florui-cli -- dev --fixture path/to/app.css
```

The fixture must follow the same limited format. A desktop session is required to open the preview window.

## Create a project

```sh
cargo run -p florui-cli -- new my-app
```

writes `my-app/` with a component, its stylesheet, a window entry, a `florui dev` entry that reloads the CSS, a test that clicks the button, and a `florui.config.toml`. Existing files are never overwritten: any collision is listed and nothing is written.

The generated `Cargo.toml` depends on the Florui crates by version, as if they were published. They are not yet, so the project does not resolve from crates.io today. The generated project was built and tested against this checkout by patching the crates to local paths; that check is the ignored test `the_generated_project_builds_and_passes_its_own_test` in `florui-cli`.

Run `florui --help` for the other commands.

## Run the tests

```sh
florui test [--package NAME] [--suite cargo|visual|all] [-- <cargo test args>]
```

runs `cargo test -p <package>` for the selected package (never the whole workspace; arguments after `--` go to `cargo test`) and, under `all`, the reference fixtures in `fixtures/reference` at the workspace root against Chromium. It ends with one line per suite: `passed`, `failed` or `skipped`. Under `all` the fixtures are skipped, and reported as skipped, when the workspace has none or no Chromium is found (`scripts/fetch-chromium.ps1` or `FLORUI_CHROMIUM`); `--suite visual` fails in that case instead, so a suite asked for by name never counts as passed without running. The exit code is 1 when any suite failed.

## Format on save

`florui fmt --stdin-filepath <path>` reads a Rust source file from stdin, prints only the formatted source to stdout and sends every diagnostic to stderr (exit code 2 on failure, so an editor keeps the buffer as it was). The path selects the `rustfmt.toml` and the edition of the package that owns the file.

In VS Code, with the [Custom Local Formatters](https://marketplace.visualstudio.com/items?itemName=jkillian.custom-local-formatters) extension:

```jsonc
{
  "customLocalFormatters.formatters": [
    { "command": "florui fmt --stdin-filepath ${file}", "languages": ["rust"] }
  ],
  "[rust]": {
    "editor.defaultFormatter": "jkillian.custom-local-formatters",
    "editor.formatOnSave": true
  }
}
```

Checked on Windows with VS Code and version 0.2.0 of that extension: saving a file with an unformatted `view!` reformats it using the project's `rustfmt.toml`, and a file that does not parse is saved unchanged with the formatter's error shown. The command is run through the shell, so `florui` must be on `PATH`. Other editors and `rust-analyzer`'s `overrideCommand` (which does not pass the file path) were not tried.

## Direction

- **CSS fidelity:** implement style, layout, text, and painting with explicit compatibility coverage and reproducible visual comparisons.
- **Composition:** user-defined components, typed props, hooks, context, and owned async work.
- **Development tools first:** CSS hot reload, an inspector, source-linked diagnostics, and profiling to support development of the engine itself.
- **Complete interaction:** text editing, keyboard navigation, accessibility, and overlays alongside visual rendering.
- **Shared authoring for Web:** a planned DOM backend using Rust/WebAssembly, separate from the native rendering pipeline.

Pixel-perfect output is a testing goal for controlled fixtures, not a current claim of complete CSS support or identical rendering across all platforms. Performance claims will require published measurements.

## Development checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace --doc
```

Keep changes focused and distinguish implemented behavior from proposals. Bug reports are most useful with reproduction steps, environment details, and a minimal fixture. Visual reports should include the expected and actual output.

## License

Unless otherwise noted, Florui's original code and documentation are licensed
under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

Third-party code and assets retain their own licenses. Bundled fonts are
covered by the license files shipped alongside them; dependencies such as
Stylo retain their upstream licensing terms.