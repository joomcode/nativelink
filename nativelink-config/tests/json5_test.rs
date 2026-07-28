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
