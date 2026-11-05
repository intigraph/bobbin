//! Live-lane IRI scheme for code/document entities.
//!
//! `bobbin:` ≡ `aegis:` (ian's aegis-6noan ruling §1): `code-entities.ttl:14`
//! binds the `bobbin:` prefix to the aegis ontology namespace, so there is one
//! vocabulary, not two. Entity IRIs live under the same namespace.
//!
//! WHICH LANE THIS REPRODUCES, AND WHY IT CHANGED (aegis-6noan).
//!
//! This module used to implement quipu's `scripts/ingest-repos.py` /
//! `namespace.rs::CODE_BASE` lane — `http://aegis.gastown.local/code/` with
//! `name-L{line}` symbols and `S{line}` sections. It implemented it *exactly*,
//! down to that lane's own stated reason for the line suffix. That lane is
//! real, declared, and **superseded**: measured 2026-08-24 against the deployed
//! store it has **0 live instances**, and it appears nowhere in the hourly
//! ingest cron.
//!
//! The live producer is **yupana** (`yupana-src/src/export.rs:513-575`), which
//! `~/.local/lib/aegis/quipu-ingest-cron.sh:22` runs via `code-promote.sh`, and
//! which minted all **10,433** live code entities. This module now reproduces
//! yupana's scheme so a bobbin entity and a yupana entity for the same symbol are
//! ONE node:
//!
//! ```text
//! module    {ONTOLOGY_NS}code/{repo}/{path with '/' -> %2F}
//! symbol    {module}::{name}                       (NOT a line number)
//! document  {ONTOLOGY_NS}doc/{repo}/{path with '/' -> %2F}
//! section   {document}#{github-style-slug}         (NOT S{line})
//! ```
//!
//! `-L{line}` is not merely the other lane's spelling — it is unstable. A
//! symbol's identity must survive edits ELSEWHERE in its file, and with a line
//! suffix, inserting one line above a function re-mints it on the next reindex
//! and orphans the old node. yupana keeps same-named symbols apart with a scope
//! chain (`{module}::Foo::bar`) for exactly this reason (aegis-1q14: without
//! it, 42 same-kind collisions merged and unioned different symbols' call
//! edges).
//!
//! KNOWN, DELIBERATE GAP — bobbin cannot mint yupana's scope chain. `Chunk`
//! carries `name` and no enclosing scope, so a method is `{module}::bar` here
//! and `{module}::Foo::bar` in yupana. That is a MISS (two nodes), never a false
//! merge, and two same-named symbols in one bobbin file collapse to one node.
//! Under-merging is the safe direction and it is a strict improvement on the
//! 0-of-10,433 overlap the superseded lane produced. Closing it needs the
//! parser to carry scope; that is separate work, not a blocker.
//!
//! Shared here (feature-ungated) so the local entities table and the
//! knowledge-gated quipu emitters mint identical identities.

/// Vocabulary namespace (`bobbin:name`, `bobbin:Chunk`, …).
pub const ONTOLOGY_NS: &str = "http://aegis.gastown.local/ontology/";

/// Entity namespace for code module/symbol IRIs (yupana `export.rs:513`).
pub const CODE_BASE: &str = "http://aegis.gastown.local/ontology/code/";

/// Entity namespace for document/section IRIs (yupana `export.rs:575`).
pub const DOC_BASE: &str = "http://aegis.gastown.local/ontology/doc/";

/// Entity namespace for `bobbin:Chunk` spans.
///
/// A distinct top-level lane on purpose. Chunks are bobbin's own referent — a
/// retrieval SPAN, which ian ratified as genuinely not a `CodeSymbol` — and
/// yupana mints nothing chunk-shaped, so there is no contract to match. Hanging
/// them off the code lane instead (`{module}/C{line}`) would make a contract
/// reader parse a chunk as a CodeModule whose path ends in `/C12`, which is
/// the silent-misparse failure this whole bead is about.
pub const CHUNK_BASE: &str = "http://aegis.gastown.local/ontology/chunk/";

/// Entity namespace for quarantined model-inferred candidates.
///
/// Also deliberately off the code lane: an inferred candidate must never be
/// parseable as an observed code entity, and `{CODE_BASE}{repo}/inferred/…`
/// (what this used to build) reads as exactly that.
pub const INFERRED_BASE: &str = "http://aegis.gastown.local/ontology/inferred/";

/// Percent-encode one IRI segment, matching yupana's `export.rs::iri_segment`.
///
/// `%` first so the encoding is injective (two different raw segments can
/// never encode to the same text). `[` and `]` are gen-delims reserved for
/// IPv6 literals in the authority and are ILLEGAL in a path segment, so one
/// raw bracket makes the whole Turtle document unparseable, not just its own
/// triple — yupana measured that as 55 failed promote runs over 12 days
/// (aegis-r5xta), every offender a JavaScript computed method name like
/// `[Symbol.iterator]` in vendored minified bundles. bobbin indexes those
/// same bundles.
pub fn iri_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '"' => out.push_str("%22"),
            '{' => out.push_str("%7B"),
            '}' => out.push_str("%7D"),
            '|' => out.push_str("%7C"),
            '\\' => out.push_str("%5C"),
            '^' => out.push_str("%5E"),
            '`' => out.push_str("%60"),
            '[' => out.push_str("%5B"),
            ']' => out.push_str("%5D"),
            '\n' | '\t' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Encode a relative path into ONE opaque IRI segment.
///
/// yupana's `module_iri`/`document_iri` escape `/` and nothing else. We escape
/// `/` and then apply [`iri_segment`]. For every path without IRI-hostile
/// characters — effectively all of them — this is byte-identical to yupana, so
/// identities merge. For a path containing a space or a bracket, yupana emits an
/// INVALID IRI and its promote fails on the whole document (the aegis-r5xta
/// shape, one level up from symbol names); we emit a valid one. That can only
/// differ from yupana on files yupana cannot currently emit at all, so it costs no
/// merge and avoids inheriting the bug. The divergence is reported upstream
/// rather than silently absorbed.
pub fn path_segment(rel: &str) -> String {
    // Encode FIRST, then insert the separator. The other order double-encodes
    // our own `%2F` into `%252F` (caught by `paths_are_one_opaque_segment`),
    // because `iri_segment` escapes `%` — which it must, to stay injective.
    iri_segment(rel).replace('/', "%2F")
}

/// Percent-encode a path into one opaque IRI segment.
///
/// Retained for bobbin-private lanes (file-coupling, quarantine) whose IRIs
/// merge with nothing in yupana. Prefer [`path_segment`] / [`iri_segment`] for
/// anything on the shared code or doc lanes.
pub fn iri_encode(s: &str) -> String {
    s.replace('%', "%25")
        .replace('/', "%2F")
        .replace(' ', "%20")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('"', "%22")
        .replace('{', "%7B")
        .replace('}', "%7D")
}

/// GitHub-style anchor slug, matching yupana's `docref.rs::slugify`: lowercase,
/// spaces/underscores/dashes collapse to one `-`, other punctuation dropped,
/// leading and trailing dashes trimmed.
pub fn slugify(heading: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true; // leading dashes are trimmed by starting `true`
    for c in heading.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            last_dash = false;
        } else if (c == ' ' || c == '-' || c == '_') && !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// IRI for a code module (one per source file).
pub fn code_module_iri(repo: &str, path: &str) -> String {
    format!("{CODE_BASE}{}/{}", iri_segment(repo), path_segment(path))
}

/// IRI for a symbol: yupana's `{module}::{name}` idiom.
///
/// bobbin has no enclosing-scope chain to insert (see the module docs), so
/// this is the single-segment case of yupana's `{module}::{scope}::{name}`.
pub fn symbol_iri(repo: &str, path: &str, name: &str) -> String {
    format!("{}::{}", code_module_iri(repo, path), iri_segment(name))
}

/// IRI for a document entity (one per markdown/PDF file).
pub fn document_iri(repo: &str, path: &str) -> String {
    format!("{DOC_BASE}{}/{}", iri_segment(repo), path_segment(path))
}

/// IRI for a document section: yupana's `{document}#{slug}` idiom.
///
/// bobbin carries a section's BREADCRUMB in `name` (`Guide > Setup`) while the
/// live lane anchors on the slug of the heading itself, so the leaf is taken
/// here rather than by each caller. Doing it here is the point: `entities.rs`
/// and the quipu emitter must mint identical identities, and when this lived
/// at one call site the two disagreed (`#guide-setup` vs `#setup`) — caught by
/// `entities::tests::derives_modules_symbols_and_sections`.
pub fn section_iri(repo: &str, path: &str, heading: &str) -> String {
    format!(
        "{}#{}",
        document_iri(repo, path),
        slugify(section_heading_leaf(heading))
    )
}

/// The leaf of a `Parent > Child` section breadcrumb.
pub fn section_heading_leaf(heading: &str) -> &str {
    heading.rsplit(" > ").next().unwrap_or(heading)
}

/// IRI for a chunk span, on the chunk lane.
///
/// Keyed by start line because a chunk IS a line range — unlike a symbol,
/// whose identity must survive edits above it, a span that moves is a
/// different span. Reindexing replaces the whole per-repo snapshot, so a
/// shifted chunk retracts and re-asserts rather than accumulating.
pub fn chunk_iri(repo: &str, path: &str, start_line: u32) -> String {
    format!(
        "{CHUNK_BASE}{}/{}#C{start_line}",
        iri_segment(repo),
        path_segment(path)
    )
}

mod parse;
pub use parse::{code_module_prefix_of, entity_iri_file_path};

#[cfg(test)]
mod tests {
    use super::*;

    /// The shapes below are transcribed from yupana `src/export.rs` and from
    /// LIVE IRIs read back off the deployed store on 2026-08-24 — deliberately
    /// as literal strings rather than by calling our own constructors, so this
    /// asserts agreement with the producer instead of self-consistency.
    #[test]
    fn matches_the_live_hank_lane() {
        assert_eq!(
            code_module_iri("quipu", "src/w3c.rs"),
            "http://aegis.gastown.local/ontology/code/quipu/src%2Fw3c.rs"
        );
        assert_eq!(
            symbol_iri("quipu", "src/w3c.rs", "negotiate"),
            "http://aegis.gastown.local/ontology/code/quipu/src%2Fw3c.rs::negotiate"
        );
        assert_eq!(
            code_module_iri("quipu", "tests/no_internal_identifiers.rs"),
            "http://aegis.gastown.local/ontology/code/quipu/tests%2Fno_internal_identifiers.rs"
        );
        assert_eq!(
            symbol_iri("quipu", "build.rs", "main"),
            "http://aegis.gastown.local/ontology/code/quipu/build.rs::main"
        );
        assert_eq!(
            document_iri("quipu", "docs/design/reasoner.md"),
            "http://aegis.gastown.local/ontology/doc/quipu/docs%2Fdesign%2Freasoner.md"
        );
        assert_eq!(
            section_iri("quipu", "README.md", "See it in action"),
            "http://aegis.gastown.local/ontology/doc/quipu/README.md#see-it-in-action"
        );
        assert_eq!(
            section_iri("quipu", "README.md", "REST API / Web UI"),
            "http://aegis.gastown.local/ontology/doc/quipu/README.md#rest-api-web-ui"
        );
        // A breadcrumb anchors on its leaf, and identically from either caller.
        assert_eq!(
            section_iri("quipu", "README.md", "Guide > Setup"),
            section_iri("quipu", "README.md", "Setup")
        );
    }

    /// The superseded lane, named explicitly so a regression to it fails loudly
    /// instead of quietly minting a second code graph (aegis-6noan).
    #[test]
    fn never_mints_the_superseded_ingest_repos_lane() {
        for iri in [
            code_module_iri("quipu", "src/w3c.rs"),
            symbol_iri("quipu", "src/w3c.rs", "negotiate"),
            document_iri("quipu", "README.md"),
            section_iri("quipu", "README.md", "Features"),
            chunk_iri("quipu", "README.md", 7),
        ] {
            assert!(
                !iri.starts_with("http://aegis.gastown.local/code/"),
                "regressed to the superseded ingest-repos.py lane: {iri}"
            );
            assert!(
                !iri.contains("-L"),
                "regressed to the -L{{line}} idiom: {iri}"
            );
            assert!(
                !iri.contains("/S") || iri.contains("%2FS"),
                "regressed to the S{{line}} section idiom: {iri}"
            );
        }
    }

    #[test]
    fn slugify_matches_github_style() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  Multi   Space  "), "multi-space");
        assert_eq!(slugify("snake_case name"), "snake-case-name");
        assert_eq!(slugify("REST API / Web UI"), "rest-api-web-ui");
    }

    /// aegis-r5xta: one raw bracket makes the whole Turtle document
    /// unparseable, not just its own triple.
    #[test]
    fn encodes_the_characters_that_break_a_whole_document() {
        assert_eq!(iri_segment("[Symbol.iterator]"), "%5BSymbol.iterator%5D");
        assert_eq!(iri_segment("Foo<T>"), "Foo%3CT%3E");
        assert_eq!(iri_segment("a|b\\c^d`e"), "a%7Cb%5Cc%5Ed%60e");
        assert_eq!(iri_segment("50% done"), "50%25%20done");
        // `%` first, so encoding is injective.
        assert_ne!(iri_segment("%2F"), iri_segment("/"));
    }

    #[test]
    fn paths_are_one_opaque_segment() {
        assert_eq!(path_segment("src/main.rs"), "src%2Fmain.rs");
        assert_eq!(path_segment("docs/my file.md"), "docs%2Fmy%20file.md");
    }

    #[test]
    fn chunks_are_off_the_code_and_doc_lanes() {
        let c = chunk_iri("q", "README.md", 7);
        assert_eq!(
            c,
            "http://aegis.gastown.local/ontology/chunk/q/README.md#C7"
        );
        assert!(!c.starts_with(CODE_BASE));
        assert!(!c.starts_with(DOC_BASE));
    }
}
