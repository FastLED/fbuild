//! ESP8266 platform build support (NodeMCU, Wemos D1, etc.)

pub mod esp8266_compiler;
pub mod esp8266_linker;
pub mod mcu_config;
pub mod orchestrator;

pub use esp8266_compiler::Esp8266Compiler;
pub use esp8266_linker::Esp8266Linker;
pub use orchestrator::Esp8266Orchestrator;

/// ESP8266 platform support.
pub struct Esp8266PlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Esp8266PlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn install_deps(&self, project_dir: &std::path::Path) -> fbuild_core::Result<()> {
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::Esp8266Toolchain::new(project_dir);
        Package::ensure_installed(&tc).await?;
        tracing::info!("ESP8266 toolchain installed");
        Ok(())
    }

    fn default_board_id(&self) -> &str {
        "nodemcuv2"
    }
}
