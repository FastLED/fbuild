//! Per-request bridge to daemon-owned prebuilt storage and one source retry.
use crate::{
    BuildOrchestrator, BuildParams, BuildResult,
    prebuilt_fp::{FrameworkBuildPlan, fingerprint},
};
use fbuild_packages::prebuilt::{MissReason, PrebuiltOutcome};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

type Fetch = dyn Fn(FrameworkBuildPlan) -> Pin<Box<dyn Future<Output = PrebuiltOutcome> + Send>>
    + Send
    + Sync;

#[derive(Clone)]
pub struct Client {
    pub fetch: Arc<Fetch>,
    pub reject: Arc<dyn Fn(&str) + Send + Sync>,
    used: Arc<Mutex<Vec<String>>>,
}
impl Client {
    pub fn new(fetch: Arc<Fetch>, reject: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self {
            fetch,
            reject,
            used: Arc::default(),
        }
    }
    pub async fn resolve(&self, plan: FrameworkBuildPlan) -> PrebuiltOutcome {
        let fp = fingerprint(&plan);
        let outcome = (self.fetch)(plan).await;
        if let PrebuiltOutcome::Hit(_) = &outcome {
            self.used.lock().unwrap_or_else(|e| e.into_inner()).push(fp);
        }
        outcome
    }
    pub fn used(&self) -> bool {
        !self
            .used
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
    fn reject_used(&self) {
        for fp in self.used.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            (self.reject)(fp);
        }
    }
}

pub async fn build_with_retry(
    orchestrator: &dyn BuildOrchestrator,
    mut params: BuildParams,
) -> fbuild_core::Result<BuildResult> {
    let result = orchestrator.build(&params).await;
    if result.as_ref().is_ok_and(|r| r.success)
        || !params.prebuilt.as_ref().is_some_and(Client::used)
    {
        return result;
    }
    params
        .prebuilt
        .as_ref()
        .expect("hit requires a client")
        .reject_used();
    params.prebuilt = None;
    tracing::info!("prebuilt: compile/link failed after hit; retrying once from source");
    // The hit path never populates the reusable core cache. Discard project
    // outputs so the retry cannot consume any installed prebuilt object.
    if params.build_dir.exists() {
        std::fs::remove_dir_all(&params.build_dir)?;
    }
    orchestrator.build(&params).await
}

pub fn disabled() -> PrebuiltOutcome {
    PrebuiltOutcome::Miss(MissReason("opted out".into()))
}
