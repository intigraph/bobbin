//! Terminal rendering shared by the local and remote `bobbin feedback` paths.

use colored::Colorize;

/// One feedback record line: `#id injection[ item] rating agent — reason`.
pub(super) fn print_record(
    id: i64,
    injection_id: &str,
    item: Option<&str>,
    rating: &str,
    agent: &str,
    reason: &str,
) {
    let target = match item {
        Some(it) => format!("{injection_id} {it}"),
        None => injection_id.to_string(),
    };
    let rating = match rating {
        "useful" => rating.green(),
        "noise" => rating.yellow(),
        "harmful" => rating.red(),
        "used" => rating.blue(),
        _ => rating.normal(),
    };
    let reason = if reason.is_empty() {
        String::new()
    } else {
        format!("— {reason}")
    };
    println!(
        "{} {} {} {} {}",
        format!("#{id}").dimmed(),
        target.cyan(),
        rating,
        agent.dimmed(),
        reason
    );
}

/// One cited memory entity's usage line under `bobbin feedback stats`.
pub(super) fn print_entity(kind: &str, injected: u64, used: u64, noise: u64, iri: &str) {
    println!("  memory {kind:<9} inj {injected:>3} used {used:>3} noise {noise:>3}  {iri}");
}
