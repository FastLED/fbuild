//! Versioned reference evidence for the final linked image. Missing edges are
//! not evidence of dead code: indirect dispatch and linker roots are partial.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SymbolIdentity {
    pub name: String,
    pub address: u64,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Disassembly,
    StaticData,
    FragmentOwner,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceEdge {
    pub source: SymbolIdentity,
    pub target: SymbolIdentity,
    pub kind: ReferenceKind,
    pub offset: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisStatus {
    Analyzed,
    #[default]
    Unavailable,
    Error,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalysisPass {
    pub status: AnalysisStatus,
    pub tool: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionRoot {
    pub symbol: SymbolIdentity,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnresolvedReference {
    pub name: String,
    pub address: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceAnalysis {
    pub schema: u32,
    pub disassembly: AnalysisPass,
    pub static_data: AnalysisPass,
    pub object_references: AnalysisPass,
    pub edges: Vec<ReferenceEdge>,
    pub roots: Vec<RetentionRoot>,
    pub unexplained: Vec<SymbolIdentity>,
    pub unresolved: Vec<UnresolvedReference>,
    pub limitations: Vec<String>,
}

impl Default for ReferenceAnalysis {
    fn default() -> Self {
        Self {
            schema: 1,
            disassembly: AnalysisPass::default(),
            static_data: AnalysisPass::default(),
            object_references: AnalysisPass::default(),
            edges: Vec::new(),
            roots: Vec::new(),
            unexplained: Vec::new(),
            unresolved: Vec::new(),
            limitations: vec![
                "Runtime indirect calls are not reconstructed.".into(),
                "Static pointer extraction covers absolute Itanium vtables; relative vtables and function descriptors are unsupported.".into(),
                "KEEP and platform-specific linker roots require additional linker evidence."
                    .into(),
                "Disassembly annotations are references, not necessarily function calls; interior-offset annotations are not reconstructed.".into(),
            ],
        }
    }
}

impl From<&super::FineGrainedSymbol> for SymbolIdentity {
    fn from(symbol: &super::FineGrainedSymbol) -> Self {
        Self {
            name: symbol.mangled.clone(),
            address: symbol.address,
            source: symbol.source.clone(),
        }
    }
}
