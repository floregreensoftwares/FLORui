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

## Build a release

```sh
florui build --target native [--package NAME] [--strict]
```

builds only the selected package (not the whole workspace) in release mode and stages its executables in a fresh `target/florui-build/<package>/native/`, with a `report.json` beside them: the package and application identity, the toolchain, the git commit and whether the tree was dirty, the `Cargo.lock` hash, and each file's size, SHA-256 and BLAKE3.

The report also says whether the artifact includes the developer tooling: the inspector crate, or the `profiling` and `source-locations` features of the Florui crates. That is read from what `cargo` compiled for this build, so it is evidence about the dependency graph and not a scan of the executable, and an executable stays inspectable whatever was left out. Included tooling is a warning; `--strict` rejects it with exit code 2 and still writes the report. The build is not claimed to be reproducible.

On Windows the staged executables also carry the application's identity, written into the copy (the file cargo produced is left alone, and its hashes are recorded as `built`). The version information comes from the configuration: `app.name`, `app.description`, `bundle.publisher` and `app.version`, whole, prerelease and build metadata included. The icon is `app.icons.windows`, else `app.icons.source`, an SVG or a PNG, written at 16, 24, 32, 48, 64, 128 and 256 pixels with transparent edges kept. A PNG is never enlarged: a 64 pixel PNG gives the sizes up to 64 and a warning, and one under 16 pixels is refused. A source that is not square is centered on a transparent square, not cropped. An icon that is declared but cannot be converted fails the build before anything is staged. The same icon is staged as `icon.ico`, and `report.json` lists its source, the source's SHA-256 and the sizes generated or left out. Without a declared icon the executable keeps the default one. The native icon is never used for the Web build. On other hosts the report says no resources were written. Installers, signing, the Windows application manifest and the macOS and Linux icon formats are not part of it yet.

### Check a build

```sh
florui doctor --target native --artifacts target/florui-build/<package>/native [--strict] [--json]
```

reads an existing staged output without building, changing or running anything, and checks it against its `report.json` and against the project as it is now: `artifacts.files` (every listed file has the recorded size and hashes, nothing else is in the directory, and no name leaves it), `artifacts.exposure` (included developer tooling is a warning that `--strict` turns into a failure), `artifacts.resources` (the version information and icon are read from the executable itself, not from the report), `artifacts.identity` (the name, identifier and version the build was made with against the configuration now) and `artifacts.provenance` (the commit, a clean tree and the lockfile). A missing or unreadable report fails; evidence a check cannot get, such as a build made outside a git repository, is reported as unknown, never as a pass. Without `--artifacts` none of these checks is reported. Web output is not checked yet.

Run `florui --help` for the other commands.

## Run the tests

```sh
florui test [--package NAME] [--suite cargo|visual|all] [-- <cargo test args>]
```

runs `cargo test -p <package>` for the selected package (never the whole workspace; arguments after `--` go to `cargo test`) and, under `all`, the reference fixtures in `fixtures/reference` at the workspace root against Chromium. It ends with one line per suite: `passed`, `failed` or `skipped`. Under `all` the fixtures are skipped, and reported as skipped, when the workspace has none or no Chromium is found (`scripts/fetch-chromium.ps1` or `FLORUI_CHROMIUM`); `--suite visual` fails in that case instead, so a suite asked for by name never counts as passed without running. The exit code is 1 when any suite failed.

## Compare against Chromium

```sh
florui compare [--fixture NAME_OR_PATH]... [--chromium PATH] [--out-dir DIR] [--open]
```

renders the reference fixtures in Chromium and in Florui and writes, for each, the two images, their difference, overlays and a `report.json` under `target/florui-conformance/<fixture>/` of the project. A fixture is named by its directory under `fixtures/reference` at the workspace root or given as a path; with none named, all are compared in one browser launch. The Chromium is `--chromium`, `FLORUI_CHROMIUM`, or the pinned build from `scripts/fetch-chromium.ps1`. Every result prints where its artifacts are; `--open` opens a failing fixture's folder (or the only fixture's) in the file manager on Windows. The command only reads the fixtures: it never updates an expected result. The exit code is 1 when any fixture differs. `compare-all` records a run history instead.

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