//! CH32V RISC-V platform build support (WCH CH32V003, CH32V203, etc.)

pub mod ch32v_compiler;
pub mod ch32v_linker;
pub mod mcu_config;
pub mod orchestrator;

pub use ch32v_compiler::Ch32vCompiler;
pub use ch32v_linker::Ch32vLinker;
pub use orchestrator::Ch32vOrchestrator;

/// CH32V platform support.
pub struct Ch32vPlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Ch32vPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn install_deps(&self, project_dir: &std::path::Path) -> fbuild_core::Result<()> {
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::RiscvToolchain::new(project_dir);
        Package::ensure_installed(&tc).await?;
        tracing::info!("RISC-V toolchain installed");
        Ok(())
    }

    fn default_board_id(&self) -> &str {
        "genericCH32V003F4P6"
    }
}
