//! Cache statistics and GC handlers.

use crate::context::DaemonContext;
use crate::models::{CacheStatsResponse, GcResponse};
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

/// GET /api/cache/stats
pub async fn cache_stats(State(_ctx): State<Arc<DaemonContext>>) -> Json<CacheStatsResponse> {
    match fbuild_packages::DiskCache::open() {
        Ok(dc) => match dc.stats() {
            Ok(stats) => Json(CacheStatsResponse {
                success: true,
                archive_bytes: stats.archive_bytes,
                installed_bytes: stats.installed_bytes,
                total_bytes: stats.total_bytes(),
                entry_count: stats.entry_count,
                high_watermark: stats.budget.high_watermark,
                low_watermark: stats.budget.low_watermark,
                archive_budget: stats.budget.archive_budget,
                message: None,
            }),
            Err(e) => Json(CacheStatsResponse {
                success: false,
                message: Some(format!("failed to read cache stats: {}", e)),
                ..Default::default()
            }),
        },
        Err(e) => Json(CacheStatsResponse {
            success: false,
            message: Some(format!("failed to open cache: {}", e)),
            ..Default::default()
        }),
    }
}

/// POST /api/cache/gc
pub async fn run_gc(State(ctx): State<Arc<DaemonContext>>) -> Json<GcResponse> {
    // Serialize with background GC loop to prevent interleaved deletes.
    let _guard = ctx.gc_mutex.lock().await;
    match fbuild_packages::DiskCache::open() {
        Ok(dc) => match dc.run_gc() {
            Ok(report) => Json(GcResponse {
                success: true,
                installed_evicted: report.installed_evicted,
                installed_bytes_freed: report.installed_bytes_freed,
                archives_evicted: report.archives_evicted,
                archive_bytes_freed: report.archive_bytes_freed,
                total_bytes_freed: report.total_bytes_freed(),
                orphan_files_removed: report.orphan_files_removed,
                orphan_rows_cleaned: report.orphan_rows_cleaned,
                message: None,
            }),
            Err(e) => Json(GcResponse {
                success: false,
                message: Some(format!("GC failed: {}", e)),
                ..Default::default()
            }),
        },
        Err(e) => Json(GcResponse {
            success: false,
            message: Some(format!("failed to open cache: {}", e)),
            ..Default::default()
        }),
    }
}
