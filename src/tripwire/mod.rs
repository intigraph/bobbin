//! Governed tripwires, surfaced in injected context.
//!
//! A **tripwire** is the governance plane's `Binding / Gate` primitive at its
//! simplest: an `aegis:Policy` at `aegis:boundary "action"` carrying
//! `aegis:appliesTo` path globs and **no** selector or predicate — touching the
//! path *is* the crossing, so the claim needs no evidence beyond the action's
//! own target. Quipu is the canonical store
//! (`shapes/policies/tripwire.ttl`, `docs/book/src/concepts/governance.md`);
//! yupana projects them and enforces the ones it can at its pre-edit guard
//! (`src/project_tripwire.rs`).
//!
//! Bobbin's job is neither of those. Bobbin **tells the agent a wire is there
//! before it walks into one** — the boundary shows up in the same injected
//! bundle as the code it spans, so "I did not know that path was governed" is
//! not available as an excuse or as a surprise.
//!
//! ## Three lines this module will not cross
//!
//! 1. **Bobbin does not enforce.** Every rendered line says what the policy
//!    *declares*, never what bobbin will do. Bobbin has no pre-edit hook and
//!    cannot block a write; a section implying otherwise would be the
//!    armed-looking-inert-control defect the whole tripwire concept exists to
//!    prevent, inverted.
//! 2. **Bobbin never drops a wire it does not understand.** Yupana refuses a
//!    projection carrying an effect it cannot enforce, and it is right to: a
//!    dropped wire there is a boundary that reads as guarded and is not. Here
//!    the failure runs the other way — a dropped wire is a boundary the agent
//!    is never told about. So an unrecognised effect is *surfaced verbatim*
//!    ([`TripEffect::Other`]) and an absent one is surfaced as undeclared. The
//!    full `aegis:effect` vocabulary is `allow | warn | require-approval |
//!    deny | escalate | record` plus the catalog's `throttle`; bobbin reads all
//!    of it because it only has to *name* an effect, not execute it.
//! 3. **Nothing is presented as current that is not.** Every section states
//!    where the facts came from and how old they are. A cached projection says
//!    it is cached and how stale; a failed refresh says the refresh failed
//!    rather than quietly serving yesterday's boundaries as today's.
//!
//! ## Why both injection paths read this directly
//!
//! Bobbin injects context two ways: locally (`bobbin inject-context`, which
//! opens the stores itself) and as a thin client (`--server`, which asks a
//! bobbin server for `/context`). The obvious design puts the tripwires on the
//! `/context` response so the thin client stays thin. This module does not do
//! that, for two reasons:
//!
//! - **The two paths must not disagree.** Routing governance through the
//!   server makes "which boundaries am I told about" depend on which bobbin
//!   deployment answered, which is exactly the divergence that got the sibling
//!   bead (`bobbin-dee`) deferred. Reading the graph from the same place in
//!   both paths makes them identical by construction rather than by review.
//! - **A search server should not be able to suppress a governance boundary.**
//!   The quipu endpoint is an organisation fact, not a bobbin-deployment
//!   detail. Keeping bobbin's own server out of the path means a
//!   misconfigured or compromised index host cannot silently un-govern a repo.
//!
//! The cost is that a thin client needs to reach quipu itself. It already
//! loads local bobbin config for every other hook setting, so this adds a
//! reachability requirement, not a configuration one.
//!
//! ## Configuration
//!
//! None of its own, deliberately. The transport is the existing
//! `quipu_endpoint` key — the same one search spotlight annotations and the
//! MCP ontology tools use — with `BOBBIN_QUIPU_REMOTE` overriding it, matching
//! `src/mcp/server.rs`. A second knob meaning "where is quipu" would let one
//! surface work while another silently did not. **No endpoint configured means
//! no governance plane**: the section is absent and no HTTP call is made, so
//! an ungoverned deployment pays nothing on the hook's hot path.

mod cache;
mod decode;
mod render;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use decode::decode_tripwires;
pub use render::{matching, section, Match};

/// What crossing a wire's boundary triggers, as the policy declares it.
///
/// `Other` and `Undeclared` are not error states here — see the module's rule
/// 2. They are how a surfacing tool stays honest about a vocabulary it does
/// not own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TripEffect {
    /// Advisory: tell the actor, block nothing.
    Warn,
    /// The action is inadmissible; a governing host blocks it before dispatch.
    Deny,
    /// The crossing is priced — an expiring backoff applies to *subsequent*
    /// actions, never the crossing itself.
    Throttle,
    /// Declared, in the `aegis:effect` vocabulary or not, but not one of the
    /// three above. Surfaced by name; bobbin makes no claim about it.
    Other(String),
    /// The policy declared no effect at all. Malformed, and said so.
    Undeclared,
}

impl TripEffect {
    /// Parse the literal quipu serves. Never fails — see rule 2.
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("warn") => Self::Warn,
            Some("deny") => Self::Deny,
            Some("throttle") => Self::Throttle,
            Some("") | None => Self::Undeclared,
            Some(other) => Self::Other(other.to_string()),
        }
    }

    /// The declared name, for rendering.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Warn => "warn",
            Self::Deny => "deny",
            Self::Throttle => "throttle",
            Self::Other(s) => s.as_str(),
            Self::Undeclared => "(none declared)",
        }
    }

    /// Whether a governing host is expected to refuse the action outright.
    /// Advisory to the reader — bobbin refuses nothing.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        matches!(self, Self::Deny)
    }
}

/// One governed tripwire, as projected from quipu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedTripwire {
    /// The policy IRI. Kept because it is the only globally stable handle —
    /// `rdfs:label` is optional and not guaranteed unique.
    pub policy: String,
    /// `rdfs:label`, else the IRI's last segment.
    pub name: String,
    /// `aegis:appliesTo` — repo-relative path globs, accumulated across rows.
    pub paths: Vec<String>,
    /// `aegis:effect`, as declared.
    pub effect: TripEffect,
    /// `aegis:claim` — the policy's own sentence about the compliant
    /// condition. This is the payload an agent actually needs; yupana does not
    /// project it because it composes its own message from the local wire.
    pub claim: Option<String>,
    /// `aegis:constraintClass` — `hard` or `soft`, when declared.
    pub class: Option<String>,
    /// `aegis:verificationPoint` — `PAG` (pre-action gate) or `PAA`
    /// (post-action assessment), when declared.
    pub verification_point: Option<String>,
    /// `aegis:backoffFormula` — required by quipu's placement gate for a
    /// `throttle` effect, so its absence on a throttle wire is a real defect
    /// worth showing rather than hiding.
    pub backoff_formula: Option<String>,
    /// Fields that disagreed across the rows of one policy.
    ///
    /// Yupana refuses the whole projection on a conflict, which is correct for
    /// an enforcement seam: acting on a coin flip is worse than not acting.
    /// Bobbin does not act, so refusing would throw away every *other* wire in
    /// the batch to punish one — and leave the agent told nothing. Instead the
    /// wire survives with its conflict named: the boundary paths accumulate
    /// and are not in conflict, so the agent still learns the boundary exists
    /// and learns not to trust its effect.
    pub conflicts: Vec<String>,
}

impl GovernedTripwire {
    /// Whether this wire's declaration is trustworthy enough to state plainly.
    /// A conflicted, undeclared-effect, or backoff-less throttle wire is not.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.conflicts.is_empty()
            && self.effect != TripEffect::Undeclared
            && !(self.effect == TripEffect::Throttle && self.backoff_formula.is_none())
    }

    /// Why the declaration cannot be stated plainly, if it cannot.
    #[must_use]
    pub fn defect(&self) -> Option<String> {
        if !self.conflicts.is_empty() {
            return Some(format!(
                "conflicting values for {} across its rows — effect not trustworthy",
                self.conflicts.join(", ")
            ));
        }
        if self.effect == TripEffect::Undeclared {
            return Some("declares no aegis:effect".to_string());
        }
        if self.effect == TripEffect::Throttle && self.backoff_formula.is_none() {
            return Some(
                "effect \"throttle\" with no aegis:backoffFormula — quipu's placement gate \
                 refuses this, so it should not exist"
                    .to_string(),
            );
        }
        None
    }
}

/// How this module got the wires it is about to render.
///
/// Rendered verbatim into the section: a reader must be able to tell a live
/// read from a cached one from a failed refresh without knowing the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// Read from quipu just now.
    Live {
        /// The endpoint that answered.
        endpoint: String,
    },
    /// Served from the on-disk projection cache, this many seconds old.
    Cached {
        /// The endpoint the cache was filled from.
        endpoint: String,
        /// Age of the cached projection, in seconds.
        age_secs: u64,
        /// Why the live read was not used: `None` = still inside the TTL.
        refresh_error: Option<String>,
    },
}

impl Provenance {
    /// The one-line source note that rides every rendered section.
    #[must_use]
    pub fn note(&self) -> String {
        match self {
            Self::Live { endpoint } => {
                format!("source: quipu {endpoint}, read live for this turn")
            }
            Self::Cached {
                endpoint,
                age_secs,
                refresh_error: None,
            } => format!(
                "source: quipu {endpoint}, cached projection {} old (within refresh interval)",
                human_age(*age_secs)
            ),
            Self::Cached {
                endpoint,
                age_secs,
                refresh_error: Some(e),
            } => format!(
                "source: quipu {endpoint}, cached projection {} old — REFRESH FAILED ({e}). \
                 These boundaries may have changed since; treat them as last-known, not current.",
                human_age(*age_secs)
            ),
        }
    }
}

/// Render a duration the way a sentence wants it, not the way a clock does.
#[must_use]
pub fn human_age(secs: u64) -> String {
    match secs {
        0..=90 => format!("{secs}s"),
        91..=5400 => format!("{}m", secs / 60),
        5401..=172_800 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

pub use cache::{endpoint, load_tripwires, TRIPWIRE_QUERY};

/// Prepend the governed-boundary section to an already-formatted context block.
///
/// The single entry point both injection paths call, which is the point: one
/// function means the two paths cannot render different governance for the
/// same repo, and a future third path gets it by calling one thing.
///
/// Prepended rather than appended because the boundary is a precondition for
/// reading the code, not a footnote to it — an agent that has already planned
/// an edit by the time it reads "this path is denied" has wasted the turn.
///
/// Infallible by construction: an ungoverned deployment, an unreachable quipu
/// with no cache, or a bundle no wire spans all return the text unchanged.
/// Injection must never fail because governance was unavailable — that would
/// make the governance surface an availability risk for search itself.
pub async fn with_boundaries(
    context_text: String,
    config: &crate::config::Config,
    paths: Vec<String>,
    repo_root: &std::path::Path,
    format_mode: &str,
) -> String {
    let Some((wires, provenance)) = load_tripwires(config, repo_root).await else {
        return context_text;
    };
    let matches = matching(
        &wires,
        &paths,
        config.server.repo_path_prefix.as_deref(),
        repo_root,
    );
    match section(&matches, &provenance, format_mode) {
        Some(s) => format!("{s}{context_text}"),
        None => context_text,
    }
}
