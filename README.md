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

writes `my-app/` with a component, its stylesheet, a window entry, a `florui dev` entry that reloads the CSS, a test that clicks the button, and a `florui.config.toml` that names its editor schema, with that `florui.config.schema.json` beside it (see "Editor support" below). Existing files are never overwritten: any collision is listed and nothing is written.

The generated `Cargo.toml` depends on the Florui crates by version, as if they were published. They are not yet, so the project does not resolve from crates.io today. The generated project was built and tested against this checkout by patching the crates to local paths; that check is the ignored test `the_generated_project_builds_and_passes_its_own_test` in `florui-cli`.

## Editor support for `florui.config.toml`

```sh
florui schema --write
```

writes `florui.config.schema.json`, a JSON Schema (draft-07) for the configuration, into the project next to `florui.config.toml` (`florui schema` alone prints it; `--write <PATH>` names the file). It is generated from the same typed definition the parser reads, so it lists the same keys, types and values, and every key carries the explanation shown on hover. Rules that need code, such as an SPDX license expression, a URL or a window size that must not exceed another, are still checked when the configuration is read, with their line and column; the schema describes them in each key's text.

To use it, start the configuration with a comment naming the file, which Taplo and the Even Better TOML extension for VS Code read:

```toml
#:schema ./florui.config.schema.json
schema_version = 1
```

The locales file has its own schema: `florui schema --locales` prints it and `florui schema --locales --write` writes `florui.locales.schema.json` next to the project's `florui.locales.toml`, which is associated the same way with `#:schema ./florui.locales.schema.json` on its first line. It describes the one table per locale tag (`en`, `pt-BR`) with the application's name and description; whether a key is a valid locale tag is still checked when the configuration is read. Checked against the language server of Even Better TOML 0.21.2 driven directly (an unknown key and a wrong type flagged, hover text, keys offered inside a locale table), not through the VS Code window.

No schema URL is published, so the association is a relative path and the copy belongs to the project: `florui new` writes it and the line, and `florui doctor` has a `config.editor_schema` check that warns when the copy is not the one the installed Florui generates (written by another version, or edited); run `florui schema --write` again after upgrading Florui to refresh it. It was checked in VS Code with Even Better TOML 0.21.2 (Taplo's engine): an unknown key, a wrong type and a value outside an enum are underlined with the schema's messages, hovering a key shows its explanation, completing a key inside a table, including nested ones such as `[environments.development.app.icons]` and tables named by a map key, offers the keys that table defines with their text (inside an array of tables such as `[[app.activation.file_associations]]` the extension offers no keys, which it also does not for a trivial schema), and the values of an enum are offered with theirs; the example application's configuration and the `florui new` template raise no diagnostics. The extension appears to keep a schema it has already read, so after refreshing the file an editor window may need to be reloaded to see the new one.

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

### Distribution metadata

`[bundle]` in `florui.config.toml` takes `publisher`, `copyright`, `license` (an SPDX expression), `license_file` (relative to the file, inside the package), `category` (a lowercase token) and `homepage` (an http or https URL). `license`, `license_file` and `homepage` fall back to the package's Cargo `license`, `license-file` and `homepage` when not set; a configured value always wins and nothing else is inherited. An invalid value is an error with its line and column.

A build writes the copyright into the executable's version information and stages the license file beside the executables under its own name, listed with its hashes. A Windows executable has no place for the license, category or homepage, so `report.json` records each of them (with where it came from) as not represented instead of dropping it. `florui doctor --target native --distribution` checks all of it offline: the identifier is a reverse-domain name, the version maps to a Windows version, the license is declared and valid, the license file is readable, the icon converts, and what is missing is a warning. It also says that no installer or signing format is supported yet.

### Localized names

`[app.locales.<tag>]` (or `florui.locales.toml`) gives each language its own `name` and `description`, with
`app.default_locale` as the fallback. A Windows build writes one string table per locale into the
executable's version information, the default locale's first, so the name and description shown for a
language are its own; identifiers, versions and the publisher never change with language. A tag is mapped to
a Windows language for a fixed list of common ones (`en`, `pt-BR`, `es`, `fr`, `de`, `ja`, `zh-CN`, ...); a tag
without a mapping, or one that maps to a language another locale already uses, is not written, and
`florui build` and `florui doctor --distribution` say so instead of dropping it silently. `florui doctor
--artifacts` reads every table back from the executable. This is identity localization, not an application
translation framework, and other targets are not covered yet.

### URL schemes and file types

`[app.activation]` declares the URL schemes (`url_schemes`) and file types (`file_associations`) the application
opens; the running application receives them as typed activation events. `florui register` is the one explicit
step that makes Windows know them, for the current user and from the staged build (`florui build` first):

```sh
florui register [--dry-run] [--artifacts DIR] [--exe NAME] [--json]
florui unregister [--json]
florui doctor --registration
```

Nothing else changes an association: `dev`, `build` and `doctor` never do. Registering writes per-user keys
(no administrator): a command for each scheme and file type that launches the staged executable with the URL or path as
its one argument, the application's capabilities so Windows lists it in "Open with" and Settings > Apps > Default apps, and a
record of every key and value it wrote, so `unregister` removes exactly that and leaves anything that is no longer the
application's alone. It **never sets a default**: the application becomes a candidate and the user chooses.

It refuses, before writing anything, a reserved scheme (`http`, `https`, `mailto`, `file`, `ms-*`, ...), a scheme or
file association that another application (or another Florui identifier) already owns, a scheme registered for every user
by something else, and an executable that is not the file the build wrote (its SHA-256 is checked against `report.json`).
`doctor --registration` reads the same state without changing it: whether the application is registered and still as it
was registered, whether registering would take something over, and whether two environments of the project would register
the same scheme or file type. Environments that are installed side by side give themselves distinct ones in
`[environments.<name>.app.activation]`.

An application that wants to be the default for some of them lists them in `request_default` (a scheme, or an extension
with a dot) as a statement of intent; each must be declared above. At run time it asks with
`florui_platform::request_default_handler(&permission, &target)`, which refuses what was not declared, refuses when the
application is not registered, and otherwise opens the operating system's own Default apps page for this application, where
the user decides. It never sets a default itself and its answer (`Opened`) says the page opened, not that the user chose this
application. The `single_instance` example has a button that does this. Verified on Windows only, per user; machine-wide
registration, installers and other operating systems are not covered.

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

## Glass

Glass is built from supported CSS: a translucent `background-color`, a `border`, a `box-shadow` and
`backdrop-filter` (`blur`, `brightness`, `contrast`, `saturate`), over what is painted behind. The panel's
edge is handled the way Chromium handles it (the backdrop is mirrored there, so a blur does not fade
toward the panel's border), and filter lengths are CSS pixels at any display scale. These are compared
against Chromium in the reference fixtures.

An advanced material is opt-in and does not change the meaning of any CSS property. On a node that
also has a `backdrop-filter`:

```css
.panel {
  backdrop-filter: blur(8px) saturate(1.5);
  --florui-glass: refract;
  --florui-glass-refraction: 12px;   /* peak displacement at the edge */
  --florui-glass-edge: 24px;         /* width of the lensing band, inward from the edge */
  --florui-glass-light-angle: 315deg;
  --florui-glass-light-strength: 0.5;
  --florui-glass-quality: full;      /* full, reduced or off */
}
```

What is behind the node is refracted along its rounded edge, then run through the `backdrop-filter`,
then the node's own background and border paint over it, then the rim light is added. The displacement
is never more than the requested refraction, the width of the band or 32 CSS pixels; a request above
that is cut and counted (`glass-clamped` in the profile), and `florui_paint::glass_effective` gives the
values that will be used so an interface can state them. `--florui-glass-quality: off` is exactly the
basic glass, and a value that cannot be honored gives no material with a stated reason. Like any custom
property these inherit, so a child that must not refract says `--florui-glass: none`. The full contract is
in the docs of `florui-paint`'s `material` module.

`cargo run --example glass_showcase -p florui-example-app` shows opaque, glass and advanced modes over a
moving backdrop, with the blur, refraction and quality adjustable and the effective settings on screen;
`-- --freeze 1500` stops animation time for reproducible captures. The advanced material has reviewed
reference scenes of its own (they are Florui's renders, not Chromium's, which has no such material) and is
compared against a fresh render while the backdrop moves and the panel changes.

Declared coverage and limits: verified on Windows only. The renderer is CPU raster, so the material needs
no GPU and its cost is CPU time (see "The glass material" in `florui-bench`'s README for measurements on
one machine). A window made translucent shows the desktop through it, but the desktop behind it is not
blurred; operating-system materials are not part of this. No claim is made of matching any other
system's glass.

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