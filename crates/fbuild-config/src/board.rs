//! Board configuration from JSON board definitions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    pub name: String,
    pub mcu: String,
    pub f_cpu: String,
    pub board: String,
    pub core: String,
    pub variant: String,
}
