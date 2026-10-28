//! Normal CI guard for single-source primer copies.
use std::path::Path;

#[test]
fn primer_sections_and_generated_surfaces_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let primer = std::fs::read_to_string(root.join("docs/primer.md")).unwrap();
    assert!(primer.starts_with("# Bobbin agent primer\n"));
    let required = [
        "What Bobbin is",
        "Agent workflow",
        "Core commands",
        "MCP Tools",
        "Next steps",
    ];
    let mut previous = 0;
    for heading in required {
        let i = primer.find(&format!("## {heading}\n")).unwrap();
        assert!(i > previous);
        previous = i;
    }
    let path = root.join("skills/bobbin/SKILL.md");
    let skill = std::fs::read_to_string(&path).unwrap();
    let start = skill.find("<!-- BEGIN bobbin prime -->\n").unwrap()
        + "<!-- BEGIN bobbin prime -->\n".len();
    let end = skill.find("<!-- END bobbin prime -->").unwrap();
    let expected = primer
        .lines()
        .map(|l| {
            if l.starts_with('#') {
                format!("##{l}\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect::<String>();
    if std::env::var_os("BOBBIN_BLESS_PRIMER").is_some() {
        std::fs::write(
            path,
            format!("{}{expected}{}", &skill[..start], &skill[end..]),
        )
        .unwrap();
    } else {
        assert_eq!(
            &skill[start..end],
            expected,
            "Run BOBBIN_BLESS_PRIMER=1 just prime test and commit the generated copy"
        );
    }
    let page = std::fs::read_to_string(root.join("docs/book/src/cli/prime.md")).unwrap();
    assert!(page.contains("{{#include ../../../../docs/primer.md}}"));
}
