---
name: code-review
description: End-of-session code review checking for hardcoded values, core vs platform-specific placement, and bugs in outstanding changes.
allowed-tools: Bash Read Grep Glob Agent
---

# End-of-Session Code Review

You are performing a code review of all changes made during this session. Review the diff of all uncommitted changes and recent commits on this branch.

## How to gather changes

```bash
# Uncommitted changes (staged + unstaged)
git diff HEAD

# If the working tree is clean, review the most recent commits on this branch
git log --oneline -10
git diff HEAD~5..HEAD  # adjust range as needed
```

Review every changed file. For each file, apply all three checks below.

## Check 1: No Hardcoded Values in Rust Source

**Rule**: Configuration values, magic numbers, board-specific data, URLs, paths, pin mappings, clock speeds, memory sizes, and similar constants should live in JSON data files (e.g., `crates/fbuild-config/assets/boards/json/`) or configuration structs — NOT as literals scattered through `.rs` source code.

**What to flag**:
- String literals that look like board names, URLs, file paths, or hardware identifiers
- Numeric literals that represent hardware-specific values (clock speeds, baud rates, memory addresses, pin numbers, flash sizes) unless they are in an obvious constant or enum
- Repeated magic values that should be a named constant or pulled from config

**What is OK**:
- Constants in `const` declarations with descriptive names
- Default values in `Default` impls or builder patterns
- Test fixtures and test data
- Log messages, error messages, format strings
- Standard protocol values (e.g., `0xFF` for padding, `115200` as a default baud in a fallback)

## Check 2: Code Belongs in Core, Not Platform-Specific Crates

**Rule**: Logic that is not specific to a particular hardware platform should live in `fbuild-core` (or another shared crate), not in platform-specific crates like `fbuild-build`, `fbuild-deploy`, or `fbuild-serial`.

**What to flag**:
- Utility functions in platform crates that have no platform-specific dependencies
- Data structures or parsing logic that could be reused across platforms
- Error types or trait definitions duplicated across crates
- Anything in a platform crate that doesn't import or reference platform-specific APIs

**What is OK**:
- Platform-specific implementations of shared traits
- Glue code that wires core logic to platform APIs
- Platform-specific error variants or configuration

## Check 3: Bug Scan

**Rule**: Look for common bugs and correctness issues in all changed code.

**What to flag**:
- Off-by-one errors in loops or slicing
- Missing error handling (unwrap in non-test code, ignored Results)
- Resource leaks (files, connections, locks not released)
- Race conditions or missing synchronization
- Logic errors (wrong operator, inverted condition, unreachable branches)
- Incorrect lifetime or ownership patterns
- Panicking code paths in library code (non-CLI)
- Security issues (path traversal, injection, unchecked input at boundaries)

## Output Format

For each issue found, report:

```
### [CHECK_NAME] file_path:line_number
**Severity**: high | medium | low
**Issue**: One-line description
**Suggestion**: How to fix it
```

If a check finds nothing, say so briefly. End with a summary count:
- Hardcoded values: N issues
- Core placement: N issues
- Bugs: N issues

If there are zero issues across all checks, just say "Code review passed - no issues found."
