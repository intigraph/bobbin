//! Request -> query-parameter mapping for the MCP remote backend.
//!
//! Pure functions on purpose: this is the part of the proxy that can be wrong
//! without anything failing. A dropped or misnamed parameter is ACCEPTED by
//! the server and simply ignored, so the tool returns a plausible answer to a
//! question nobody asked. Keeping the mapping pure lets `params_tests` pin
//! every field of every request without standing up a server.

use crate::mcp::tools::*;

pub(super) struct Params(Vec<(&'static str, String)>);

impl Params {
    pub(super) fn new() -> Self {
        Params(Vec::new())
    }

    pub(super) fn set(mut self, key: &'static str, value: impl ToString) -> Self {
        self.0.push((key, value.to_string()));
        self
    }

    pub(super) fn opt<T: ToString>(mut self, key: &'static str, value: &Option<T>) -> Self {
        if let Some(v) = value {
            self.0.push((key, v.to_string()));
        }
        self
    }

    pub(super) fn opt_str(self, key: &'static str, value: &Option<String>) -> Self {
        self.opt(key, value)
    }

    pub(super) fn into_vec(self) -> Vec<(&'static str, String)> {
        self.0
    }

    #[cfg(test)]
    pub(super) fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    #[cfg(test)]
    pub(super) fn keys(&self) -> Vec<&'static str> {
        self.0.iter().map(|(k, _)| *k).collect()
    }
}

pub(super) fn search_params(req: &SearchRequest) -> Params {
    Params::new()
        .set("q", &req.query)
        .set("mode", req.mode.as_deref().unwrap_or("hybrid"))
        .set("limit", req.limit.unwrap_or(10))
        .opt_str("type", &req.r#type)
        .opt_str("repo", &req.repo)
        .opt_str("tag", &req.tag)
        .opt_str("exclude_tag", &req.exclude_tag)
        .opt_str("bundle", &req.bundle)
}

pub(super) fn grep_params(req: &GrepRequest) -> Params {
    Params::new()
        .set("pattern", &req.pattern)
        .set("limit", req.limit.unwrap_or(10))
        .opt("ignore_case", &req.ignore_case)
        .opt("regex", &req.regex)
        .opt_str("type", &req.r#type)
        .opt_str("repo", &req.repo)
}

pub(super) fn context_params(req: &ContextRequest) -> Params {
    Params::new()
        .set("q", &req.query)
        .opt("budget", &req.budget)
        .opt("depth", &req.depth)
        .opt("max_coupled", &req.max_coupled)
        .opt("limit", &req.limit)
        .opt("coupling_threshold", &req.coupling_threshold)
        .opt_str("repo", &req.repo)
        .opt_str("tag", &req.tag)
        .opt_str("exclude_tag", &req.exclude_tag)
        .opt_str("bundle", &req.bundle)
}

pub(super) fn read_chunk_params(req: &ReadChunkRequest) -> Params {
    Params::new()
        .opt_str("repo", &req.repo)
        .set("file", &req.file)
        .set("start_line", req.start_line)
        .set("end_line", req.end_line)
        .opt("context", &req.context)
}

pub(super) fn related_params(req: &RelatedRequest) -> Params {
    Params::new()
        .set("file", &req.file)
        .set("limit", req.limit.unwrap_or(10))
        .opt("threshold", &req.threshold)
        .opt_str("repo", &req.repo)
}

pub(super) fn find_refs_params(req: &FindRefsRequest) -> Params {
    Params::new()
        .set("symbol", &req.symbol)
        .set("limit", req.limit.unwrap_or(20))
        .opt_str("type", &req.r#type)
        .opt_str("repo", &req.repo)
}

pub(super) fn list_symbols_params(req: &ListSymbolsRequest) -> Params {
    Params::new()
        .set("file", &req.file)
        .opt_str("repo", &req.repo)
}

pub(super) fn dependencies_params(req: &DependenciesRequest) -> Params {
    Params::new()
        .set("file", &req.file)
        .opt("reverse", &req.reverse)
        .opt("both", &req.both)
}

pub(super) fn file_history_params(req: &FileHistoryRequest) -> Params {
    Params::new()
        .set("file", &req.file)
        .set("limit", req.limit.unwrap_or(20))
}

pub(super) fn hotspots_params(req: &HotspotsRequest) -> Params {
    Params::new()
        .set("limit", req.limit.unwrap_or(20))
        .opt_str("since", &req.since)
        .opt("threshold", &req.threshold)
}

pub(super) fn impact_params(req: &ImpactRequest) -> Params {
    Params::new()
        .set("target", &req.target)
        .opt("depth", &req.depth)
        .opt_str("mode", &req.mode)
        .opt("limit", &req.limit)
        .opt("threshold", &req.threshold)
        .opt_str("repo", &req.repo)
}

pub(super) fn similar_params(req: &SimilarRequest) -> Params {
    Params::new()
        .opt_str("target", &req.target)
        .opt("scan", &req.scan)
        .opt("threshold", &req.threshold)
        .opt("limit", &req.limit)
        .opt_str("repo", &req.repo)
        .opt("cross_repo", &req.cross_repo)
}

pub(super) fn search_beads_params(req: &SearchBeadsRequest) -> Params {
    Params::new()
        .set("q", &req.query)
        .set("limit", req.limit.unwrap_or(10))
        .opt("priority", &req.priority)
        .opt_str("status", &req.status)
        .opt_str("assignee", &req.assignee)
        .opt_str("rig", &req.rig)
        .opt_str("issue_type", &req.issue_type)
        .opt_str("label", &req.label)
        .opt("enrich", &req.enrich)
        .opt("compact", &req.compact)
}

// NOTE the rename: the tool calls it `filter`, the endpoint calls it
// `name_filter`. Passing it through under the tool's own name would
// be accepted and ignored, silently widening the result set.
pub(super) fn archive_search_params(req: &ArchiveSearchRequest) -> Params {
    Params::new()
        .set("q", &req.query)
        .set("limit", req.limit.unwrap_or(10))
        .opt_str("mode", &req.mode)
        .opt_str("source", &req.source)
        .opt_str("name_filter", &req.filter)
        .opt_str("after", &req.after)
        .opt_str("before", &req.before)
}

pub(super) fn archive_recent_params(req: &ArchiveRecentRequest) -> Params {
    Params::new()
        .set("limit", req.limit.unwrap_or(20))
        .opt_str("after", &req.after)
        .opt_str("source", &req.source)
}

pub(super) fn prime_params(req: &PrimeRequest) -> Params {
    Params::new()
        .opt_str("section", &req.section)
        .opt("brief", &req.brief)
}

pub(super) fn feedback_list_params(req: &FeedbackListRequest) -> Params {
    Params::new()
        .opt_str("rating", &req.rating)
        .opt_str("agent", &req.agent)
        .opt("limit", &req.limit)
}

pub(super) fn feedback_lineage_list_params(req: &FeedbackLineageListRequest) -> Params {
    Params::new()
        .opt("feedback_id", &req.feedback_id)
        .opt_str("bead", &req.bead)
        .opt_str("commit_hash", &req.commit_hash)
        .opt("limit", &req.limit)
}
