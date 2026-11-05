//! Parsing the IRIs this module's siblings mint — the inverse direction.
//!
//! Split out of `iri/mod.rs` only for file size; it is deliberately the same
//! module tree as the minters, because the reason this code exists is that a
//! parser living somewhere else drifted onto a lane no minter writes (see
//! [`entity_iri_file_path`]). Keep the round-trip tests here composing the
//! real constructors from `super`.

use super::{CHUNK_BASE, CODE_BASE, DOC_BASE};

/// Decode one percent-encoded IRI segment back to its raw text.
///
/// The exact inverse of [`iri_segment`], and it must stay that way: the encoder
/// escapes `%` FIRST precisely so decoding is a single left-to-right pass over
/// `%XX` escapes with no second round. Decoding by chained `replace` (what the
/// context parser used to do) is not that inverse — it decodes `%252F` to `/`
/// instead of to the literal `%2F` — so this is written as one pass.
///
/// A malformed `%` escape is left verbatim rather than dropped: a path we
/// cannot decode should read wrong, not silently shorten into a different
/// valid path.
pub fn iri_segment_decode(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Recover the repo-relative file path from any file-keyed entity IRI.
///
/// The inverse of [`code_module_iri`] / [`symbol_iri`] / [`document_iri`] /
/// [`section_iri`] / [`chunk_iri`], and it lives here, beside them, so the
/// parser cannot drift onto a lane no minter writes. It previously lived in
/// `search::context` and parsed the SUPERSEDED `http://aegis.gastown.local/code/`
/// base with `{name}-L{line}` / `S{line}` suffixes — a lane with 0 live
/// instances that no producer in this repo mints — so knowledge expansion
/// matched nothing at all while its tests stayed green over the dead lane.
/// `parses_what_the_minters_mint` is the round trip that keeps that from
/// recurring.
///
/// Accepts the three file-keyed lanes and the suffix forms yupana actually
/// mints:
///
/// ```text
/// {CODE_BASE}{repo}/{path}              {CODE_BASE}{repo}/{path}::{symbol}
/// {DOC_BASE}{repo}/{path}               {DOC_BASE}{repo}/{path}#{slug}
/// {CHUNK_BASE}{repo}/{path}#C{line}
/// ```
///
/// Returns `None` for a vocabulary term, for the quarantined inferred lane
/// (which must never be read back as an observed code entity), and for the
/// superseded base.
pub fn entity_iri_file_path(iri: &str) -> Option<String> {
    let rest = [CODE_BASE, DOC_BASE, CHUNK_BASE]
        .iter()
        .find_map(|base| iri.strip_prefix(*base))?;
    let (_repo, path_and_suffix) = rest.split_once('/')?;
    // `::` (symbol) and `#` (section slug, chunk span) are the only suffixes
    // any minter appends, and neither can appear inside an encoded path
    // segment — `/` is `%2F` and every other IRI-hostile character is escaped.
    let encoded_path = path_and_suffix
        .split("::")
        .next()
        .and_then(|s| s.split('#').next())
        .filter(|s| !s.is_empty())?;
    Some(iri_segment_decode(encoded_path))
}

/// The `{module}::` prefix that every symbol defined in the same file as this
/// chunk shares, for same-file narrowing of an ambiguous mention.
///
/// Accepts either lane a chunk can be on: its own `chunk/` span IRI, or the
/// `code/` symbol IRI it took when it was dual-typed. Returns `None` for a
/// document/section chunk — a markdown file defines no code symbols, so
/// "narrow to symbols in this file" has no referent rather than an empty one.
pub fn code_module_prefix_of(chunk_iri: &str) -> Option<String> {
    if let Some(rest) = chunk_iri.strip_prefix(CHUNK_BASE) {
        let span = rest.split('#').next()?;
        return Some(format!("{CODE_BASE}{span}::"));
    }
    if let Some(rest) = chunk_iri.strip_prefix(CODE_BASE) {
        let module = rest.split("::").next()?;
        return Some(format!("{CODE_BASE}{module}::"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::{
        chunk_iri, code_module_iri, document_iri, iri_segment, section_iri, symbol_iri,
        INFERRED_BASE,
    };
    use super::*;

    /// THE ROUND TRIP. `entity_iri_file_path` must accept whatever the minters
    /// in this module emit — that is the property whose absence let the parser
    /// sit on a third, dead lane while its own tests passed. Assert it by
    /// composing the real constructors, never by transcribing strings.
    #[test]
    fn parses_what_the_minters_mint() {
        for path in [
            "src/main.rs",
            "build.rs",
            "src/store/ops.rs",
            "docs/my file.md",
            "tests/no_internal_identifiers.rs",
            "vendor/50% done/[Symbol.iterator].js",
        ] {
            for iri in [
                code_module_iri("quipu", path),
                symbol_iri("quipu", path, "negotiate"),
                symbol_iri("quipu", path, "[Symbol.iterator]"),
                document_iri("quipu", path),
                section_iri("quipu", path, "REST API / Web UI"),
                chunk_iri("quipu", path, 42),
            ] {
                assert_eq!(
                    entity_iri_file_path(&iri).as_deref(),
                    Some(path),
                    "round trip lost the path for {iri}"
                );
            }
        }
    }

    #[test]
    fn does_not_parse_the_superseded_or_quarantined_lanes() {
        // The superseded ingest-repos.py lane, which nothing mints.
        assert_eq!(
            entity_iri_file_path("http://aegis.gastown.local/code/quipu/src%2Fw3c.rs"),
            None
        );
        // A CURIE, which no writer ever produced.
        assert_eq!(entity_iri_file_path("bobbin:code/repo/src/lib.rs"), None);
        // A vocabulary term is not an entity.
        assert_eq!(
            entity_iri_file_path("http://aegis.gastown.local/ontology/CodeModule"),
            None
        );
        // Quarantined model-inferred candidates must never read back as
        // observed code entities.
        assert_eq!(
            entity_iri_file_path(&format!("{INFERRED_BASE}quipu/src%2Fa.rs")),
            None
        );
    }

    #[test]
    fn decode_is_the_inverse_of_encode() {
        for raw in [
            "plain",
            "50% done",
            "%2F",
            "a|b\\c^d`e",
            "[Symbol.iterator]",
        ] {
            assert_eq!(iri_segment_decode(&iri_segment(raw)), raw);
        }
        // A malformed escape survives verbatim rather than shortening the path
        // into a different, valid-looking one.
        assert_eq!(iri_segment_decode("a%zz"), "a%zz");
        assert_eq!(iri_segment_decode("trailing%2"), "trailing%2");
    }

    #[test]
    fn module_prefix_narrows_from_either_lane() {
        let want = "http://aegis.gastown.local/ontology/code/q/src%2Fa.rs::".to_string();
        // an anonymous chunk, on the chunk lane
        assert_eq!(
            code_module_prefix_of(&chunk_iri("q", "src/a.rs", 12)),
            Some(want.clone())
        );
        // a dual-typed chunk, which took the symbol identity
        assert_eq!(
            code_module_prefix_of(&symbol_iri("q", "src/a.rs", "run")),
            Some(want)
        );
        // a doc section defines no code symbols
        assert_eq!(
            code_module_prefix_of(&section_iri("q", "README.md", "Features")),
            None
        );
        // and the prefix must actually match a sibling symbol
        let prefix = code_module_prefix_of(&chunk_iri("q", "src/a.rs", 12)).unwrap();
        assert!(symbol_iri("q", "src/a.rs", "sibling").starts_with(&prefix));
        assert!(!symbol_iri("q", "src/b.rs", "sibling").starts_with(&prefix));
    }
}
