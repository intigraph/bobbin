/// Extract brief primer text (title + first section only).
pub(super) fn extract_brief(primer: &str) -> String {
    let mut result = String::new();
    let mut heading_count = 0;
    for line in primer.lines() {
        if line.starts_with("## ") {
            heading_count += 1;
            if heading_count > 1 {
                break;
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    result.trim_end().to_string()
}
