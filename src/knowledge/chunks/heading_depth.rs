//! The Section heading level shared with Yupana (aegis-dxq).

/// `bobbin:headingDepth` is the markdown heading LEVEL (`#` = 1, `##` = 2),
/// per the shared SectionShape. Yupana asserts the same maxCount-1 property on
/// the same Section IRI from the ATX `#` count, so a breadcrumb length breaks
/// whenever a document has no H1 or skips a level: post-state holds two values
/// and every promote of that document is refused (aegis-dxq). A section chunk
/// begins at its heading line, so read the ATX level there; Yupana emits no
/// Section for any other heading form, so the breadcrumb length stays as the
/// fallback for those and cannot collide.
pub(super) fn section_heading_depth(content: &str, heading: &str) -> usize {
    section_heading_prefix(content)
        .map(|prefix| prefix.bytes().filter(|byte| *byte == b'#').count())
        .unwrap_or_else(|| heading.split(" > ").count())
}

/// Retain only observed ATX syntax for content-free graph projection rows.
/// At most three spaces and six hashes survive; no title or body text does.
pub(crate) fn section_heading_prefix(content: &str) -> Option<&str> {
    let line = content.lines().find(|l| !l.trim().is_empty())?;
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    let after = &rest[hashes..];
    (indent <= 3
        && (1..=6).contains(&hashes)
        && (after.is_empty() || after.starts_with(' ') || after.starts_with('\t')))
    .then_some(&line[..indent + hashes])
}

#[cfg(test)]
mod tests {
    use super::section_heading_depth;

    #[test]
    fn heading_depth_is_the_atx_level_not_the_breadcrumb_length() {
        // aegis-dxq: quipu's README has no H1, so "## Before you start" has a
        // one-segment breadcrumb. Yupana says 2 for the same IRI; a breadcrumb
        // count said 1 and post-state refused every promote of the README.
        let mut no_h1 = super::super::tests::chunk("a", "README.md", 110, Some("Before you start"));
        no_h1.content = "## Before you start\n\nRead this first.".into();
        let mut skipped = super::super::tests::chunk("b", "README.md", 120, Some("Top > Deep"));
        skipped.content = "   ### Deep\nbody".into();
        let turtle = super::super::generate_chunk_turtle(&[no_h1, skipped], &[], "repo");
        let block = |anchor: &str| {
            let start = turtle.find(&format!("README.md#{anchor}> a")).unwrap();
            turtle[start..].split("\n\n").next().unwrap().to_string()
        };
        assert!(block("before-you-start").contains("bobbin:headingDepth \"2\"^^xsd:integer"));
        assert!(block("deep").contains("bobbin:headingDepth \"3\"^^xsd:integer"));

        // Not an ATX heading (no space, 7 hashes, 4-space indent, setext):
        // Yupana emits no Section for these, so the breadcrumb fallback stays.
        assert_eq!(section_heading_depth("#hashtag", "A > B"), 2);
        assert_eq!(section_heading_depth("####### seven", "A"), 1);
        assert_eq!(section_heading_depth("    ## code", "A > B > C"), 3);
        assert_eq!(section_heading_depth("Title\n=====", "Title"), 1);
        assert_eq!(section_heading_depth("\n\n#### Four", "X"), 4);
        assert_eq!(section_heading_depth("#", "X > Y"), 1);
    }
}
