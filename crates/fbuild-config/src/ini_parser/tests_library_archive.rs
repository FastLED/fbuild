use super::PlatformIOConfig;

#[test]
fn library_archive_option_preserves_explicitness_and_inheritance() {
    let root = tempfile::tempdir().unwrap();
    let ini = root.path().join("platformio.ini");
    std::fs::write(
        &ini,
        "[env:default]\nplatform=native\n[env:no]\nlib_archive=no\n\
         [env:child]\nextends=env:no\n[env:yes]\nlib_archive=TRUE ; explicit\n\
         [env:invalid]\nlib_archive=maybe\n",
    )
    .unwrap();
    let config = PlatformIOConfig::from_path(&ini).unwrap();
    assert_eq!(config.get_lib_archive("default").unwrap(), None);
    assert_eq!(config.get_lib_archive("no").unwrap(), Some(false));
    assert_eq!(config.get_lib_archive("child").unwrap(), Some(false));
    assert_eq!(config.get_lib_archive("yes").unwrap(), Some(true));
    assert!(config.get_lib_archive("invalid").is_err());
}
