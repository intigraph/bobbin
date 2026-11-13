//! Prompt retrieval preferences, independent of repository access controls.
//! An explicit empty scope means abstain, never an unrestricted search.

use anyhow::{bail, Result};
use std::collections::BTreeSet;

use crate::config::Config;

#[derive(Debug, PartialEq, Eq)]
pub struct Scope(pub Option<BTreeSet<String>>);

impl Scope {
    pub fn resolve(config: &Config, role: &str, task_groups: Option<&[String]>) -> Result<Self> {
        for pattern in config.hooks.roles.keys() {
            glob::Pattern::new(pattern)?;
        }
        let matched = config
            .hooks
            .roles
            .iter()
            .filter(|(pattern, _)| glob::Pattern::new(pattern).is_ok_and(|p| p.matches(role)))
            .max_by_key(|(pattern, _)| {
                (
                    pattern
                        .chars()
                        .filter(|c| !matches!(*c, '*' | '?' | '[' | ']'))
                        .count(),
                    pattern.len(),
                )
            });
        let defaults: Vec<String> = config.hooks.search_group.iter().cloned().collect();
        let groups = task_groups
            .or_else(|| matched.map(|(_, groups)| groups.as_slice()))
            .or_else(|| (!defaults.is_empty()).then_some(defaults.as_slice()));
        let Some(groups) = groups else {
            return Ok(Self(None));
        };
        let mut repos = BTreeSet::new();
        for group in groups {
            let Some(members) = config.resolve_group(group) else {
                bail!("unknown prompt retrieval group: {group}");
            };
            for repo in members {
                if repo.is_empty()
                    || !repo
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
                {
                    bail!("invalid repository in prompt retrieval group");
                }
                repos.insert(repo.clone());
            }
        }
        Ok(Self(Some(repos)))
    }

    /// Keyword hints narrow an explicit group; they cannot widen it.
    pub fn narrow_keywords(&mut self, keywords: &[String]) {
        if keywords.is_empty() {
            return;
        }
        let keywords: BTreeSet<String> = keywords.iter().cloned().collect();
        self.0 = Some(match self.0.take() {
            Some(repos) => repos.intersection(&keywords).cloned().collect(),
            None => keywords,
        });
    }

    pub fn empty(&self) -> bool {
        self.0.as_ref().is_some_and(BTreeSet::is_empty)
    }
    pub fn active(&self) -> bool {
        self.0.is_some()
    }
    pub fn allows(&self, repo: Option<&str>) -> bool {
        self.0
            .as_ref()
            .is_none_or(|repos| repo.is_some_and(|r| repos.contains(r)))
    }
    pub fn csv(&self) -> Option<String> {
        self.0
            .as_ref()
            .map(|repos| repos.iter().cloned().collect::<Vec<_>>().join(","))
    }
    pub fn sql(&self) -> Option<String> {
        self.0.as_ref().map(|repos| {
            if repos.is_empty() {
                return "1 = 0".to_string();
            }
            format!(
                "repo IN ({})",
                repos
                    .iter()
                    .map(|r| format!("'{}'", r.replace('\'', "''")))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GroupConfig;

    fn config() -> Config {
        let mut config = Config::default();
        config.groups = vec![
            GroupConfig {
                name: "team".into(),
                repos: vec!["engine".into()],
            },
            GroupConfig {
                name: "external".into(),
                repos: vec!["example".into()],
            },
        ];
        config.hooks.search_group = Some("team".into());
        config
            .hooks
            .roles
            .insert("crew/*".into(), vec!["team".into()]);
        config
            .hooks
            .roles
            .insert("crew/reviewer".into(), vec!["external".into()]);
        config
    }

    #[test]
    fn role_specificity_and_task_override_preserve_positive_controls() {
        let c = config();
        assert!(Scope::resolve(&c, "crew/worker", None)
            .unwrap()
            .allows(Some("engine")));
        let reviewer = Scope::resolve(&c, "crew/reviewer", None).unwrap();
        assert!(reviewer.allows(Some("example")));
        assert!(!reviewer.allows(Some("engine")));
        assert!(Scope::resolve(&c, "crew/reviewer", Some(&["team".into()]))
            .unwrap()
            .allows(Some("engine")));
    }

    #[test]
    fn unknown_empty_and_keyword_intersection_never_expand_scope() {
        let c = config();
        assert!(Scope::resolve(&c, "default", Some(&["missing".into()])).is_err());
        assert!(Scope::resolve(&c, "default", Some(&[])).unwrap().empty());
        let mut scope = Scope::resolve(&c, "default", None).unwrap();
        scope.narrow_keywords(&["example".into()]);
        assert!(scope.empty());
        assert_eq!(scope.sql().as_deref(), Some("1 = 0"));
        assert!(!scope.allows(None));
        assert!(!scope.allows(Some("example")));
    }

    #[test]
    fn unconfigured_preserves_legacy_retrieval() {
        let scope = Scope::resolve(&Config::default(), "default", None).unwrap();
        assert!(!scope.active());
        assert!(scope.allows(None));
        assert!(scope.sql().is_none());
    }
}
