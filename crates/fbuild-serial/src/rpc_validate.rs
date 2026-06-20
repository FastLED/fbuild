//! Host-side JSON-RPC schema fetch + validate component.
//!
//! FastLED/fbuild#698. Today the FastLED autoresearch validator
//! calls `rpc.discover` on the device, parses the response via
//! pydantic, and fails with a cryptic `2 validation errors for
//! RpcSchema: jsonrpc required, methods required` when the device's
//! schema shape differs from the harness's expectation — the user
//! sees "validation failed" but not "what's missing."
//!
//! This module centralizes the validator with a typed error surface
//! so failures spell out the actual problem: `Expected method 'echo'
//! bound — NOT FOUND in device schema. Device returned 7 methods
//! total; harness expects 2.`
//!
//! ## Module layout
//!
//! - [`RpcSchema`] / [`MethodDecl`] — the typed shape of the
//!   `rpc.discover` response.
//! - [`parse_schema_response`] — JSON-RPC envelope parser. Surfaces
//!   the typed structure or a [`SchemaParseError`] saying exactly
//!   which field was missing / wrong-shaped.
//! - [`validate_against_expected`] — produces a structured
//!   [`ValidationReport`] with per-method status.
//! - [`ValidationReport`] / [`MethodStatus`] — the report shape.
//!   `Display` renders the diagnostic the user sees.
//!
//! `fetch_schema` (the actual serial round-trip) is intentionally
//! NOT in this module — that lives in the bring-up orchestrator
//! (FastLED/fbuild#697) where the serial connection is already
//! established. This module is the pure host-side validator,
//! testable from JSON strings without a port.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The shape of `rpc.discover`'s `result` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcSchema {
    /// JSON-RPC protocol version. Always `"2.0"` for the schemas we
    /// care about; surfaced so a mismatch is observable.
    pub jsonrpc: String,
    /// Every method the device advertises as bound.
    pub methods: Vec<MethodDecl>,
}

/// One method declaration in the device's schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodDecl {
    pub name: String,
    /// Parameter types — left as opaque strings (`"int"`, `"string"`,
    /// `"int[]"`) because device firmware emits them as such, and
    /// the harness compares by string.
    #[serde(default)]
    pub params: Vec<String>,
    /// Return type — same string-opaque convention.
    #[serde(default)]
    pub returns: String,
}

/// Typed error from parsing a `rpc.discover` response.
#[derive(Debug, thiserror::Error)]
pub enum SchemaParseError {
    /// The JSON envelope didn't deserialize.
    #[error("invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    /// The envelope was valid JSON but missing the top-level
    /// `jsonrpc` field. Mirror of FastLED's pydantic error message
    /// shape (FastLED/FastLED#3339).
    #[error("JSON-RPC envelope missing required field `jsonrpc`")]
    MissingJsonRpc,
    /// The envelope was valid JSON but missing the `result.methods`
    /// array. Same shape mirror.
    #[error("JSON-RPC envelope missing required field `methods`")]
    MissingMethods,
    /// `rpc.discover` returned a JSON-RPC error response instead of
    /// a success result.
    #[error("device returned JSON-RPC error code {code}: {message}")]
    DeviceError { code: i32, message: String },
}

/// Parse a `rpc.discover` JSON-RPC response (envelope) into the
/// typed [`RpcSchema`].
///
/// Accepts the canonical JSON-RPC 2.0 envelope shape:
///
/// ```json
/// {
///   "jsonrpc": "2.0",
///   "id": 1,
///   "result": {
///     "jsonrpc": "2.0",
///     "methods": [...]
///   }
/// }
/// ```
///
/// OR the response-error shape:
///
/// ```json
/// { "jsonrpc": "2.0", "id": 1, "error": { "code": -32601, "message": "..." } }
/// ```
///
/// The latter is surfaced as [`SchemaParseError::DeviceError`] so
/// the caller can distinguish "device returned an error" from "the
/// response shape was wrong."
pub fn parse_schema_response(json: &str) -> Result<RpcSchema, SchemaParseError> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let obj = value.as_object().ok_or(SchemaParseError::MissingJsonRpc)?;

    // Distinguish error response from result response BEFORE
    // checking for the schema fields — a device error is a more
    // useful diagnostic than "missing methods."
    if let Some(err) = obj.get("error") {
        let code = err
            .get("code")
            .and_then(|c| c.as_i64())
            .map(|v| v as i32)
            .unwrap_or(0);
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("(no message)")
            .to_string();
        return Err(SchemaParseError::DeviceError { code, message });
    }

    if !obj.contains_key("jsonrpc") {
        return Err(SchemaParseError::MissingJsonRpc);
    }

    // The schema itself lives under `result`. Some firmwares return
    // the schema at the top level too (no JSON-RPC envelope); accept
    // both shapes.
    let schema_value = obj.get("result").unwrap_or(&value);
    let schema_obj = schema_value
        .as_object()
        .ok_or(SchemaParseError::MissingMethods)?;
    if !schema_obj.contains_key("methods") {
        return Err(SchemaParseError::MissingMethods);
    }

    let schema: RpcSchema = serde_json::from_value(schema_value.clone())?;
    Ok(schema)
}

/// Per-method status in the [`ValidationReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodStatus {
    /// Expected method is bound on the device. Carries the device's
    /// signature so the user can spot drift.
    Present {
        params: Vec<String>,
        returns: String,
    },
    /// Expected method is NOT in the device schema. The smoking-gun
    /// failure FastLED/FastLED#3339 didn't surface today.
    Missing,
}

/// Structured report from
/// [`validate_against_expected`]. `Display` renders the diagnostic
/// the user sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    /// Per-expected-method status. Iteration order matches the
    /// caller's `expected_methods` slice (BTreeMap because the keys
    /// have to be sorted somewhere; if iteration order matters the
    /// caller can re-sort).
    pub per_method: BTreeMap<String, MethodStatus>,
    /// Total number of methods the device advertised.
    pub device_method_count: usize,
    /// Number of expected methods the caller passed in.
    pub expected_method_count: usize,
}

impl ValidationReport {
    /// `true` iff every expected method was found bound on the
    /// device.
    pub fn is_passing(&self) -> bool {
        self.per_method
            .values()
            .all(|s| matches!(s, MethodStatus::Present { .. }))
    }
}

impl std::fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_passing() {
            writeln!(
                f,
                "✓ Device schema validation passed ({}/{} expected methods bound)",
                self.expected_method_count, self.expected_method_count
            )?;
        } else {
            writeln!(f, "⚠️ Device schema mismatch:")?;
        }
        for (name, status) in &self.per_method {
            match status {
                MethodStatus::Present { params, returns } => {
                    let params_str = params.join(", ");
                    writeln!(
                        f,
                        "    Expected method '{name}' bound — present (signature: {returns}({params_str}))"
                    )?;
                }
                MethodStatus::Missing => {
                    writeln!(
                        f,
                        "    Expected method '{name}' bound — NOT FOUND in device schema."
                    )?;
                }
            }
        }
        writeln!(
            f,
            "    Device returned {} methods total; harness expects {}.",
            self.device_method_count, self.expected_method_count
        )?;
        Ok(())
    }
}

/// Walk `expected_methods` against the device's schema and build a
/// structured [`ValidationReport`].
///
/// **Signature mismatch is not flagged** here — the report surfaces
/// the device's signature so the human can spot drift, but
/// auto-failing on signature mismatch would over-fire on common
/// shape differences (`int` vs `int32_t`, `string` vs `String`).
/// Strict signature checking is a future enhancement.
pub fn validate_against_expected(
    actual: &RpcSchema,
    expected_methods: &[&str],
) -> ValidationReport {
    let mut per_method: BTreeMap<String, MethodStatus> = BTreeMap::new();
    for expected in expected_methods {
        let status = actual
            .methods
            .iter()
            .find(|m| m.name == *expected)
            .map(|m| MethodStatus::Present {
                params: m.params.clone(),
                returns: m.returns.clone(),
            })
            .unwrap_or(MethodStatus::Missing);
        per_method.insert((*expected).to_string(), status);
    }
    ValidationReport {
        per_method,
        device_method_count: actual.methods.len(),
        expected_method_count: expected_methods.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_schema_json() -> &'static str {
        r#"
        {
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "jsonrpc": "2.0",
                "methods": [
                    {"name": "echo", "params": ["int"], "returns": "int"},
                    {"name": "pinToggleRx",
                     "params": ["int", "int", "int", "int"],
                     "returns": "string"}
                ]
            }
        }
        "#
    }

    // ─── parse_schema_response ─────────────────────────────────────

    #[test]
    fn parse_canonical_envelope_succeeds() {
        let schema = parse_schema_response(sample_schema_json()).unwrap();
        assert_eq!(schema.jsonrpc, "2.0");
        assert_eq!(schema.methods.len(), 2);
        assert_eq!(schema.methods[0].name, "echo");
        assert_eq!(schema.methods[1].params, vec!["int", "int", "int", "int"]);
    }

    #[test]
    fn parse_accepts_bare_schema_without_envelope() {
        // Some firmwares return the schema body without the
        // JSON-RPC envelope (no `result` wrapper).
        let bare = r#"{
            "jsonrpc": "2.0",
            "methods": [{"name": "echo", "params": ["int"], "returns": "int"}]
        }"#;
        let schema = parse_schema_response(bare).unwrap();
        assert_eq!(schema.methods.len(), 1);
    }

    #[test]
    fn parse_missing_jsonrpc_surfaces_typed_error() {
        // FastLED's pydantic error: `jsonrpc required`. Same shape
        // surfaced here.
        let no_jsonrpc = r#"{"id": 1, "result": {"methods": []}}"#;
        match parse_schema_response(no_jsonrpc) {
            Err(SchemaParseError::MissingJsonRpc) => {}
            other => panic!("expected MissingJsonRpc, got {other:?}"),
        }
    }

    #[test]
    fn parse_missing_methods_surfaces_typed_error() {
        // FastLED's pydantic error: `methods required`. Same shape.
        let no_methods = r#"{"jsonrpc": "2.0", "result": {"jsonrpc": "2.0"}}"#;
        match parse_schema_response(no_methods) {
            Err(SchemaParseError::MissingMethods) => {}
            other => panic!("expected MissingMethods, got {other:?}"),
        }
    }

    #[test]
    fn parse_device_error_response_surfaces_typed_error() {
        let err_response = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32601, "message": "Method not found: rpc.discover"}
        }"#;
        match parse_schema_response(err_response) {
            Err(SchemaParseError::DeviceError { code, message }) => {
                assert_eq!(code, -32601);
                assert!(message.contains("rpc.discover"));
            }
            other => panic!("expected DeviceError, got {other:?}"),
        }
    }

    #[test]
    fn parse_malformed_json_surfaces_typed_error() {
        let bad = r#"{not even json"#;
        match parse_schema_response(bad) {
            Err(SchemaParseError::InvalidJson(_)) => {}
            other => panic!("expected InvalidJson, got {other:?}"),
        }
    }

    // ─── validate_against_expected ─────────────────────────────────

    #[test]
    fn validate_all_present_reports_passing() {
        let schema = parse_schema_response(sample_schema_json()).unwrap();
        let report = validate_against_expected(&schema, &["echo", "pinToggleRx"]);
        assert!(report.is_passing());
        assert_eq!(report.expected_method_count, 2);
        assert_eq!(report.device_method_count, 2);
    }

    #[test]
    fn validate_missing_method_reports_failing() {
        let schema = parse_schema_response(sample_schema_json()).unwrap();
        let report = validate_against_expected(&schema, &["echo", "nonexistent_method"]);
        assert!(!report.is_passing());
        match report.per_method.get("nonexistent_method").unwrap() {
            MethodStatus::Missing => {}
            other => panic!("expected Missing, got {other:?}"),
        }
        match report.per_method.get("echo").unwrap() {
            MethodStatus::Present { returns, params } => {
                assert_eq!(returns, "int");
                assert_eq!(params, &vec!["int".to_string()]);
            }
            other => panic!("expected Present, got {other:?}"),
        }
    }

    #[test]
    fn validate_surfaces_device_method_count() {
        // Extra device methods are not a failure (a device with more
        // capabilities than the harness expects is fine), but the
        // report MUST surface the count so the user can spot drift.
        let schema_with_extras = r#"{
            "jsonrpc": "2.0",
            "methods": [
                {"name": "echo", "params": ["int"], "returns": "int"},
                {"name": "extra1", "params": [], "returns": "void"},
                {"name": "extra2", "params": [], "returns": "void"}
            ]
        }"#;
        let schema = parse_schema_response(schema_with_extras).unwrap();
        let report = validate_against_expected(&schema, &["echo"]);
        assert!(report.is_passing());
        assert_eq!(report.device_method_count, 3);
        assert_eq!(report.expected_method_count, 1);
    }

    #[test]
    fn display_renders_actionable_diagnostic_for_missing() {
        let schema = parse_schema_response(sample_schema_json()).unwrap();
        let report = validate_against_expected(&schema, &["echo", "ghost"]);
        let rendered = report.to_string();
        // The diagnostic mirrors the issue's worked example.
        assert!(rendered.contains("Device schema mismatch"));
        assert!(rendered.contains("'echo' bound — present"));
        assert!(rendered.contains("'ghost' bound — NOT FOUND"));
        assert!(rendered.contains("Device returned 2 methods total"));
        assert!(rendered.contains("harness expects 2"));
    }

    #[test]
    fn display_renders_pass_for_all_present() {
        let schema = parse_schema_response(sample_schema_json()).unwrap();
        let report = validate_against_expected(&schema, &["echo"]);
        let rendered = report.to_string();
        assert!(rendered.contains("validation passed"));
    }

    #[test]
    fn empty_expected_list_passes_trivially() {
        let schema = parse_schema_response(sample_schema_json()).unwrap();
        let report = validate_against_expected(&schema, &[]);
        assert!(report.is_passing());
        assert_eq!(report.expected_method_count, 0);
    }
}
