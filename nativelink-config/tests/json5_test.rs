use std::fs;
use std::path::Path;

use nativelink_config::cas_server::CasConfig;

#[test]
fn test_example_parsing() {
    let mut examples_path = Path::new(".")
        .canonicalize()
        .expect("Can canonicalize current dir");

    if examples_path.join("nativelink-config").exists() {
        // inside bazel
        examples_path = examples_path.join("nativelink-config");
    }
    examples_path = examples_path.join("examples");

    let mut found_at_least_one_entry = false;

    for entry in fs::read_dir(&examples_path)
        .unwrap_or_else(|e| panic!("Failed to read from {:?}: {}", &examples_path, e))
    {
        let config_file = entry.unwrap().path().display().to_string();
        if !config_file.contains(".json5") {
            continue;
        }
        CasConfig::try_from_json5_file(&config_file)
            .unwrap_or_else(|e| panic!("Error while reading {config_file}: {e}"));
        found_at_least_one_entry = true;
    }

    assert!(found_at_least_one_entry);
}

#[test]
fn test_global_metrics_identity_allowlist() {
    // SAFETY: this is the only test in this binary that mutates the environment.
    unsafe { std::env::set_var("NL_TEST_CI_IDENTITY", "ci") };

    let config: CasConfig = serde_json5::from_str(
        r#"{
          "stores": [],
          "servers": [],
          "global": {
            "max_open_files": 512,
            "metrics_identity_allowlist": ["$NL_TEST_CI_IDENTITY", "local"],
          },
        }"#,
    )
    .expect("Config with a metrics identity allowlist should parse");

    let global = config.global.expect("global config present");
    assert_eq!(
        global.metrics_identity_allowlist,
        vec!["ci".to_string(), "local".to_string()],
        "identities should be shell-expanded"
    );
}

#[test]
fn test_global_metrics_identity_allowlist_defaults_to_empty() {
    let config: CasConfig = serde_json5::from_str(
        r#"{"stores": [], "servers": [], "global": {"max_open_files": 512}}"#,
    )
    .expect("Config without an identity allowlist should parse");

    assert!(
        config
            .global
            .expect("global config present")
            .metrics_identity_allowlist
            .is_empty()
    );
}

#[test]
fn test_global_config_fields_are_all_optional() {
    // A `global` block set for one reason (e.g. only an identity allowlist)
    // must not force unrelated fields to be spelled out. `max_open_files: 0`
    // means "use the default", which nativelink applies at startup.
    let config: CasConfig = serde_json5::from_str(
        r#"{
          "stores": [],
          "servers": [],
          "global": {
            "metrics_identity_allowlist": ["ci", "local"],
          },
        }"#,
    )
    .expect("A global block without max_open_files should parse");

    let global = config.global.expect("global config present");
    assert_eq!(global.max_open_files, 0, "0 signals the startup default");
    assert_eq!(global.default_digest_size_health_check, 0);
    assert!(global.default_digest_hash_function.is_none());
    assert_eq!(
        global.metrics_identity_allowlist,
        vec!["ci".to_string(), "local".to_string()]
    );
}
