//! Core types for the compile database: entries, container, and target arch enum.

/// A single entry in the compile database.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompileEntry {
    /// The compiler invocation as an argument list (preferred by clangd).
    pub arguments: Vec<String>,
    /// The working directory for the compilation.
    pub directory: String,
    /// The source file being compiled.
    pub file: String,
    /// The output object file (optional per spec).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Container for compile database entries.
pub struct CompileDatabase {
    pub(in crate::compile_database) entries: Vec<CompileEntry>,
}

impl Default for CompileDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl CompileDatabase {
    /// Create an empty compile database.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add an entry to the database.
    pub fn add_entry(&mut self, entry: CompileEntry) {
        self.entries.push(entry);
    }

    /// Add multiple entries to the database.
    pub fn extend(&mut self, entries: Vec<CompileEntry>) {
        self.entries.extend(entries);
    }

    /// Whether the database has any entries.
    pub fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }
}

/// Target architecture for clang flag translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetArchitecture {
    Xtensa,
    Riscv32,
    Avr,
    Arm,
}

impl TargetArchitecture {
    pub fn target_triple(&self) -> &'static str {
        match self {
            Self::Xtensa => "xtensa-esp-elf",
            Self::Riscv32 => "riscv32-esp-elf",
            Self::Avr => "avr",
            Self::Arm => "arm-none-eabi",
        }
    }
}
