//! MCU memory specifications.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McuSpec {
    pub mcu: String,
    pub max_flash: u64,
    pub max_ram: u64,
}
