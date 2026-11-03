//! The optional cache path is exercised around a real local C compile/link.
use super::{
    Service,
    tests::{asset, bundle, server},
};
use fbuild_build::{
    BuildOrchestrator, BuildParams, BuildResult,
    prebuilt_fp::{FrameworkBuildPlan, fingerprint},
};
use fbuild_packages::prebuilt::PrebuiltOutcome;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct SourceBuild {
    plan: FrameworkBuildPlan,
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl BuildOrchestrator for SourceBuild {
    fn platform(&self) -> fbuild_core::Platform {
        fbuild_core::Platform::Espressif32
    }
    async fn build(&self, params: &BuildParams) -> fbuild_core::Result<BuildResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(client) = &params.prebuilt {
            if matches!(
                client.resolve(self.plan.clone()).await,
                PrebuiltOutcome::Hit(_)
            ) {
                return Err(fbuild_core::FbuildError::BuildFailed(
                    "fixture linker rejects prebuilt".into(),
                ));
            }
        }
        std::fs::create_dir_all(&params.build_dir)?;
        let source = params.project_dir.join("fixture.c");
        let firmware = params.build_dir.join("firmware.bin");
        let result = fbuild_core::subprocess::run_command(
            &[
                if cfg!(windows) { "clang" } else { "cc" },
                source.to_str().unwrap(),
                "-o",
                firmware.to_str().unwrap(),
            ],
            None,
            None,
            None,
        )
        .await?;
        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(result.stderr));
        }
        Ok(BuildResult {
            success: true,
            firmware_path: Some(firmware),
            elf_path: None,
            size_info: None,
            symbol_map: None,
            build_time_secs: 0.0,
            message: "real source build".into(),
            compile_database_path: None,
            build_log: fbuild_core::BuildLog::new(),
        })
    }
}

fn params(root: &std::path::Path, client: fbuild_build::prebuilt::Client) -> BuildParams {
    BuildParams {
        prebuilt: Some(client),
        project_dir: root.to_path_buf(),
        env_name: "fixture".into(),
        clean_all: false,
        clean_only: false,
        clean: false,
        profile: fbuild_core::BuildProfile::Release,
        build_dir: root.join("build"),
        verbose: false,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: true,
        src_dir: None,
        pio_env: BTreeMap::new(),
        extra_build_flags: vec![],
        watch_set_cache: None,
        bloat_analysis: false,
        shrink: Default::default(),
        caller_path: None,
    }
}

#[tokio::test]
async fn all_mirrors_down_still_builds_from_source_and_a_failed_hit_retries_once() {
    for status in [
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::http::StatusCode::OK,
    ] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("fixture.c"),
            "int main(void) {return 0;}\n",
        )
        .unwrap();
        let plan = super::tests::fixture_plan();
        let fp = fingerprint(&plan);
        let bytes = bundle(&fp);
        let (url, _, task) = server(bytes.clone(), status).await;
        let service = Arc::new(Service::new(root.path()));
        let request_asset = asset(&bytes, vec![url]);
        let fetcher = service.clone();
        let rejecter = service.clone();
        let expected = fp.clone();
        let client = fbuild_build::prebuilt::Client::new(
            Arc::new(move |_| {
                let service = fetcher.clone();
                let asset = request_asset.clone();
                let fp = expected.clone();
                Box::pin(async move { service.fetch(&asset, &fp).await })
            }),
            Arc::new(move |fp| rejecter.tombstone(fp)),
        );
        let source = SourceBuild {
            plan,
            calls: AtomicUsize::new(0),
        };
        let result = fbuild_build::prebuilt::build_with_retry(&source, params(root.path(), client))
            .await
            .unwrap();
        assert!(result.success && result.firmware_path.unwrap().is_file());
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            if status.is_success() { 2 } else { 1 }
        );
        assert_eq!(service.tombstoned(&fp), status.is_success());
        task.abort();
    }
}
