//! Bead source selection for `[beads]`.
use serde::{Deserialize, Serialize};

/// Which store bead content is read from.
///
/// `Dolt` is the original MySQL-protocol backend. `Jsonl` reads the br store's
/// exported `issues.jsonl`, which is what the crew store became at the clbx2
/// cutover (2026-08-29) — see aegis-205llh, where bobbin was found still
/// answering bead search out of the frozen Dolt snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BeadsSource {
    /// Dolt / MySQL protocol (`host`, `port`, `user`, `databases`).
    #[default]
    Dolt,
    /// br JSONL export, one path per configured database (`jsonl_paths`).
    Jsonl,
    /// The live seeds board: items read from its Quipu named graph
    /// (`[beads.seeds]`), refreshed from the graph's `/changes` feed.
    Seeds,
}

/// Where the live seeds board is read from (`[beads.seeds]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SeedsBoard {
    /// Base URL of the Quipu server holding the board (e.g. `http://quipu.svc`).
    /// Empty is an error, never an empty rig.
    pub url: String,
    /// Database name -> board graph IRI. Unset entries default to
    /// `https://seeds.local/project/<rig>`.
    pub graphs: std::collections::BTreeMap<String, String>,
    /// Rows per bounded board query page.
    pub page_size: usize,
}

impl Default for SeedsBoard {
    fn default() -> Self {
        Self {
            url: String::new(),
            graphs: std::collections::BTreeMap::new(),
            page_size: 500,
        }
    }
}
