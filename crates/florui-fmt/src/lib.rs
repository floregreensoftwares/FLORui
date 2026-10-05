//! `florui fmt`'s real engine: reformats `view! { ... }` bodies in a
//! `.rs` file using `view!`'s own real grammar (via
//! [`florui_view_syntax`] — one grammar, not a second one this crate
//! invents), then delegates everything else in the file to real
//! `rustfmt`.
//!
//! # What this crate actually synthesizes, and what it never touches
//!
//! Only the *structural* shape of a `view!` body is this crate's own
//! decision: tag/attribute/child layout, indentation, and line breaks,
//! per the small stable style below. Every string literal attribute
//! value and every `{expr}` child is copied **byte-for-byte** from the
//! original source (found via `proc_macro2::Span::byte_range`, which is
//! real and accurate here specifically because this crate parses a file
//! from plain text, never from inside an actual macro expansion — see
//! `florui_view_syntax`'s own doc) — never re-serialized through
//! `quote!`/`ToTokens`, which would not necessarily reproduce the
//! author's own original spelling, spacing, or comments. Bare text
//! content is the one exception: it is re-emitted through
//! [`florui_view_syntax::text`]'s own reconstruction, which is not a new
//! formatting decision this crate invents — it is the exact
//! normalization `view!` already performs at real macro-expansion time,
//! so a text run round-trips through formatting exactly as it already
//! does through compilation.
//!
//! Style: short elements may stay on one line; an element with real
//! children always breaks across lines, one nesting level per level of
//! depth; an opening tag whose attributes would overflow 100 columns
//! breaks to one attribute per line. This mirrors `rustfmt`'s own
//! default `max_width`/4-space indent, matching "indentation follows the
//! effective Rust configuration."
//!
//! # Comment safety: skip, never guess
//!
//! Comments do not exist in a `proc_macro2::TokenStream` at all — Rust's
//! lexer discards them before any macro ever sees a token — so a `view!`
//! invocation's parsed [`florui_view_syntax::Nodes`] carries no
//! information about where comments were. Rather than risk silently
//! dropping one, this crate scans a candidate `view!` invocation's own
//! *raw source text* for anything that looks like `//` or `/*` before
//! attempting to reformat it at all; a hit leaves that invocation
//! completely untouched, reported as skipped rather than reformatted.
//! This is deliberately conservative: a string literal containing `//`
//! (a URL in a class name, say) also triggers a skip, even though no
//! real comment is present — a missed reformatting opportunity, not a
//! correctness risk, which is the tradeoff this crate takes on the side
//! of never losing a comment.
//!
//! # What's genuinely not implemented yet
//!
//! Explicit format-skip regions, CRLF-specific handling, and the full
//! acceptance-fixture matrix (raw strings, lifetimes, generics inside
//! `{expr}` bodies interacting with the width-fitting heuristic, and so
//! on) are open — this is a real, working first version, not a claim of
//! complete grammar coverage.

use std::ops::Range;
use std::path::Path;
use std::process::{Command, Stdio};

use florui_view_syntax::{AttrValue, Node, Nodes};
use proc_macro2::TokenStream;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

const MAX_WIDTH: usize = 100;
const INDENT: usize = 4;

/// Whether `candidate` can stand as a single formatted line at `indent`
/// — width *and* the absence of any embedded newline. A candidate can
/// contain a real `\n` even when short: an `{expr}` attribute or child
/// is copied byte-for-byte from source (see this module's own doc), so
/// a multi-line closure or expression the author already wrote across
/// several lines carries its own newlines straight through. Checking
/// length alone would silently accept that as "one line" and splice a
/// broken result — never just measure width without checking this too.
fn fits_inline(indent: usize, candidate: &str) -> bool {
    !candidate.contains('\n') && indent + candidate.len() <= MAX_WIDTH
}

/// One `view!` invocation this crate declined to reformat, and why —
/// never silent: [`format_source`] always reports every skip it made.
#[derive(Debug, Clone)]
pub struct Skipped {
    /// 1-indexed line the invocation starts on, for a human-readable
    /// diagnostic.
    pub line: usize,
    pub reason: String,
}

#[derive(Debug)]
pub struct FormatOutcome {
    pub output: String,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug)]
pub enum FormatError {
    /// The file itself is not valid Rust — nothing here can be
    /// reformatted safely, so the whole file is left untouched.
    Parse(syn::Error),
    /// `rustfmt` could not be run at all (missing, or a real failure
    /// exit) — distinct from a `view!`-specific skip, since it means
    /// this crate could not finish the pipeline for the file at all.
    Rustfmt(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Parse(error) => write!(f, "could not parse as Rust: {error}"),
            FormatError::Rustfmt(message) => write!(f, "rustfmt failed: {message}"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Reformats every `view! { ... }` invocation `source` contains it has
/// enough confidence to touch (see this crate's own doc on comment
/// safety), then runs real `rustfmt` over the whole result — a single
/// deterministic pipeline, not two printers that could each undo the
/// other's work (confirmed empirically: `rustfmt` leaves an unrecognized
/// macro's own token stream completely untouched, so ordering this
/// crate's own pass before `rustfmt` is safe).
///
/// `config_dir` is where `rustfmt` looks for `rustfmt.toml` (normally the
/// formatted file's own directory); `None` uses the process's.
pub fn format_source(
    original_source: &str,
    edition: &str,
    config_dir: Option<&Path>,
) -> Result<FormatOutcome, FormatError> {
    // Every byte offset this function computes (spans, splice ranges,
    // line/indent lookups) must agree with whatever string they're
    // taken against. Rather than track two parallel line-ending
    // conventions through the whole pipeline, normalize to `\n` once up
    // front and restore the original convention on the way out --
    // `rustfmt` itself always emits `\n` over stdin/stdout regardless of
    // the input's own style, so this also keeps this crate's own output
    // consistent with real `rustfmt`'s rather than fighting it.
    let uses_crlf = original_source.contains("\r\n");
    let source = &original_source.replace("\r\n", "\n");

    let file: syn::File = syn::parse_str(source).map_err(FormatError::Parse)?;

    let mut finder = ViewMacroFinder { found: Vec::new() };
    finder.visit_file(&file);

    let mut skipped = Vec::new();
    // Applied highest-offset-first so an earlier splice never invalidates
    // a later one's own byte range.
    let mut replacements: Vec<(Range<usize>, String)> = Vec::new();

    for invocation in &finder.found {
        let raw = &source[invocation.range.clone()];
        if let Some(reason) = looks_uncertain(raw) {
            skipped.push(Skipped {
                line: line_of(source, invocation.range.start),
                reason,
            });
            continue;
        }
        match syn::parse2::<Nodes>(invocation.tokens.clone()) {
            Ok(nodes) => {
                let indent = indent_of_line(source, invocation.macro_start);
                let new_body = print_body(&nodes.0, indent, source);
                if new_body != *raw {
                    replacements.push((invocation.range.clone(), new_body));
                }
            }
            Err(error) => skipped.push(Skipped {
                line: line_of(source, invocation.range.start),
                reason: format!("could not parse this view! body: {error}"),
            }),
        }
    }

    replacements.sort_by(|a, b| b.0.start.cmp(&a.0.start));
    let mut rewritten = source.to_string();
    for (range, replacement) in replacements {
        rewritten.replace_range(range, &replacement);
    }

    let mut output = run_rustfmt(&rewritten, edition, config_dir).map_err(FormatError::Rustfmt)?;
    if uses_crlf {
        output = output.replace('\n', "\r\n");
    }
    Ok(FormatOutcome { output, skipped })
}

struct ViewInvocation {
    /// The inner token range — what actually gets replaced.
    range: Range<usize>,
    /// Where `view!` itself starts — the base indent a reformatted body
    /// (and its own closing brace) must line up with, which is *not*
    /// necessarily the same line as `range.start` when the original body
    /// was itself indented inconsistently (exactly the case a
    /// reformatter exists to fix).
    macro_start: usize,
    tokens: TokenStream,
}

struct ViewMacroFinder {
    found: Vec<ViewInvocation>,
}

impl Visit<'_> for ViewMacroFinder {
    fn visit_macro(&mut self, mac: &syn::Macro) {
        if is_view_macro(mac) {
            // Strictly between the delimiters, not just from the first to
            // the last real token — the gap right after `{` or right
            // before `}` can itself be original (possibly inconsistent)
            // whitespace that must be replaced too, not left duplicated
            // alongside a freshly synthesized one.
            let delim_span = mac.delimiter.span();
            let range = delim_span.open().byte_range().end..delim_span.close().byte_range().start;
            let macro_start = mac.path.span().byte_range().start;
            self.found.push(ViewInvocation {
                range,
                macro_start,
                tokens: mac.tokens.clone(),
            });
        }
        visit::visit_macro(self, mac);
    }
}

/// Matches a bare `view!` and a qualified `florui::view!`/`::florui::view!`
/// — the two forms `florui`'s own documentation recognizes.
fn is_view_macro(mac: &syn::Macro) -> bool {
    mac.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "view")
}

/// True if `raw` contains anything that could be a real comment —
/// deliberately over-approximate; see this crate's own doc.
fn looks_uncertain(raw: &str) -> Option<String> {
    if raw.contains("//") {
        return Some("contains `//`, which may be a real comment".to_string());
    }
    if raw.contains("/*") {
        return Some("contains `/*`, which may be a real comment".to_string());
    }
    None
}

fn line_of(source: &str, byte_offset: usize) -> usize {
    source[..byte_offset.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1
}

fn indent_of_line(source: &str, byte_offset: usize) -> usize {
    let line_start = source[..byte_offset.min(source.len())]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    source[line_start..byte_offset.min(source.len())]
        .chars()
        .take_while(|c| *c == ' ')
        .count()
}

/// The whole replacement text for one `view! { ... }` invocation's own
/// body, at `indent` (the `view!` keyword's own line indent — the same
/// base [`format_source`] anchors the closing brace to). A single short
/// root node stays on `view!`'s own line (`view! { <button>...</button> }`,
/// the same shape this project's own real components already use, e.g.
/// `florui-style`'s own `styles_the_button_flagship_example_end_to_end`
/// test) — the same width-driven rule [`print_element`] already applies
/// to a single child one level down, just applied once more at the
/// outermost level. Anything else falls back to the standard
/// `view! {\n    ...\n}` block shape.
fn print_body(nodes: &[Node], indent: usize, source: &str) -> String {
    if let [only_node] = nodes {
        let inline = print_node(only_node, indent, source);
        // `view! { ` + body + ` }` — the two single-space paddings a
        // one-line `view!` invocation always has around its body.
        let padding = "view! { ".len() + " }".len();
        if fits_inline(indent + padding, &inline) {
            return format!(" {inline} ");
        }
    }
    let printed = print_nodes(nodes, indent + INDENT, source);
    format!("\n{printed}\n{}", " ".repeat(indent))
}

fn print_nodes(nodes: &[Node], indent: usize, source: &str) -> String {
    let pad = " ".repeat(indent);
    nodes
        .iter()
        .map(|node| format!("{pad}{}", print_node(node, indent, source)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_node(node: &Node, indent: usize, source: &str) -> String {
    match node {
        Node::Text(text) => text.clone(),
        Node::Expr(expr) => format!("{{{}}}", slice(source, expr.span())),
        Node::Element {
            tag,
            attrs,
            children,
            self_closing,
        } => print_element(tag, attrs, children, *self_closing, indent, source),
    }
}

fn print_element(
    tag: &syn::Ident,
    attrs: &[(syn::Ident, AttrValue)],
    children: &[Node],
    self_closing: bool,
    indent: usize,
    source: &str,
) -> String {
    let attr_strs: Vec<String> = attrs
        .iter()
        .map(|(name, value)| format!("{name}={}", print_attr_value(value, source)))
        .collect();

    let open_inline = if attr_strs.is_empty() {
        format!("<{tag}>")
    } else {
        format!("<{tag} {}>", attr_strs.join(" "))
    };

    if self_closing {
        let inline = if attr_strs.is_empty() {
            format!("<{tag} />")
        } else {
            format!("<{tag} {} />", attr_strs.join(" "))
        };
        if fits_inline(indent, &inline) {
            return inline;
        }
        return format!(
            "<{tag}\n{}\n{}/>",
            attr_strs
                .iter()
                .map(|attr| format!("{}{attr}", " ".repeat(indent + INDENT)))
                .collect::<Vec<_>>()
                .join("\n"),
            " ".repeat(indent),
        );
    }

    let close = format!("</{tag}>");

    // A single child (of any kind, including a nested element) that
    // still fits on one line stays inline — the same shape
    // `<span>{"hi"}</span>` and `<div class="stage"><div class="box" /></div>`
    // both already use throughout this project's own real components.
    // Purely width-driven, matching "short elements may remain inline";
    // *multiple* children always break one per line regardless of width
    // (below), since interleaving more than one sibling on the same line
    // reads ambiguously no matter how much room is left.
    if let [only_child] = children {
        let child_str = print_node(only_child, indent, source);
        let inline = format!("{open_inline}{child_str}{close}");
        if fits_inline(indent, &inline) {
            return inline;
        }
    }

    if children.is_empty() {
        let inline = format!("{open_inline}{close}");
        if fits_inline(indent, &inline) {
            return inline;
        }
    }

    let open = if fits_inline(indent, &open_inline) {
        open_inline
    } else {
        format!(
            "<{tag}\n{}\n{}>",
            attr_strs
                .iter()
                .map(|attr| format!("{}{attr}", " ".repeat(indent + INDENT)))
                .collect::<Vec<_>>()
                .join("\n"),
            " ".repeat(indent),
        )
    };

    let body = print_nodes(children, indent + INDENT, source);
    format!("{open}\n{body}\n{}{close}", " ".repeat(indent))
}

fn print_attr_value(value: &AttrValue, source: &str) -> String {
    match value {
        AttrValue::Lit(lit) => slice(source, lit.span()).to_string(),
        AttrValue::Expr(expr) => format!("{{{}}}", slice(source, expr.span())),
    }
}

fn slice(source: &str, span: proc_macro2::Span) -> &str {
    let range = span.byte_range();
    &source[range]
}

/// Runs the real `rustfmt` binary over `source` via stdin/stdout —
/// never a hand-rolled Rust printer for anything outside a `view!` body,
/// per this crate's own doc.
fn run_rustfmt(source: &str, edition: &str, config_dir: Option<&Path>) -> Result<String, String> {
    use std::io::Write;

    let mut command = Command::new("rustfmt");
    command
        .args(["--emit", "stdout", "--quiet", "--edition", edition])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // rustfmt looks its configuration up from the working directory when
    // the source comes over stdin.
    if let Some(dir) = config_dir {
        command.current_dir(dir);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not run rustfmt: {error}"))?;

    child
        .stdin
        .take()
        .ok_or("rustfmt gave no stdin handle")?
        .write_all(source.as_bytes())
        .map_err(|error| format!("could not write to rustfmt's stdin: {error}"))?;

    let output = child
        .wait_with_output()
        .map_err(|error| format!("could not wait for rustfmt: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "rustfmt exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("rustfmt gave non-UTF-8 output: {error}"))
}

/// Whether `path` should even be considered for formatting — `.rs`
/// files only, per this crate's own first-version scope.
pub fn is_formattable(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "rs")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(source: &str) -> FormatOutcome {
        format_source(source, "2024", None).expect("well-formed test input should always format")
    }

    #[test]
    fn rustfmt_reads_the_configuration_of_the_given_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rustfmt.toml"), "hard_tabs = true\n").unwrap();
        let source = "fn f() {\n    let x = 1;\n}\n";

        let configured = format_source(source, "2024", Some(dir.path())).unwrap();
        let plain = format_source(source, "2024", None).unwrap();

        assert_eq!(configured.output, "fn f() {\n\tlet x = 1;\n}\n");
        assert_eq!(plain.output, source);
    }

    #[test]
    fn reindents_a_badly_indented_view_body() {
        // Two children force the multi-line path even after the
        // width-driven single-child inlining fix below — this test's
        // own point is reindentation across real lines, not inlining.
        let source = r#"
fn root() -> Element {
    view! {
                <div class="card">
            <h2>{"Hello"}</h2>
                    <p>More</p>
        </div>
    }
}
"#;
        let outcome = format(source);
        assert!(outcome.skipped.is_empty());
        assert!(
            outcome
                .output
                .contains("    view! {\n        <div class=\"card\">\n"),
            "output was:\n{}",
            outcome.output
        );
    }

    #[test]
    fn is_idempotent_on_its_own_output() {
        let source = r#"
fn root() -> Element {
    view! {
        <div class="card">
            <h2>{"Hello"}</h2>
            <p>Some text here</p>
        </div>
    }
}
"#;
        let once = format(source).output;
        let twice = format(&once).output;
        assert_eq!(
            once, twice,
            "formatting the formatter's own output must be a no-op"
        );
    }

    #[test]
    fn a_single_text_child_stays_inline() {
        let source = r#"
fn root() -> Element {
    view! {
        <span class="count">{count.get().to_string()}</span>
    }
}
"#;
        let outcome = format(source);
        assert!(outcome.skipped.is_empty());
        assert!(
            outcome
                .output
                .contains(r#"<span class="count">{count.get().to_string()}</span>"#),
            "output was:\n{}",
            outcome.output
        );
    }

    #[test]
    fn a_view_containing_a_line_comment_is_skipped_not_mangled() {
        let source = r#"
fn root() -> Element {
    view! {
        <div>
            // a real comment a naive reformat would drop
            <h2>{"Hello"}</h2>
        </div>
    }
}
"#;
        let outcome = format(source);
        assert_eq!(outcome.skipped.len(), 1);
        assert!(outcome.output.contains("// a real comment"));
    }

    #[test]
    fn preserves_an_attribute_expression_byte_for_byte() {
        let source = r#"
fn root() -> Element {
    view! {
        <button onclick={move || clicked.set(clicked.get() + 1)}>{"+1"}</button>
    }
}
"#;
        let outcome = format(source);
        assert!(
            outcome
                .output
                .contains("onclick={move || clicked.set(clicked.get() + 1)}"),
            "output was:\n{}",
            outcome.output
        );
    }

    #[test]
    fn a_self_closing_void_element_stays_inline_when_it_fits() {
        let source = r#"
fn root() -> Element {
    view! {
        <br />
    }
}
"#;
        let outcome = format(source);
        assert!(outcome.output.contains("<br />"));
    }

    #[test]
    fn a_single_element_child_that_fits_stays_inline_not_just_text_or_expr() {
        // Matches this project's own real components (filter_test.rs,
        // transform_test.rs): a short single-Element child stays inline
        // exactly like a single text/expr child already does, purely
        // width-driven.
        let source = r#"
fn root() -> Element {
    view! {
        <div class="stage">
            <div class="box none"></div>
        </div>
    }
}
"#;
        let outcome = format(source);
        assert!(
            outcome
                .output
                .contains(r#"<div class="stage"><div class="box none"></div></div>"#),
            "output was:\n{}",
            outcome.output
        );
    }

    #[test]
    fn multiple_children_always_break_one_per_line_even_if_they_would_fit() {
        let source = r#"
fn root() -> Element {
    view! {
        <div>
            <span>{"a"}</span>
            <span>{"b"}</span>
        </div>
    }
}
"#;
        let outcome = format(source);
        assert!(
            outcome.output.contains("<div>\n") && outcome.output.contains("</div>"),
            "output was:\n{}",
            outcome.output
        );
        assert!(
            !outcome
                .output
                .contains(r#"<span>{"a"}</span><span>{"b"}</span>"#),
            "two siblings must never share one line regardless of width, output was:\n{}",
            outcome.output
        );
    }

    #[test]
    fn ordinary_rust_outside_view_is_still_reformatted_by_real_rustfmt() {
        let source = "fn root( ) -> Element { view! { <br /> } }";
        let outcome = format(source);
        assert!(
            outcome.output.contains("fn root() -> Element {"),
            "output was:\n{}",
            outcome.output
        );
    }
}
