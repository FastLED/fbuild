//! Ordered PlatformIO source filters shared by source consumers.
use std::path::Path;
use regex::Regex;

pub fn normalize_glob_separators(pattern: &str) -> String {
    // Glob-pattern normalization is INTENTIONALLY unconditional
    // (unlike `NormalizedPath::display_slash()` which gates on
    // `fbuild_core::platform::host::is_windows()`) — glob patterns come from `platformio.ini` and
    // may contain a mix of `\` and `/` regardless of host OS.
    pattern.replace('\\', "/")
}

#[derive(Debug)]
pub struct SourceFilter {
    rules: Vec<SourceFilterRule>,
    has_include_rules: bool,
}

#[derive(Debug)]
struct SourceFilterRule {
    include: bool,
    matcher: Regex,
}

impl SourceFilter {
    pub fn parse(spec: Option<&str>) -> fbuild_core::Result<Self> {
        let mut rules = Vec::new();
        let mut has_include_rules = false;

        let Some(spec) = spec else {
            return Ok(Self {
                rules,
                has_include_rules,
            });
        };

        for raw in spec.lines().flat_map(|line| line.split(',')) {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }

            let (include, inner) = if token.starts_with("+<") && token.ends_with('>') {
                (true, &token[2..token.len() - 1])
            } else if token.starts_with("-<") && token.ends_with('>') {
                (false, &token[2..token.len() - 1])
            } else {
                return Err(fbuild_core::FbuildError::ConfigError(format!(
                    "invalid source filter rule '{}': expected +/-<pattern>",
                    token
                )));
            };

            let pattern = normalize_glob_separators(inner.trim());
            if pattern.is_empty() {
                return Err(fbuild_core::FbuildError::ConfigError(
                    "source filter rule must not be empty".to_string(),
                ));
            }

            if include {
                has_include_rules = true;
            }

            rules.push(SourceFilterRule {
                include,
                matcher: compile_source_filter_pattern(&pattern)?,
            });
        }

        Ok(Self {
            rules,
            has_include_rules,
        })
    }

    pub fn matches(&self, root: &Path, path: &Path) -> bool {
        if self.rules.is_empty() {
            return true;
        }

        let rel =
            normalize_glob_separators(&path.strip_prefix(root).unwrap_or(path).to_string_lossy());

        let mut included = !self.has_include_rules;
        for rule in &self.rules {
            if rule.matcher.is_match(&rel) {
                included = rule.include;
            }
        }
        included
    }
}

fn compile_source_filter_pattern(pattern: &str) -> fbuild_core::Result<Regex> {
    let normalized = normalize_glob_separators(pattern);
    let regex_body = if normalized == "*" {
        String::from(".*")
    } else if normalized.ends_with('/') {
        format!(
            "{}(?:/.*)?",
            glob_fragment_to_regex(normalized.trim_end_matches('/'))
        )
    } else if normalized.contains('/') {
        glob_fragment_to_regex(&normalized)
    } else {
        format!("(?:.*/)?{}", glob_fragment_to_regex(&normalized))
    };

    Regex::new(&format!("^{}$", regex_body)).map_err(|e| {
        fbuild_core::FbuildError::ConfigError(format!(
            "invalid source filter pattern '{}': {}",
            normalized, e
        ))
    })
}

fn glob_fragment_to_regex(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    out.push_str(".*");
                    i += 1;
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            c if ".+()[]{}^$|\\".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}
