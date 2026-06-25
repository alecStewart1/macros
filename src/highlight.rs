use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;

use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Language, Node, Parser, Point, Query, QueryCursor, Tree};
use tree_sitter_language::LanguageFn;

/// The vendored Mermaid grammar (`grammars/tree-sitter-mermaid`), compiled from
/// C by build.rs. No crate exposes Mermaid against tree-sitter 0.26, so we link
/// its generated `tree_sitter_mermaid()` and wrap it as a `LanguageFn`, matching
/// the `LANGUAGE` const every other grammar crate provides.
mod mermaid {
    use tree_sitter_language::LanguageFn;

    unsafe extern "C" {
        fn tree_sitter_mermaid() -> *const ();
    }

    pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_mermaid) };
}

/// The capture names we recognize, in the order tree-sitter assigns highlight
/// indices. Each maps 1:1 to a theme face of the same name. Dotted captures in
/// the grammar's query (e.g. `constant.builtin`) resolve to their prefix here.
pub const FACE_NAMES: &[&str] = &[
    "comment",
    "string",
    "keyword",
    "function",
    "number",
    "constant",
    "type",
    "operator",
    "punctuation",
    "variable",
];

/// The theme face name for a highlight index.
pub fn face_name(face: usize) -> &'static str {
    FACE_NAMES.get(face).copied().unwrap_or("variable")
}

/// A contiguous run of source bytes and the face (if any) to draw it with.
pub struct HlSpan {
    pub range: Range<usize>,
    pub face: Option<usize>,
}

/// The byte and point deltas of a single edit, for incremental reparsing.
/// Points are `(row, byte-column)` pairs, as tree-sitter expects.
pub struct Edit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_point: (usize, usize),
    pub old_end_point: (usize, usize),
    pub new_end_point: (usize, usize),
}

/// A tree-sitter highlighter that keeps a persistent parse tree.
///
/// Parsing reads directly from the rope in chunks (no whole-buffer string copy)
/// and reuses the previous tree, so an edit reparses incrementally. Highlight
/// spans aren't cached: [`spans_in`](Self::spans_in) runs the query over just a
/// byte range on demand, so the painter asks only for the visible region.
pub struct Highlighter {
    parser: Parser,
    query: Query,
    tree: Option<Tree>,
    /// The grammar, kept so scripts can compile ad-hoc queries against it
    /// (`query_captures`) without us re-resolving the language.
    language: Language,
    /// Maps a query capture index to a face index (`None` if unrecognized).
    capture_faces: Vec<Option<usize>>,
    /// Language injection (e.g. fenced code blocks in markdown): a query that
    /// finds `(language, content)` regions, plus a lazily-built, per-language
    /// cache of sub-highlighters that color those regions. `None` for grammars
    /// that don't inject. Interior-mutable so it can warm during `spans_in`.
    injection: Option<Injection>,
}

/// Detection + sub-highlighter cache for language injection.
struct Injection {
    query: Query,
    lang_capture: u32,
    content_capture: u32,
    cache: RefCell<HashMap<String, Option<Box<Highlighter>>>>,
}

impl Highlighter {
    /// Build a highlighter from a grammar's `LanguageFn` and highlight query, or
    /// `None` if the grammar/query won't load. This is grammar-agnostic: capture
    /// names map to faces by [`face_for_capture`], so any standard tree-sitter
    /// highlights query works.
    pub fn new(language_fn: LanguageFn, query: &str) -> Option<Self> {
        let language = Language::new(language_fn);
        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;
        let query = Query::new(&language, query).ok()?;
        let capture_faces = query
            .capture_names()
            .iter()
            .map(|name| face_for_capture(name))
            .collect();
        Some(Self {
            parser,
            query,
            tree: None,
            language,
            capture_faces,
            injection: None,
        })
    }

    /// Build a Scheme highlighter, or `None` if the grammar/query won't load.
    pub fn scheme() -> Option<Self> {
        Self::new(
            tree_sitter_scheme::LANGUAGE,
            tree_sitter_scheme::HIGHLIGHTS_QUERY,
        )
    }

    /// A highlighter for a language id (the registry of bundled grammars), or
    /// `None` when no grammar is bundled for it. Wired from file extensions by
    /// `language_for_path` and the active mode's language.
    pub fn for_language(language: &str) -> Option<Self> {
        let (lang, query) = grammar(language)?;
        let mut hl = Self::new(lang, query)?;
        if let Some(src) = injection_query(language) {
            hl.injection = build_injection(&hl.language, src);
        }
        Some(hl)
    }
}

/// The injection-detection query for a grammar that embeds other languages, or
/// `None`. Markdown fenced code blocks carry a language tag and content we color
/// with that language's own grammar.
fn injection_query(language: &str) -> Option<&'static str> {
    match language {
        "markdown" => Some(
            "(fenced_code_block \
               (info_string (language) @injection.language) \
               (code_fence_content) @injection.content)",
        ),
        _ => None,
    }
}

/// Compile an injection query and locate its `@injection.language` /
/// `@injection.content` captures. `None` if the query won't compile or is
/// missing either capture.
fn build_injection(language: &Language, src: &str) -> Option<Injection> {
    let query = Query::new(language, src).ok()?;
    let names = query.capture_names();
    let idx = |want: &str| names.iter().position(|n| *n == want).map(|i| i as u32);
    Some(Injection {
        lang_capture: idx("injection.language")?,
        content_capture: idx("injection.content")?,
        query,
        cache: RefCell::new(HashMap::new()),
    })
}

/// Map a fenced-block info-string tag (`rust`, `rs`, `py`, `c++`, …) to a
/// bundled grammar id, or `None` if we don't bundle it. Markdown is excluded so
/// a nested ```markdown block can't recurse.
fn injection_lang_id(tag: &str) -> Option<&'static str> {
    Some(match tag.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => "rust",
        "python" | "py" => "python",
        "javascript" | "js" | "jsx" | "mjs" => "javascript",
        "typescript" | "ts" => "typescript",
        "tsx" => "tsx",
        "json" => "json",
        "c" | "h" => "c",
        "cpp" | "c++" | "cc" | "cxx" | "hpp" => "cpp",
        "go" => "go",
        "html" => "html",
        "css" => "css",
        "bash" | "sh" | "shell" | "zsh" | "console" => "bash",
        "java" => "java",
        "ruby" | "rb" => "ruby",
        "lua" => "lua",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "scheme" | "scm" | "lisp" | "racket" => "scheme",
        "zig" => "zig",
        "r" => "r",
        "ocaml" | "ml" => "ocaml",
        "haskell" | "hs" => "haskell",
        _ => return None,
    })
}

/// The bundled `(LanguageFn, highlights-query)` for a language id, if any.
/// Adding a language is a single entry here plus its grammar crate in Cargo.toml
/// and an extension mapping in `language_for_path`.
fn grammar(language: &str) -> Option<(LanguageFn, &'static str)> {
    let entry = match language {
        "scheme" => (
            tree_sitter_scheme::LANGUAGE,
            tree_sitter_scheme::HIGHLIGHTS_QUERY,
        ),
        "rust" => (
            tree_sitter_rust::LANGUAGE,
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        ),
        "python" => (
            tree_sitter_python::LANGUAGE,
            tree_sitter_python::HIGHLIGHTS_QUERY,
        ),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE,
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        ),
        "json" => (
            tree_sitter_json::LANGUAGE,
            tree_sitter_json::HIGHLIGHTS_QUERY,
        ),
        "c" => (tree_sitter_c::LANGUAGE, tree_sitter_c::HIGHLIGHT_QUERY),
        "cpp" => (tree_sitter_cpp::LANGUAGE, tree_sitter_cpp::HIGHLIGHT_QUERY),
        "go" => (tree_sitter_go::LANGUAGE, tree_sitter_go::HIGHLIGHTS_QUERY),
        "html" => (
            tree_sitter_html::LANGUAGE,
            tree_sitter_html::HIGHLIGHTS_QUERY,
        ),
        "css" => (tree_sitter_css::LANGUAGE, tree_sitter_css::HIGHLIGHTS_QUERY),
        "bash" => (
            tree_sitter_bash::LANGUAGE,
            tree_sitter_bash::HIGHLIGHT_QUERY,
        ),
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ),
        "java" => (
            tree_sitter_java::LANGUAGE,
            tree_sitter_java::HIGHLIGHTS_QUERY,
        ),
        "ruby" => (
            tree_sitter_ruby::LANGUAGE,
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
        ),
        "lua" => (tree_sitter_lua::LANGUAGE, tree_sitter_lua::HIGHLIGHTS_QUERY),
        "yaml" => (
            tree_sitter_yaml::LANGUAGE,
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
        ),
        "toml" => (
            tree_sitter_toml_ng::LANGUAGE,
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        ),
        // Markdown is a split block/inline grammar; we highlight block structure
        // (headings, code fences, lists) with the block parser.
        "markdown" => (
            tree_sitter_md::LANGUAGE,
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
        ),
        "zig" => (tree_sitter_zig::LANGUAGE, tree_sitter_zig::HIGHLIGHTS_QUERY),
        "r" => (tree_sitter_r::LANGUAGE, tree_sitter_r::HIGHLIGHTS_QUERY),
        "ocaml" => (
            tree_sitter_ocaml::LANGUAGE_OCAML,
            tree_sitter_ocaml::HIGHLIGHTS_QUERY,
        ),
        "haskell" => (
            tree_sitter_haskell::LANGUAGE,
            tree_sitter_haskell::HIGHLIGHTS_QUERY,
        ),
        // LaTeX, Typst, and Mermaid bundle no highlights query with the parser,
        // so we vendor nvim-treesitter's under `grammars/queries/`. (LaTeX/Typst
        // parsers come from the `codebook-*` crates; Mermaid from build.rs.)
        "latex" => (
            codebook_tree_sitter_latex::LANGUAGE,
            include_str!("../grammars/queries/latex.scm"),
        ),
        "typst" => (
            codebook_tree_sitter_typst::LANGUAGE,
            include_str!("../grammars/queries/typst.scm"),
        ),
        "mermaid" => (
            mermaid::LANGUAGE,
            include_str!("../grammars/queries/mermaid.scm"),
        ),
        _ => return None,
    };
    Some(entry)
}

impl Highlighter {
    /// Full parse from scratch — used on initial load and after undo, where the
    /// edit deltas aren't tracked.
    pub fn set_text(&mut self, rope: &Rope) {
        self.tree = parse(&mut self.parser, rope, None);
    }

    /// Apply an edit to the current tree. Call this (with the pre-edit byte/point
    /// positions) before `reparse`.
    pub fn apply_edit(&mut self, edit: &Edit) {
        if let Some(tree) = self.tree.as_mut() {
            tree.edit(&InputEdit {
                start_byte: edit.start_byte,
                old_end_byte: edit.old_end_byte,
                new_end_byte: edit.new_end_byte,
                start_position: point(edit.start_point),
                old_end_position: point(edit.old_end_point),
                new_end_position: point(edit.new_end_point),
            });
        }
    }

    /// Incrementally reparse, reusing the edited tree.
    pub fn reparse(&mut self, rope: &Rope) {
        self.tree = parse(&mut self.parser, rope, self.tree.as_ref());
    }

    /// Highlight spans tiling `range`, computed on demand by querying just that
    /// byte range of the current tree. Reading node text for query predicates
    /// goes straight to the rope.
    pub fn spans_in(&self, rope: &Rope, range: Range<usize>) -> Vec<HlSpan> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());

        let provider = |node: Node| {
            rope.byte_slice(node.byte_range())
                .chunks()
                .map(str::as_bytes)
        };
        let mut captures: Vec<(usize, usize, usize)> = Vec::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), provider);
        while let Some(m) = matches.next() {
            for capture in m.captures {
                if let Some(face) = self
                    .capture_faces
                    .get(capture.index as usize)
                    .copied()
                    .flatten()
                {
                    let node_range = capture.node.byte_range();
                    let start = node_range.start.max(range.start);
                    let end = node_range.end.min(range.end);
                    if end > start {
                        captures.push((start, end, face));
                    }
                }
            }
        }
        // Injected sub-language spans (e.g. a ```rust block). They nest inside
        // the markdown `text.literal` span covering the block, so `flatten`'s
        // innermost-wins sweep makes them take precedence over the base face.
        self.append_injected_spans(rope, &range, &mut captures);
        flatten(captures, range.start, range.end)
    }

    /// For each injected region (fenced code block) intersecting `range`, run the
    /// embedded language's highlighter over its content and push the resulting
    /// spans (offset into the buffer, clipped to `range`) onto `captures`. No-op
    /// unless this grammar injects. Only visible regions are parsed, so the cost
    /// scales with the viewport, not the whole file.
    fn append_injected_spans(
        &self,
        rope: &Rope,
        range: &Range<usize>,
        captures: &mut Vec<(usize, usize, usize)>,
    ) {
        let (Some(inj), Some(tree)) = (self.injection.as_ref(), self.tree.as_ref()) else {
            return;
        };
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let provider = |node: Node| {
            rope.byte_slice(node.byte_range())
                .chunks()
                .map(str::as_bytes)
        };
        let mut cache = inj.cache.borrow_mut();
        let mut matches = cursor.matches(&inj.query, tree.root_node(), provider);
        while let Some(m) = matches.next() {
            let mut tag: Option<String> = None;
            let mut content: Option<Node> = None;
            for cap in m.captures {
                if cap.index == inj.lang_capture {
                    tag = Some(rope.byte_slice(cap.node.byte_range()).to_string());
                } else if cap.index == inj.content_capture {
                    content = Some(cap.node);
                }
            }
            let (Some(tag), Some(content)) = (tag, content) else {
                continue;
            };
            let Some(lang_id) = injection_lang_id(&tag) else {
                continue;
            };
            // Lazily build (and cache) the sub-highlighter for this language.
            let entry = cache
                .entry(lang_id.to_string())
                .or_insert_with(|| Highlighter::for_language(lang_id).map(Box::new));
            let Some(sub) = entry.as_mut() else {
                continue;
            };
            // Parse the whole block as a standalone document (so the sub-grammar
            // never sees a truncated fragment), then shift its spans to absolute
            // offsets and clip to the requested range. `set_byte_range` above
            // already limited us to blocks intersecting the viewport.
            let cr = content.byte_range();
            let cs = cr.start;
            let text: String = rope.byte_slice(cs..cr.end).to_string();
            let sub_rope = Rope::from_str(&text);
            sub.set_text(&sub_rope);
            for s in sub.spans_in(&sub_rope, 0..text.len()) {
                if let Some(face) = s.face {
                    let start = (s.range.start + cs).max(range.start);
                    let end = (s.range.end + cs).min(range.end);
                    if end > start {
                        captures.push((start, end, face));
                    }
                }
            }
        }
    }

    /// The chain of nodes covering `byte`, innermost first, each as
    /// `(kind, start_byte, end_byte)`. Empty when there's no tree yet. Lets a
    /// script ask "what am I inside?" — e.g. skip a bracket that sits in a string
    /// or comment node.
    pub fn node_at(&self, byte: usize) -> Vec<(String, usize, usize)> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let mut node = tree.root_node().descendant_for_byte_range(byte, byte);
        let mut chain = Vec::new();
        while let Some(n) = node {
            chain.push((n.kind().to_string(), n.start_byte(), n.end_byte()));
            node = n.parent();
        }
        chain
    }

    /// Run an arbitrary tree-sitter query over the current tree, returning each
    /// capture as `(capture_name, start_byte, end_byte, node_kind)`. Returns
    /// `None` if the query source won't compile against this grammar, and an
    /// empty vec if it compiles but matches nothing. Node text for query
    /// predicates is read straight from the rope.
    pub fn query_captures(
        &self,
        rope: &Rope,
        query_src: &str,
    ) -> Option<Vec<(String, usize, usize, String)>> {
        let tree = self.tree.as_ref()?;
        let query = Query::new(&self.language, query_src).ok()?;
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let provider = |node: Node| {
            rope.byte_slice(node.byte_range())
                .chunks()
                .map(str::as_bytes)
        };
        let mut out = Vec::new();
        let mut matches = cursor.matches(&query, tree.root_node(), provider);
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let name = names.get(capture.index as usize).copied().unwrap_or("");
                let node = capture.node;
                out.push((
                    name.to_string(),
                    node.start_byte(),
                    node.end_byte(),
                    node.kind().to_string(),
                ));
            }
        }
        Some(out)
    }
}

/// Parse `rope` by feeding tree-sitter chunks straight from the rope (no
/// whole-buffer string allocation), reusing `old_tree` for incremental reparse.
fn parse(parser: &mut Parser, rope: &Rope, old_tree: Option<&Tree>) -> Option<Tree> {
    let mut callback = |byte: usize, _: Point| -> &[u8] {
        if byte >= rope.len_bytes() {
            return &[];
        }
        let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - chunk_start..]
    };
    parser.parse_with_options(&mut callback, old_tree, None)
}

fn point((row, column): (usize, usize)) -> Point {
    Point::new(row, column)
}

/// Resolve a capture name to a face index, using the dotted prefix
/// (`string.special` -> `string`).
fn face_for_capture(name: &str) -> Option<usize> {
    let base = name.split('.').next().unwrap_or(name);
    if let Some(i) = FACE_NAMES.iter().position(|face| *face == base) {
        return Some(i);
    }
    // Many grammars use capture names that don't share a base with our 10
    // faces — markdown's `text.*`, HTML/CSS `tag`/`attribute`, CSS/YAML/TOML
    // `property`, Lua/R `conditional`/`repeat`, `boolean`, etc. Without aliasing
    // these onto the nearest existing face they render uncolored. Themes already
    // color the target faces, so no new face entries are needed. Names that
    // aren't visible faces (`none`, `spell`, `error`, `embedded`, `_*`) stay
    // unmapped on purpose.
    let alias = match base {
        // markdown block/inline structure
        "text" => match name {
            "text.title" | "text.strong" => "keyword", // headings, bold
            "text.emphasis" => "type",                 // italic
            "text.literal" => "string",                // code fences / spans
            "text.uri" => "constant",                  // link / image targets
            "text.reference" => "variable",            // link labels
            _ => return None,
        },
        // The `@markup.*` family (nvim-treesitter's modern markup naming, used by
        // the LaTeX / Typst / Mermaid queries) mirrors the `text.*` mapping above.
        "markup" => {
            if name.starts_with("markup.heading") || name == "markup.strong" {
                "keyword" // headings, bold
            } else if name == "markup.italic" {
                "type" // italic
            } else if name.starts_with("markup.raw") {
                "string" // inline code / code blocks
            } else if name == "markup.link.url" || name == "markup.math" {
                "constant" // link targets, math content
            } else if name.starts_with("markup.link") {
                "variable" // link labels
            } else {
                return None;
            }
        }
        "boolean" => "constant",            // true / false / nil
        "character" | "escape" => "string", // char literals, \n
        "property" | "field" | "parameter" | "attribute" => "variable", // keys, obj.prop, attrs
        "tag" | "conditional" | "repeat" | "label" | "preproc" => "keyword", // <tag>, if/for, #define
        "constructor" | "namespace" | "module" => "type",                    // Foo(), modules
        "method" => "function",                                              // method calls
        "delimiter" => "punctuation",                                        // bare `@delimiter`
        _ => return None,
    };
    FACE_NAMES.iter().position(|face| *face == alias)
}

/// Flatten possibly-nested capture ranges into contiguous spans tiling
/// `[start, end)`. Query captures are tree nodes, so any two are either disjoint
/// or nested — never partially overlapping — which lets a simple stack sweep
/// pick the innermost (most specific) face at each byte.
fn flatten(mut captures: Vec<(usize, usize, usize)>, start: usize, end: usize) -> Vec<HlSpan> {
    // Sort by start ascending, then by end descending so that at a shared start
    // the outer (larger) capture is processed before the inner one.
    captures.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

    let mut spans = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (end, face), innermost on top
    let mut pos = start;
    let mut i = 0;
    while pos < end {
        while let Some(&(span_end, _)) = stack.last() {
            if span_end <= pos {
                stack.pop();
            } else {
                break;
            }
        }
        while i < captures.len() && captures[i].0 <= pos {
            let (_, span_end, face) = captures[i];
            if span_end > pos {
                stack.push((span_end, face));
            }
            i += 1;
        }

        let next_start = captures.get(i).map_or(end, |c| c.0);
        let top_end = stack.last().map_or(end, |&(span_end, _)| span_end);
        let seg_end = next_start.min(top_end).min(end);
        let face = stack.last().map(|&(_, face)| face);
        if seg_end > pos {
            spans.push(HlSpan {
                range: pos..seg_end,
                face,
            });
            pos = seg_end;
        } else {
            break; // safety: never seen, but guarantees termination
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faces(spans: &[HlSpan]) -> Vec<&str> {
        spans.iter().filter_map(|s| s.face.map(face_name)).collect()
    }

    /// Every bundled grammar's parser AND highlight query must load — a query
    /// using syntax the tree-sitter version rejects would otherwise silently
    /// disable highlighting for that language.
    #[test]
    fn all_bundled_grammars_load() {
        for lang in [
            "scheme",
            "rust",
            "python",
            "javascript",
            "json",
            "c",
            "cpp",
            "go",
            "html",
            "css",
            "bash",
            "typescript",
            "tsx",
            "java",
            "ruby",
            "lua",
            "yaml",
            "toml",
            "markdown",
            "zig",
            "r",
            "ocaml",
            "haskell",
            "latex",
            "typst",
            "mermaid",
        ] {
            assert!(
                Highlighter::for_language(lang).is_some(),
                "grammar or highlights query failed to load for {lang}"
            );
        }
        assert!(Highlighter::for_language("cobol").is_none());
    }

    /// A non-Scheme grammar actually colors tokens (Rust here).
    #[test]
    fn rust_grammar_colors() {
        let mut h = Highlighter::for_language("rust").unwrap();
        let rope = Rope::from_str("fn main() { let x = 1; }");
        h.set_text(&rope);
        let spans = h.spans_in(&rope, 0..rope.len_bytes());
        let names = faces(&spans);
        assert!(
            names.contains(&"keyword"),
            "expected a keyword face, got {names:?}"
        );
    }

    /// The three newest grammars (LaTeX/Typst via the `codebook-*` crates,
    /// Mermaid vendored + compiled by build.rs) parse and emit colored spans.
    /// Catches a grammar that loads but whose vendored query matches nothing.
    #[test]
    fn latex_typst_mermaid_grammars_color() {
        let cases = [
            ("latex", "\\section{Hi}\n% a comment\n", "comment"),
            ("typst", "= Heading\n#let x = 1\n", "keyword"),
            (
                "mermaid",
                "sequenceDiagram\n  Alice->>John: hi\n",
                "keyword",
            ),
        ];
        for (lang, src, want) in cases {
            let mut h = Highlighter::for_language(lang).unwrap();
            let rope = Rope::from_str(src);
            h.set_text(&rope);
            let spans = h.spans_in(&rope, 0..rope.len_bytes());
            let names = faces(&spans);
            assert!(
                names.contains(&want),
                "{lang}: expected a {want} face, got {names:?}"
            );
        }
    }

    /// The `markup.*` aliases (used by the LaTeX/Typst/Mermaid queries) resolve.
    #[test]
    fn markup_captures_alias_to_faces() {
        assert_eq!(
            face_for_capture("markup.heading.1").map(face_name),
            Some("keyword")
        );
        assert_eq!(
            face_for_capture("markup.strong").map(face_name),
            Some("keyword")
        );
        assert_eq!(
            face_for_capture("markup.italic").map(face_name),
            Some("type")
        );
        assert_eq!(
            face_for_capture("markup.raw.block").map(face_name),
            Some("string")
        );
        assert_eq!(
            face_for_capture("markup.link.url").map(face_name),
            Some("constant")
        );
        assert_eq!(
            face_for_capture("markup.link.label").map(face_name),
            Some("variable")
        );
    }

    /// Guard: every bundled grammar's highlight captures must resolve to a face,
    /// except a small set of capture names that are deliberately not visible
    /// faces (spell/region markers). Catches a future grammar (or one whose
    /// query changes) using capture names we don't map — the bug that left
    /// markdown, HTML, CSS, YAML, Lua, etc. effectively uncolored.
    #[test]
    fn every_grammar_capture_resolves_to_a_face() {
        // Not visible faces: injected-region and spellcheck markers, error nodes.
        const ALLOWED_UNMAPPED: &[&str] = &["embedded", "spell", "nospell", "conceal", "error"];
        let langs = [
            "scheme",
            "rust",
            "python",
            "javascript",
            "json",
            "c",
            "cpp",
            "go",
            "html",
            "css",
            "bash",
            "typescript",
            "tsx",
            "java",
            "ruby",
            "lua",
            "yaml",
            "toml",
            "markdown",
            "zig",
            "r",
            "ocaml",
            "haskell",
            "latex",
            "typst",
            "mermaid",
        ];
        for lang in langs {
            let (langfn, query_src) = grammar(lang).unwrap();
            let language: Language = langfn.into();
            let query = Query::new(&language, query_src).unwrap();
            let bad: Vec<&str> = query
                .capture_names()
                .iter()
                .copied()
                .filter(|n| {
                    *n != "none"
                        && !n.starts_with('_')
                        && !ALLOWED_UNMAPPED.contains(&n.split('.').next().unwrap_or(n))
                        && face_for_capture(n).is_none()
                })
                .collect();
            assert!(
                bad.is_empty(),
                "{lang}: capture names resolve to no face (would render uncolored): {bad:?}"
            );
        }
    }

    /// Markdown headings and fenced code must color: the grammar's `text.*`
    /// captures are aliased onto existing faces (regression for no-highlight).
    #[test]
    fn markdown_grammar_colors() {
        let mut h = Highlighter::for_language("markdown").unwrap();
        let rope = Rope::from_str("# Title\n\n```\ncode\n```\n");
        h.set_text(&rope);
        let spans = h.spans_in(&rope, 0..rope.len_bytes());
        let names = faces(&spans);
        assert!(
            names.contains(&"keyword"),
            "expected a heading (keyword) face, got {names:?}"
        );
        assert!(
            names.contains(&"string"),
            "expected a code (string) face, got {names:?}"
        );
    }

    /// A fenced ```rust block is highlighted with Rust's own grammar
    /// (injection): the `fn`/`let` keywords inside the block produce a `keyword`
    /// face, which the markdown grammar alone never emits.
    #[test]
    fn markdown_injects_fenced_code_language() {
        let mut h = Highlighter::for_language("markdown").unwrap();
        let src = "# Title\n\n```rust\nfn main() { let x = 1; }\n```\n";
        let rope = Rope::from_str(src);
        h.set_text(&rope);
        let spans = h.spans_in(&rope, 0..rope.len_bytes());

        // The `fn`/`let` keywords sit inside the code block; find a keyword span
        // whose range is within the fenced content (after the ```rust line).
        let code_start = src.find("fn main").unwrap();
        let keyword_in_code = spans
            .iter()
            .any(|s| s.face.map(face_name) == Some("keyword") && s.range.start >= code_start);
        assert!(
            keyword_in_code,
            "expected Rust keyword highlighting inside the fenced block, got {:?}",
            shape(&spans)
        );
    }

    /// An unknown / untagged fence is left to the markdown grammar (no panic, no
    /// injection) — only the base `text.literal`→string face applies.
    #[test]
    fn markdown_unknown_fence_has_no_injection() {
        let mut h = Highlighter::for_language("markdown").unwrap();
        let rope = Rope::from_str("```nonsense-lang\nx y z\n```\n");
        h.set_text(&rope);
        let spans = h.spans_in(&rope, 0..rope.len_bytes());
        let names = faces(&spans);
        assert!(
            !names.contains(&"keyword"),
            "unknown fence should not inject any grammar, got {names:?}"
        );
    }

    /// The `text.*` capture aliases resolve; `none` stays unmapped.
    #[test]
    fn markdown_text_captures_alias_to_faces() {
        assert_eq!(
            face_for_capture("text.title").map(face_name),
            Some("keyword")
        );
        assert_eq!(
            face_for_capture("text.literal").map(face_name),
            Some("string")
        );
        assert_eq!(
            face_for_capture("text.uri").map(face_name),
            Some("constant")
        );
        assert_eq!(
            face_for_capture("text.reference").map(face_name),
            Some("variable")
        );
        assert_eq!(face_for_capture("none"), None);
        // Existing dotted captures still resolve by prefix.
        assert_eq!(
            face_for_capture("punctuation.special").map(face_name),
            Some("punctuation")
        );
    }

    fn shape(spans: &[HlSpan]) -> Vec<(usize, usize, Option<usize>)> {
        spans
            .iter()
            .map(|s| (s.range.start, s.range.end, s.face))
            .collect()
    }

    #[test]
    fn scheme_grammar_loads_and_colors() {
        let rope = Rope::from_str("; a comment\n(define x \"hi\")\n");
        let mut highlighter = Highlighter::scheme().expect("Scheme grammar/query should load");
        highlighter.set_text(&rope);
        let spans = highlighter.spans_in(&rope, 0..rope.len_bytes());

        let faces = faces(&spans);
        assert!(!spans.is_empty(), "expected contiguous spans");
        assert!(
            faces.contains(&"comment"),
            "expected a comment face, got {faces:?}"
        );
        assert!(
            faces.contains(&"string"),
            "expected a string face, got {faces:?}"
        );
    }

    #[test]
    fn query_captures_finds_brackets_skipping_strings() {
        // A "(" inside a string must NOT be reported as a bracket node — the
        // whole reason a script can trust the tree over raw character scanning.
        let rope = Rope::from_str("(define x \"a ( b\")\n");
        let mut h = Highlighter::scheme().unwrap();
        h.set_text(&rope);

        let caps = h
            .query_captures(&rope, "[\"(\" \")\"] @b")
            .expect("query should compile");
        // Two real parens: the opening at 0 and the closing at 17. The "(" at
        // byte 13 lives inside the string and is not a node.
        let starts: Vec<usize> = caps.iter().map(|(_, s, _, _)| *s).collect();
        assert_eq!(starts, vec![0, 17], "got {caps:?}");
        assert!(
            caps.iter()
                .all(|(name, _, _, kind)| name == "b" && (kind == "(" || kind == ")"))
        );

        // A malformed query reports failure rather than panicking.
        assert!(h.query_captures(&rope, "(((").is_none());
    }

    #[test]
    fn node_at_reports_enclosing_chain() {
        let rope = Rope::from_str("(define x 1)\n");
        let mut h = Highlighter::scheme().unwrap();
        h.set_text(&rope);
        // Byte 8 is the `x` symbol; its chain should climb to the list and root.
        let chain = h.node_at(8);
        let kinds: Vec<&str> = chain.iter().map(|(k, _, _)| k.as_str()).collect();
        assert!(
            kinds.contains(&"list"),
            "expected a list ancestor, got {kinds:?}"
        );
        // Innermost is first, the document root last.
        assert_eq!(chain.last().map(|(k, _, _)| k.as_str()), Some("program"));
    }

    #[test]
    fn incremental_reparse_matches_full_parse() {
        let before = Rope::from_str("(define x 1)\n");
        let after = Rope::from_str("(define x 1)\n(define y 2)\n");

        let mut h = Highlighter::scheme().unwrap();
        h.set_text(&before);
        h.apply_edit(&Edit {
            start_byte: before.len_bytes(),
            old_end_byte: before.len_bytes(),
            new_end_byte: after.len_bytes(),
            start_point: (1, 0),
            old_end_point: (1, 0),
            new_end_point: (2, 0),
        });
        h.reparse(&after);
        let incremental = h.spans_in(&after, 0..after.len_bytes());

        let mut fresh = Highlighter::scheme().unwrap();
        fresh.set_text(&after);
        let full = fresh.spans_in(&after, 0..after.len_bytes());

        assert_eq!(
            shape(&incremental),
            shape(&full),
            "incremental must match full parse"
        );
    }

    #[test]
    fn range_restricted_query_matches_the_full_slice() {
        // Querying just the second line should give the same spans (offset-wise)
        // as slicing the full-buffer spans to that range.
        let rope = Rope::from_str("(define x 1)\n; comment here\n(define y 2)\n");
        let mut h = Highlighter::scheme().unwrap();
        h.set_text(&rope);

        let line2 = 13..28; // the comment line's byte range
        let restricted = h.spans_in(&rope, line2.clone());
        assert!(
            faces(&restricted).contains(&"comment"),
            "range query should still see the comment"
        );
        // Spans stay within the requested range.
        assert!(
            restricted
                .iter()
                .all(|s| s.range.start >= line2.start && s.range.end <= line2.end)
        );
    }
}
