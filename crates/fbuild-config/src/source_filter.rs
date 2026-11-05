//! Ordered PlatformIO source filters shared by sketches and library manifests.
use fbuild_core::path::NormalizedPath;
use regex::Regex;
use std::path::Path;

/// Normalize lexical glob separators regardless of the build host.
/// Glob strings may contain Windows separators even when evaluated on Linux.
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

        for raw in source_filter_rules(spec)? {
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

        let path = NormalizedPath::new(path);
        let root = NormalizedPath::new(root);
        let relative = path.relative_to(&root).unwrap_or(path);
        let rel = normalize_glob_separators(&relative.to_string_lossy());

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

// Preserve rule order while allowing the documented whitespace-separated string
// form as well as newline/comma-separated project filters. Spaces inside a
// <pattern> belong to that pattern.
fn source_filter_rules(spec: &str) -> fbuild_core::Result<Vec<&str>> {
    let matcher = Regex::new(r"[+-]<[^>]*>").expect("constant source-filter grammar");
    let mut rules = Vec::new();
    let mut end = 0;
    for token in matcher.find_iter(spec) {
        if !spec[end..token.start()]
            .trim_matches(|c: char| c.is_whitespace() || c == ',')
            .is_empty()
        {
            return Err(fbuild_core::FbuildError::ConfigError(
                "invalid source filter rule".into(),
            ));
        }
        rules.push(token.as_str());
        end = token.end();
    }
    if !spec[end..]
        .trim_matches(|c: char| c.is_whitespace() || c == ',')
        .is_empty()
    {
        return Err(fbuild_core::FbuildError::ConfigError(
            "invalid source filter rule".into(),
        ));
    }
    Ok(rules)
}
