//! Local-fixture first-compile benchmark. See examples/README.md.
use fbuild_build::prebuilt_fp::{FrameworkBuildPlan, PackageIdentity, fingerprint};
use fbuild_core::path::NormalizedPath;
use fbuild_daemon::prebuilt::Service;
use fbuild_packages::prebuilt::{Asset, PrebuiltOutcome, sha256};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

type Error = Box<dyn std::error::Error + Send + Sync>;
struct Fixture {
    root: tempfile::TempDir,
    compiler: NormalizedPath,
    objcopy: NormalizedPath,
    plan: FrameworkBuildPlan,
}

async fn command(args: Vec<String>, cwd: &Path) -> Result<(), Error> {
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    let result = fbuild_core::subprocess::run_command(&args, Some(cwd), None, None).await?;
    if !result.success() {
        return Err(format!("compiler failed: {}", result.stderr).into());
    }
    Ok(())
}

impl Fixture {
    fn new(compiler: NormalizedPath, objcopy: NormalizedPath) -> Result<Self, Error> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("framework.h"),
            "int framework_value(void);\n",
        )?;
        std::fs::write(
            root.path().join("framework.c"),
            "#include \"framework.h\"\nint framework_value(void) { return 17; }\n",
        )?;
        std::fs::write(
            root.path().join("sketch.c"),
            "#include \"framework.h\"\nvolatile int result; void _start(void) { result = framework_value(); for (;;) {} }\n",
        )?;
        let prefix = format!("-ffile-prefix-map={}=/fbuild", root.path().display());
        let flags = vec!["-Os".into(), "-ffreestanding".into(), prefix];
        let (argv, _) = fbuild_build::compiler::compile_argv(
            &root.path().join("framework.c"),
            &root.path().join("core.o"),
            &flags,
            &[],
            &[],
        );
        let plan = FrameworkBuildPlan {
            abi_version: 1,
            fingerprint_schema: 1,
            toolchain: PackageIdentity {
                id: "xtensa-esp32-elf-gcc".into(),
                version: "local-fixture".into(),
                sha256: sha256(&std::fs::read(&compiler)?),
            },
            framework: PackageIdentity {
                id: "arduino-esp32-fixture".into(),
                version: "fixture-1".into(),
                sha256: sha256(&std::fs::read(root.path().join("framework.c"))?),
            },
            mcu: "esp32".into(),
            variant_dir: "esp32dev".into(),
            argv: vec![argv],
            config_sha256: BTreeMap::new(),
            path_map_prefix: "/fbuild".into(),
        };
        Ok(Self {
            root,
            compiler,
            objcopy,
            plan,
        })
    }
    async fn core(&self) -> Result<(), Error> {
        let mut args = vec![self.compiler.to_string_lossy().into_owned()];
        args.extend(self.plan.argv[0].clone());
        command(args, self.root.path()).await
    }
    async fn firmware(&self) -> Result<String, Error> {
        command(
            vec![
                self.compiler.to_string_lossy().into_owned(),
                "-Os".into(),
                "-ffreestanding".into(),
                "-nostdlib".into(),
                format!("-ffile-prefix-map={}=/fbuild", self.root.path().display()),
                "-Wl,-e,_start".into(),
                "sketch.c".into(),
                "core.o".into(),
                "-o".into(),
                "firmware.elf".into(),
            ],
            self.root.path(),
        )
        .await?;
        command(
            vec![
                self.objcopy.to_string_lossy().into_owned(),
                "-O".into(),
                "binary".into(),
                "firmware.elf".into(),
                "firmware.bin".into(),
            ],
            self.root.path(),
        )
        .await?;
        Ok(sha256(&std::fs::read(
            self.root.path().join("firmware.bin"),
        )?))
    }
    fn archive(&self) -> Result<Vec<u8>, Error> {
        let fp = fingerprint(&self.plan);
        let object = std::fs::read(self.root.path().join("core.o"))?;
        let header = std::fs::read(self.root.path().join("framework.h"))?;
        let manifest = serde_json::to_vec(&json!({"fp":fp,"abi_version":1,"inputs":self.plan,
            "builder_version":"fixture","fbuild_version":env!("CARGO_PKG_VERSION"),"source":{"repo_url":"local-fixture","ref":"fixture"},
            "files":{"obj/core.o":sha256(&object),"include/framework.h":sha256(&header)}}))?;
        let mut tar = tar::Builder::new(Vec::new());
        for (name, data) in [
            ("fbuild-prebuilt.json", manifest.as_slice()),
            ("obj/core.o", object.as_slice()),
            ("include/framework.h", header.as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, data)?;
        }
        Ok(zstd::encode_all(tar.into_inner()?.as_slice(), 19)?)
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

async fn measure(
    fixture: &Fixture,
    asset: &Asset,
    mode: &str,
) -> Result<(f64, u64, String, String), Error> {
    let mut samples = Vec::new();
    let mut binary_sha = String::new();
    let mut elf_sha = String::new();
    let mut bytes = 0;
    for _ in 0..5 {
        let isolated = tempfile::tempdir()?;
        let service = Service::new(isolated.path());
        let fp = fingerprint(&fixture.plan);
        if mode == "prebuilt-warm"
            && !matches!(service.fetch(asset, &fp).await, PrebuiltOutcome::Hit(_))
        {
            return Err("warmup miss".into());
        }
        for name in ["core.o", "firmware.elf", "firmware.bin"] {
            let _ = std::fs::remove_file(fixture.root.path().join(name));
        }
        let start = Instant::now();
        if mode == "source-cold" {
            fixture.core().await?;
        } else {
            let PrebuiltOutcome::Hit(entry) = service.fetch(asset, &fp).await else {
                return Err("fixture prebuilt miss".into());
            };
            std::fs::copy(entry.join("obj/core.o"), fixture.root.path().join("core.o"))?;
            if mode == "prebuilt-cold" {
                bytes += asset.size_bytes;
            }
        }
        let sha = fixture.firmware().await?;
        samples.push(start.elapsed().as_secs_f64());
        let elf = sha256(&std::fs::read(fixture.root.path().join("firmware.elf"))?);
        if !binary_sha.is_empty() && (sha != binary_sha || elf != elf_sha) {
            return Err("non-reproducible fixture".into());
        }
        binary_sha = sha;
        elf_sha = elf;
    }
    Ok((median(&mut samples), bytes / 5, binary_sha, elf_sha))
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let compiler = NormalizedPath::from(
        args.next()
            .ok_or("usage: bench/first_compile ESP32_GCC ESP32_OBJCOPY")?,
    );
    let objcopy = NormalizedPath::from(args.next().ok_or("missing ESP32_OBJCOPY")?);
    let fixture = Fixture::new(compiler, objcopy)?;
    fixture.core().await?;
    let bytes = fixture.archive()?;
    let length = bytes.len() as u64;
    let served = Arc::new(AtomicU64::new(0));
    let count = served.clone();
    let app = axum::Router::new().fallback(move || {
        let data = bytes.clone();
        let count = count.clone();
        async move {
            count.fetch_add(data.len() as u64, Ordering::SeqCst);
            data
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let asset = Asset {
        filename: "prebuilt.tar.zst".into(),
        size_bytes: length,
        sha256: sha256(&fixture.archive()?),
        urls: vec![url],
        provides: vec![],
        parts: vec![],
    };
    let source = measure(&fixture, &asset, "source-cold").await?;
    let cold = measure(&fixture, &asset, "prebuilt-cold").await?;
    let warm = measure(&fixture, &asset, "prebuilt-warm").await?;
    if source.2 != cold.2 || source.2 != warm.2 || source.3 != cold.3 || source.3 != warm.3 {
        return Err("firmware or ELF differs".into());
    }
    println!(
        "board | source-cold s | prebuilt-cold s | prebuilt-warm s | speedup | MB fetched | firmware identical"
    );
    println!(
        "esp32dev (local framework fixture) | {:.4} | {:.4} | {:.4} | {:.2}x | {:.6} | y",
        source.0,
        cold.0,
        warm.0,
        source.0 / cold.0,
        cold.1 as f64 / 1_000_000.0
    );
    println!(
        "firmware.bin sha256: {}\nfirmware.elf sha256: {}\nHTTP bytes served: {}",
        source.2,
        source.3,
        served.load(Ordering::SeqCst)
    );
    server.abort();
    Ok(())
}
