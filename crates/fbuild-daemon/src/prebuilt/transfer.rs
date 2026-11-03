//! Raw compressed byte Range/If-Range resume, with overlapping streaming extraction.
use super::{Service, extract};
use fbuild_core::{fs, path::NormalizedPath};
use fbuild_packages::prebuilt::Asset;
pub(super) use fbuild_packages::prebuilt::partial::{Meta, now};
use sha2::{Digest, Sha256};
use std::io::Read;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug)]
pub(super) enum Failure {
    Unavailable(String),
    Host(String),
    Integrity(String),
}
impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Integrity(error.to_string())
    }
}

struct Bridge {
    stream: tokio::io::DuplexStream,
    runtime: tokio::runtime::Handle,
}
impl Read for Bridge {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.runtime.block_on(self.stream.read(buffer))
    }
}

impl Service {
    async fn open_response(
        &self,
        asset: &Asset,
        url: &str,
        part: &std::path::Path,
        meta_path: &std::path::Path,
    ) -> Result<(reqwest::Response, u64), Failure> {
        let old = fs::read(meta_path)
            .await
            .ok()
            .and_then(|b| serde_json::from_slice::<Meta>(&b).ok());
        let mut offset = fs::metadata(part).await.map_or(0, |m| m.len());
        if !old
            .as_ref()
            .is_some_and(|m| m.url == url && m.etag.is_some())
            || offset >= asset.size_bytes
        {
            offset = 0;
        }
        let mut request = self.client.get(url);
        if offset > 0 {
            request = request
                .header(reqwest::header::RANGE, format!("bytes={offset}-"))
                .header(
                    reqwest::header::IF_RANGE,
                    old.as_ref()
                        .and_then(|m| m.etag.as_deref())
                        .unwrap_or_default(),
                );
        }
        let response = tokio::time::timeout(self.stall_timeout, request.send())
            .await
            .map_err(|_| Failure::Host("request headers stalled".into()))?
            .map_err(|e| Failure::Host(e.to_string()))?;
        if response.status().is_server_error() {
            return Err(Failure::Host(format!("HTTP {}", response.status())));
        }
        if !response.status().is_success() {
            return Err(Failure::Unavailable(format!("HTTP {}", response.status())));
        }
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|s| s.to_str().ok())
            .map(str::to_owned);
        if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            let expected = format!("bytes {offset}-{}", asset.size_bytes - 1);
            let valid = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|v| v == format!("{expected}/{}", asset.size_bytes));
            if offset == 0 || !valid || old.as_ref().and_then(|m| m.etag.as_ref()) != etag.as_ref()
            {
                return Err(Failure::Integrity("invalid resume range or ETag".into()));
            }
        } else {
            offset = 0;
        }
        Ok((response, offset))
    }

    pub(super) async fn transfer(
        &self,
        asset: &Asset,
        fp: &str,
        url: &str,
    ) -> Result<NormalizedPath, Failure> {
        let partial = self.root.join(".partial");
        let staging = self.root.join(".staging");
        fs::create_dir_all(&partial).await?;
        fs::create_dir_all(&staging).await?;
        let part = partial.join(format!("{}.part", asset.sha256));
        let meta_path = partial.join(format!("{}.meta", asset.sha256));
        let (response, offset) = self.open_response(asset, url, &part, &meta_path).await?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(offset == 0)
            .append(offset > 0)
            .open(&part)
            .await?;
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let meta = Meta {
            url: url.into(),
            etag,
            owner: self.owner.clone(),
            progress: now(),
        };
        write_meta(&meta_path, &meta).await?;
        let stage = tempfile::Builder::new()
            .prefix(&format!("{}.", asset.sha256))
            .tempdir_in(staging)?;
        let (mut writer, reader) = tokio::io::duplex(256 * 1024);
        let runtime = tokio::runtime::Handle::current();
        let fingerprint = fp.to_owned();
        let write_limit = self.write_limit;
        let decode = tokio::task::spawn_blocking(move || {
            extract::extract(
                Bridge {
                    stream: reader,
                    runtime,
                },
                stage.path(),
                &fingerprint,
                write_limit,
            )
            .map(|()| stage)
        });
        let stream_result = stream(
            &mut file,
            &part,
            &mut writer,
            response,
            offset,
            asset,
            &meta_path,
            meta,
            self.stall_timeout,
        )
        .await;
        drop(writer);
        let extracted = decode
            .await
            .map_err(|e| Failure::Integrity(e.to_string()))?;
        stream_result?;
        let stage = extracted.map_err(Failure::Integrity)?;
        let entries = self.root.join("entries");
        fs::create_dir_all(&entries).await?;
        fs::write(stage.path().join(".install_complete"), b"").await?;
        let destination = entries.join(fp);
        // Never expose an unverified or partially extracted directory.
        if destination.exists() {
            fs::remove_dir_all(&destination).await?;
        }
        fs::rename(stage.path(), &destination).await?;
        fs::remove_file(part).await?;
        fs::remove_file(meta_path).await?;
        Ok(destination)
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream(
    file: &mut fs::File,
    part: &std::path::Path,
    writer: &mut tokio::io::DuplexStream,
    mut response: reqwest::Response,
    offset: u64,
    asset: &Asset,
    meta_path: &std::path::Path,
    mut meta: Meta,
    stall: std::time::Duration,
) -> Result<(), Failure> {
    let mut digest = Sha256::new();
    let mut received = offset;
    if offset > 0 {
        let mut prior = fs::File::open(part).await?;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let n = prior.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
            writer.write_all(&buffer[..n]).await?;
        }
    }
    loop {
        let chunk = tokio::time::timeout(stall, response.chunk())
            .await
            .map_err(|_| Failure::Host("30s without progress".into()))?
            .map_err(|e| Failure::Host(e.to_string()))?;
        let Some(chunk) = chunk else {
            break;
        };
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| Failure::Integrity("size overflow".into()))?;
        if received > asset.size_bytes {
            return Err(Failure::Integrity("asset exceeds declared size".into()));
        }
        file.write_all(&chunk).await?;
        file.flush().await?;
        digest.update(&chunk);
        meta.progress = now();
        write_meta(meta_path, &meta).await?;
        writer.write_all(&chunk).await?;
    }
    writer.shutdown().await?;
    if received != asset.size_bytes || format!("{:x}", digest.finalize()) != asset.sha256 {
        return Err(Failure::Integrity(
            "whole asset sha256 or size mismatch".into(),
        ));
    }
    Ok(())
}

async fn write_meta(path: &std::path::Path, meta: &Meta) -> std::io::Result<()> {
    fs::write_atomic(path, serde_json::to_vec(meta)?).await
}
