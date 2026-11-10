//! Arduino Mbed bundled-library selection and effective library compile inputs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_core::Result;
use fbuild_packages::library::FrameworkLibrary;

use crate::BuildParams;
use crate::compiler::Compiler;
use crate::flag_overlay::LanguageExtraFlags;
use crate::pipeline;

pub(super) struct Bundled {
    pub sources: Vec<PathBuf>,
    pub includes: Vec<PathBuf>,
    pub archives: Vec<PathBuf>,
    pub external: Vec<String>,
}

pub(super) fn bundled(
    project: &Path,
    src: &Path,
    libraries: &[FrameworkLibrary],
    defines: &HashMap<String, String>,
    declared: &[String],
    archive_override: Option<bool>,
) -> Result<Bundled> {
    let selected = crate::framework_libs::resolve_framework_library_selection_active_declared(
        libraries, project, src, defines, declared,
    )?;
    let archives = pipeline::archive_eligible_library_sources(
        libraries,
        &selected.source_files,
        archive_override,
    )?;
    Ok(Bundled {
        sources: selected.source_files,
        includes: selected.include_dirs,
        archives,
        external: fbuild_library_select::external_declared_deps(declared, libraries),
    })
}

fn effective_flags(
    base: Vec<String>,
    overlay: &LanguageExtraFlags,
    unflags: &[String],
    source: &Path,
) -> Vec<String> {
    let (mut base, extra) =
        crate::compiler::apply_compile_unflags(base, &overlay.for_source(source), unflags);
    base.extend(extra);
    base
}

/// Foreign dependencies get the global overlay; project-as-library sources also
/// get build_src_flags and project script scopes, matching the common pipeline.
pub(super) fn flags(
    compiler: &dyn Compiler,
    ctx: &pipeline::BuildContext,
    project: bool,
) -> (Vec<String>, Vec<String>) {
    let (user, src) = ctx.compile_overlays();
    let overlay = if project { &src } else { &user };
    (
        effective_flags(
            compiler.c_flags(),
            overlay,
            &ctx.build_unflags,
            Path::new("library.c"),
        ),
        effective_flags(
            compiler.cpp_flags(),
            overlay,
            &ctx.build_unflags,
            Path::new("library.cpp"),
        ),
    )
}

pub(super) async fn dependencies(
    params: &BuildParams,
    ctx: &pipeline::BuildContext,
    compiler: &dyn Compiler,
    toolchain: &fbuild_packages::toolchain::ArmToolchain,
    includes: &mut Vec<PathBuf>,
    declared: &[String],
) -> Result<pipeline::LibDeps> {
    use fbuild_packages::Toolchain;
    let ignored = ctx
        .config
        .get_lib_ignore(&params.env_name)
        .unwrap_or_default();
    let (c, cpp) = flags(compiler, ctx, false);
    pipeline::resolve_lib_deps(
        declared,
        &ignored,
        &params.project_dir,
        &ctx.build_dir,
        &toolchain.get_gcc_path(),
        &toolchain.get_gxx_path(),
        &toolchain.get_ar_path(),
        &toolchain.get_gcc_ar_path(),
        &c,
        &cpp,
        includes,
        params.verbose,
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_language_flags_and_unflags_reach_library_commands() {
        let overlay = LanguageExtraFlags {
            common: vec!["-DMY_FEATURE=42".into()],
            c: vec!["-DC_ONLY=1".into()],
            cxx: vec!["-std=gnu++17".into()],
            ..Default::default()
        };
        let removed = vec!["-fno-rtti".into(), "-std=gnu++14".into()];
        let base = vec![
            "-mcpu=cortex-m0plus".into(),
            "-fno-rtti".into(),
            "-std=gnu++14".into(),
        ];
        let cpp = effective_flags(base.clone(), &overlay, &removed, Path::new("lib.cpp"));
        assert!(cpp.contains(&"-DMY_FEATURE=42".into()));
        assert!(cpp.contains(&"-std=gnu++17".into()));
        assert!(!cpp.contains(&"-DC_ONLY=1".into()));
        assert!(!cpp.contains(&"-fno-rtti".into()));
        assert!(!cpp.contains(&"-std=gnu++14".into()));
        let c = effective_flags(base, &overlay, &removed, Path::new("lib.c"));
        assert!(c.contains(&"-DC_ONLY=1".into()));
        assert!(!c.contains(&"-std=gnu++17".into()));
    }

    #[test]
    fn spi_and_wire_are_selected_and_not_downloaded_as_external_deps() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let src = project.join("src");
        let libraries = root.path().join("framework/libraries");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("main.cpp"),
            "#include <SPI.h>\n#include <Wire.h>\n",
        )
        .unwrap();
        for name in ["SPI", "Wire"] {
            let lib = libraries.join(name).join("src");
            std::fs::create_dir_all(&lib).unwrap();
            std::fs::write(lib.join(format!("{name}.h")), "#pragma once\n").unwrap();
            std::fs::write(
                lib.join(format!("{name}.cpp")),
                format!("#include \"{name}.h\"\n"),
            )
            .unwrap();
        }
        let discovered =
            fbuild_packages::library::framework_library::discover_framework_libraries(&libraries);
        let declared = vec!["SPI".into(), "Wire".into(), "SomeExternal".into()];
        let selected = bundled(
            &project,
            &src,
            &discovered,
            &HashMap::new(),
            &declared,
            None,
        )
        .unwrap();
        assert_eq!(selected.sources.len(), 2);
        assert_eq!(selected.archives.len(), 2);
        assert_eq!(selected.includes.len(), 2);
        assert_eq!(selected.external, vec!["SomeExternal".to_string()]);
    }
}
