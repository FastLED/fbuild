//! Registry files that omit or null `system` are universal packages.

use fbuild_core::platformio_package::{PackageKind, parse_package_spec, resolve_registry_json};

#[test]
fn registry_file_without_system_is_universal_and_exact_host_still_wins() {
    let system =
        fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
            .unwrap_or("linux_x86_64");
    let spec = parse_package_spec("acme/tool-ape@1.0.0").unwrap();
    let sha = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    for universal in [
        format!(
            r#"{{"download_url":"https://example.test/ape.zip","checksum":{{"sha256":"{sha}"}}}}"#
        ),
        format!(
            r#"{{"system":null,"download_url":"https://example.test/ape.zip","checksum":{{"sha256":"{sha}"}}}}"#
        ),
        format!(
            r#"{{"system":"*","download_url":"https://example.test/ape.zip","checksum":{{"sha256":"{sha}"}}}}"#
        ),
    ] {
        let response = format!(
            r#"{{"name":"tool-ape","owner":{{"username":"acme"}},
                "versions":[{{"name":"1.0.0","files":[{universal}]}}]}}"#
        );
        let payload = resolve_registry_json(
            spec.registry().unwrap(),
            PackageKind::Tool,
            system,
            &response,
        )
        .unwrap_or_else(|e| panic!("{universal} should resolve on {system}: {e}"));
        assert_eq!(payload.url, "https://example.test/ape.zip");
    }

    let response = format!(
        r#"{{"name":"tool-ape","owner":{{"username":"acme"}},
            "versions":[{{"name":"1.0.0","files":[
                {{"download_url":"https://example.test/ape.zip","checksum":{{"sha256":"{sha}"}}}},
                {{"system":["{system}"],"download_url":"https://example.test/native.zip","checksum":{{"sha256":"{sha}"}}}}
            ]}}]}}"#
    );
    let payload = resolve_registry_json(
        spec.registry().unwrap(),
        PackageKind::Tool,
        system,
        &response,
    )
    .unwrap();
    assert_eq!(payload.url, "https://example.test/native.zip");
}
