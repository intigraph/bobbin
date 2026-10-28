//! Single-source primer shared by CLI, MCP and HTTP prime.
pub const PRIMER: &str = include_str!("../docs/primer.md");

pub fn sections() -> Vec<&'static str> {
    PRIMER
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .collect()
}

pub fn section(query: &str) -> Result<String, String> {
    let lower = query.trim().to_lowercase();
    let query = match lower.as_str() {
        "commands" | "key commands" => "core commands",
        "what bobbin does" => "what bobbin is",
        s => s,
    };
    let mut out = Vec::new();
    let mut capture = false;
    for line in PRIMER.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if capture {
                break;
            }
            capture = heading.to_lowercase().starts_with(query);
        }
        if capture {
            out.push(line);
        }
    }
    if out.is_empty() {
        Err(format!(
            "Unknown primer section {query:?}. Sections: {}",
            sections().join(", ")
        ))
    } else {
        Ok(format!("{}\n", out.join("\n").trim_end()))
    }
}

/// Keep the existing MCP/HTTP response shape and brief semantics.
pub fn legacy_text(section_name: Option<&str>, brief: bool) -> String {
    if let Some(name) = section_name {
        return section(name).unwrap_or_else(|e| e);
    }
    if brief {
        return format!(
            "# Bobbin agent primer\n\n{}",
            section("What Bobbin is").expect("What section")
        );
    }
    PRIMER.into()
}
