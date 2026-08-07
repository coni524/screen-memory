//! Exclusion rules matched against the app name and window title.

use std::collections::HashSet;

use anyhow::{Context, Result};
use regex::RegexBuilder;

pub struct ExcludeRules {
    apps: HashSet<String>,
    patterns: Vec<regex::Regex>,
}

impl ExcludeRules {
    pub fn new(apps: &[String], title_patterns: &[String]) -> Result<Self> {
        let patterns = title_patterns
            .iter()
            .map(|p| {
                RegexBuilder::new(p)
                    .case_insensitive(true)
                    .build()
                    .with_context(|| format!("a regex in exclude_title_patterns is broken: {p:?}"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            apps: apps.iter().cloned().collect(),
            patterns,
        })
    }

    pub fn matches(&self, app: &str, title: &str) -> bool {
        self.apps.contains(app) || self.patterns.iter().any(|p| p.is_match(title))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_exact_match() {
        let rules = ExcludeRules::new(&["1Password".into()], &[]).unwrap();
        assert!(rules.matches("1Password", ""));
        assert!(!rules.matches("1Password 7", ""));
    }

    #[test]
    fn title_regex_case_insensitive() {
        let rules = ExcludeRules::new(&[], &["secret".into(), r"給与.*明細".into()]).unwrap();
        assert!(rules.matches("Safari", "My SECRET page"));
        assert!(rules.matches("Safari", "2026年給与のお支払明細"));
        assert!(!rules.matches("Safari", "ふつうのページ"));
    }

    #[test]
    fn broken_regex_is_error() {
        assert!(ExcludeRules::new(&[], &["(".into()]).is_err());
    }
}
