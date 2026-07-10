use freezeit_daemon::config::loader::{resolve_module_dir, DEFAULT_MODULE_DIR, MODULE_DIR_ENV};

#[test]
fn module_dir_prefers_cli_then_environment_then_default() {
    assert_eq!(
        resolve_module_dir(
            ["freezeit", "--module-dir", "/cli/module"],
            Some("/env/module")
        )
        .unwrap(),
        "/cli/module"
    );
    assert_eq!(
        resolve_module_dir(["freezeit"], Some("/env/module")).unwrap(),
        "/env/module"
    );
    assert_eq!(
        resolve_module_dir(["freezeit"], None).unwrap(),
        DEFAULT_MODULE_DIR
    );
    assert_eq!(MODULE_DIR_ENV, "FREEZEIT_MODULE_DIR");
}

#[test]
fn module_dir_rejects_missing_cli_value() {
    assert!(resolve_module_dir(["freezeit", "--module-dir"], None).is_err());
}
