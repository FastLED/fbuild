//! Daemon-owned integrity-checked prebuilt fetches. No cache failure fails a build.
mod transfer;
use fbuild_core::path::NormalizedPath;
use fbuild_packages::prebuilt::archive as extract;
use fbuild_packages::prebuilt::{Asset, MissReason, PrebuiltOutcome, valid_sha};
use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::OnceCell;

type Flight = Arc<OnceCell<PrebuiltOutcome>>;
#[derive(Default)]
struct State {
    flights: HashMap<String, (Instant, Flight)>,
    live: HashSet<String>,
    tombstones: HashMap<String, Instant>,
    hosts: HashMap<String, (Instant, Duration)>,
}

pub struct Service {
    root: NormalizedPath,
    state: Mutex<State>,
    client: reqwest::Client,
    owner: String,
    stall_timeout: Duration,
    write_limit: Option<u64>,
}

struct Live<'a> {
    service: &'a Service,
    sha: String,
}
impl Drop for Live<'_> {
    fn drop(&mut self) {
        self.service.state.lock().live.remove(&self.sha);
    }
}

impl Service {
    pub fn client(self: &Arc<Self>) -> fbuild_build::prebuilt::Client {
        let fetcher = self.clone();
        let rejecter = self.clone();
        let leases = Arc::new(Mutex::new(Vec::new()));
        fbuild_build::prebuilt::Client::new(
            Arc::new(move |plan| {
                let service = fetcher.clone();
                let leases = leases.clone();
                Box::pin(async move {
                    let fp = fbuild_build::prebuilt_fp::fingerprint(&plan);
                    let query = fbuild_packages::prebuilt::Query {
                        framework: "arduino-esp32".into(),
                        arch: plan.mcu,
                        abi: format!("{}@{}", plan.toolchain.id, plan.toolchain.version),
                        version: plan.framework.version,
                        fp: fp.clone(),
                    };
                    let reader = fbuild_packages::prebuilt::Reader::from_environment(
                        service.root.parent().unwrap_or(&service.root),
                    );
                    match reader.resolve(&query).await {
                        fbuild_packages::prebuilt::Resolution::Hit(asset) => {
                            let outcome = service.fetch(&asset, &fp).await;
                            if matches!(outcome, PrebuiltOutcome::Hit(_)) {
                                service.lease_entry(&asset, &fp, &leases).await;
                            }
                            outcome
                        }
                        fbuild_packages::prebuilt::Resolution::Miss(reason) => {
                            tracing::info!("prebuilt: {reason}; building from source");
                            PrebuiltOutcome::Miss(reason)
                        }
                    }
                })
            }),
            Arc::new(move |fp| rejecter.tombstone(fp)),
        )
    }
    pub fn new(cache_root: &Path) -> Self {
        let service = Self {
            root: NormalizedPath::new(cache_root).join("prebuilt"),
            state: Mutex::default(),
            client: fbuild_packages::http::client_with_timeout(Duration::from_secs(30)),
            owner: uuid::Uuid::new_v4().to_string(),
            stall_timeout: Duration::from_secs(30),
            write_limit: None,
        };
        if let Err(error) = service.gc() {
            tracing::info!("prebuilt: startup GC: {error}; building from source on a miss");
        }
        service
    }

    pub async fn fetch(&self, asset: &Asset, fp: &str) -> PrebuiltOutcome {
        if !valid_sha(fp)
            || !valid_sha(&asset.sha256)
            || asset.size_bytes == 0
            || !asset.parts.is_empty()
        {
            return miss("invalid fingerprint, checksum, or multipart asset");
        }
        if self.tombstoned(fp) {
            return miss("fingerprint tombstoned");
        }
        let flight = {
            let mut state = self.state.lock();
            let expired = state
                .flights
                .get(&asset.sha256)
                .is_some_and(|(since, cell)| {
                    cell.get().is_some_and(|outcome| match outcome {
                        PrebuiltOutcome::Hit(path) => !path.exists(),
                        PrebuiltOutcome::Miss(_) => since.elapsed() >= Duration::from_secs(30),
                    })
                });
            if expired {
                state.flights.remove(&asset.sha256);
            }
            state
                .flights
                .entry(asset.sha256.clone())
                .or_insert_with(|| (Instant::now(), Arc::default()))
                .1
                .clone()
        };
        let outcome = flight
            .get_or_init(|| async {
                self.state.lock().live.insert(asset.sha256.clone());
                let _live = Live {
                    service: self,
                    sha: asset.sha256.clone(),
                };
                self.fetch_inner(asset, fp).await
            })
            .await
            .clone();
        // A malicious catalog can associate one asset with multiple fingerprints.
        match outcome {
            PrebuiltOutcome::Hit(path) if verified(&path, fp).await => {
                self.touch_entry(asset, fp, &path).await;
                PrebuiltOutcome::Hit(path)
            }
            PrebuiltOutcome::Hit(_) => miss("cached entry fingerprint mismatch"),
            other => other,
        }
    }

    async fn fetch_inner(&self, asset: &Asset, fp: &str) -> PrebuiltOutcome {
        let entry = self.root.join("entries").join(fp);
        if verified(&entry, fp).await {
            return PrebuiltOutcome::Hit(entry.into_path_buf());
        }
        for url in &asset.urls {
            if !self.available(url, &asset.sha256) {
                continue;
            }
            match self.transfer(asset, fp, url).await {
                Ok(path) => return PrebuiltOutcome::Hit(path.into_path_buf()),
                Err(error) => self.failed(asset, url, error).await,
            }
        }
        miss("all artifact mirrors unavailable or invalid")
    }

    async fn failed(&self, asset: &Asset, url: &str, error: transfer::Failure) {
        let reason = match error {
            transfer::Failure::Unavailable(reason) => reason,
            transfer::Failure::Host(reason) => {
                self.backoff(url);
                reason
            }
            transfer::Failure::Integrity(reason) => {
                for extension in ["part", "meta"] {
                    let _ = fbuild_core::fs::remove_file(
                        self.root
                            .join(".partial")
                            .join(format!("{}.{extension}", asset.sha256)),
                    )
                    .await;
                }
                self.tombstone(&format!("{url}\n{}", asset.sha256));
                reason
            }
        };
        tracing::info!("prebuilt: {reason}; trying next mirror");
    }

    pub fn tombstone(&self, key: &str) {
        self.state
            .lock()
            .tombstones
            .insert(key.to_owned(), Instant::now());
    }

    fn tombstoned(&self, key: &str) -> bool {
        self.state
            .lock()
            .tombstones
            .get(key)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(24 * 60 * 60))
    }

    fn available(&self, url: &str, sha: &str) -> bool {
        if self.tombstoned(&format!("{url}\n{sha}")) {
            return false;
        }
        let Ok(parsed) = reqwest::Url::parse(url) else {
            return false;
        };
        if parsed.scheme() != "https"
            && !(parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
        {
            return false;
        }
        let state = self.state.lock();
        state
            .hosts
            .get(&parsed.origin().ascii_serialization())
            .is_none_or(|(since, delay)| since.elapsed() >= *delay)
    }

    fn backoff(&self, url: &str) {
        if let Ok(url) = reqwest::Url::parse(url) {
            let mut state = self.state.lock();
            let host = url.origin().ascii_serialization();
            let delay = state
                .hosts
                .get(&host)
                .map_or(Duration::from_secs(30), |(_, d)| {
                    (*d * 2).min(Duration::from_secs(600))
                });
            state.hosts.insert(host, (Instant::now(), delay));
        }
    }

    async fn touch_entry(&self, asset: &Asset, fp: &str, path: &Path) {
        let root = self.root.clone();
        let sha = asset.sha256.clone();
        let fp = fp.to_owned();
        let path = NormalizedPath::new(path);
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(cache) = fbuild_packages::DiskCache::open_at(root.parent().unwrap_or(&root)) {
                let size = i64::try_from(fbuild_packages::prebuilt::installed_size(&path))
                    .unwrap_or(i64::MAX);
                let _ = cache.record_install(
                    fbuild_packages::disk_cache::Kind::Frameworks,
                    &format!("prebuilt:{sha}"),
                    &fp,
                    &path.to_string_lossy(),
                    size,
                );
            }
        })
        .await;
    }

    async fn lease_entry(
        &self,
        asset: &Asset,
        fp: &str,
        leases: &Arc<Mutex<Vec<fbuild_packages::disk_cache::Lease>>>,
    ) {
        let root = self.root.clone();
        let sha = asset.sha256.clone();
        let fp = fp.to_owned();
        let leases = leases.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(cache) = fbuild_packages::DiskCache::open_at(root.parent().unwrap_or(&root)) {
                if let Ok(Some(entry)) = cache.lookup(
                    fbuild_packages::disk_cache::Kind::Frameworks,
                    &format!("prebuilt:{sha}"),
                    &fp,
                ) {
                    if let Ok(lease) = cache.lease(&entry) {
                        leases.lock().push(lease);
                    }
                }
            }
        })
        .await;
    }
}

fn miss(reason: &str) -> PrebuiltOutcome {
    tracing::info!("prebuilt: {reason}; building from source");
    PrebuiltOutcome::Miss(MissReason(reason.to_owned()))
}
#[cfg(test)]
mod tests;

#[cfg(test)]
mod fallback_tests;

#[cfg(test)]
mod resume_tests;

impl Service {
    pub fn gc(&self) -> std::io::Result<()> {
        let state = self.state.lock();
        fbuild_packages::prebuilt::gc::collect(&self.root, &state.live)
    }
}

async fn verified(path: &Path, fp: &str) -> bool {
    let path = NormalizedPath::new(path);
    let fp = fp.to_owned();
    tokio::task::spawn_blocking(move || extract::verify_entry(&path, &fp).is_ok())
        .await
        .unwrap_or(false)
}
