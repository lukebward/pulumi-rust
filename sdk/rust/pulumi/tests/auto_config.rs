//! Integration tests for stack configuration and settings: secrets
//! providers, secret flags, bulk and path-based config operations,
//! alternate config files, nested on-disk values, and the stack-settings
//! save/load round trip. Real `pulumi` CLI against a local file backend;
//! every test skips when the CLI is not on PATH.

mod common;

use std::time::Duration;

use common::TestEnv;
use pulumi::auto::{
    self, ConfigMap, ConfigOptions, ConfigValue, DestroyOptions, LocalWorkspaceOptions,
    ProjectSettings, Stack, StackSettingsConfigValue, UpOptions,
};

/// Write a minimal YAML project file into the test env's project dir.
fn write_project(env: &TestEnv, contents: &str) {
    std::fs::write(env.project_dir().join("Pulumi.yaml"), contents).unwrap();
}

async fn local_source_stack(env: &TestEnv, project_yaml: &str) -> Stack {
    write_project(env, project_yaml);
    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    Stack::create("dev", ws).await.expect("stack")
}

/// A passphrase provider round-trips a secret through set/up/get, and the
/// stack still decrypts after rotating to a new passphrase.
#[tokio::test]
async fn passphrase_secrets_provider_roundtrip_and_rotation() {
    require_cli!();
    const NEW_PASSPHRASE: &str = "an entirely different passphrase";
    let env = TestEnv::new();
    write_project(
        &env,
        "name: secretsprov\nruntime: yaml\noutputs:\n  fixed: ok\n",
    );

    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            secrets_provider: Some("passphrase".to_string()),
            ..Default::default()
        })
        .await;
    let mut stack = Stack::create("dev", ws).await.expect("stack");

    stack
        .set_config("creds", &ConfigValue::secret("s3cret-v1"))
        .await
        .expect("set secret");
    let before = stack.get_config("creds").await.expect("get config");
    assert_eq!(before.value, "s3cret-v1");
    assert!(before.secret, "value must round-trip as a secret");

    let up = stack.up(UpOptions::default()).await.expect("up");
    assert_eq!(
        up.summary.expect("up summary").result.as_deref(),
        Some("succeeded")
    );

    // The old passphrase decrypts via the env; the new one travels on
    // stdin. Guarded by a timeout: an unexpected extra prompt would
    // otherwise block on stdin forever.
    tokio::time::timeout(
        Duration::from_secs(120),
        stack
            .workspace()
            .change_stack_secrets_provider("dev", "passphrase", Some(NEW_PASSPHRASE)),
    )
    .await
    .expect("change-secrets-provider timed out")
    .expect("change secrets provider");

    stack
        .workspace_mut()
        .set_env_var("PULUMI_CONFIG_PASSPHRASE", NEW_PASSPHRASE);
    let after = stack.get_config("creds").await.expect("get after rotation");
    assert_eq!(after.value, "s3cret-v1");
    assert!(after.secret);

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// get/set/remove honor secret flags, a missing key errors, flag-like
/// values survive, and bulk set/remove work with mixed plain and secret
/// keys.
#[tokio::test]
async fn config_crud_and_bulk_operations() {
    require_cli!();
    let env = TestEnv::new();
    let stack = local_source_stack(&env, "name: cfgtest\nruntime: yaml\n").await;

    let missing = stack.get_config("missing").await.expect_err("missing key");
    assert!(
        missing.command_result().is_some(),
        "expected a CLI error, got: {missing}"
    );

    stack
        .set_config("plain", &ConfigValue::plain("abc"))
        .await
        .expect("set plain");
    stack
        .set_config("token", &ConfigValue::secret("hunter2"))
        .await
        .expect("set secret");
    // Flag-like values survive because the value travels after `--`.
    stack
        .set_config("dashPlain", &ConfigValue::plain("-value"))
        .await
        .expect("set flag-like plain");
    stack
        .set_config("dashSecret", &ConfigValue::secret("-value2"))
        .await
        .expect("set flag-like secret");

    let plain = stack.get_config("plain").await.expect("get plain");
    assert_eq!(plain.value, "abc");
    assert!(!plain.secret);
    let token = stack.get_config("token").await.expect("get secret");
    assert_eq!(token.value, "hunter2");
    assert!(token.secret, "secret flag must survive");
    let dash_plain = stack.get_config("dashPlain").await.expect("get dash plain");
    assert_eq!(dash_plain.value, "-value");
    assert!(!dash_plain.secret);
    let dash_secret = stack
        .get_config("dashSecret")
        .await
        .expect("get dash secret");
    assert_eq!(dash_secret.value, "-value2");
    assert!(dash_secret.secret);

    let bulk = ConfigMap::from([
        (
            "cfgtest:bulkPlain".to_string(),
            ConfigValue::plain("bulk-one"),
        ),
        (
            "cfgtest:bulkSecret".to_string(),
            ConfigValue::secret("bulk-two"),
        ),
    ]);
    stack.set_all_config(&bulk).await.expect("set-all");

    let all = stack.get_all_config().await.expect("get-all");
    assert_eq!(all["cfgtest:plain"].value, "abc");
    assert!(!all["cfgtest:plain"].secret);
    assert_eq!(all["cfgtest:token"].value, "hunter2");
    assert!(all["cfgtest:token"].secret);
    assert_eq!(all["cfgtest:bulkPlain"].value, "bulk-one");
    assert!(!all["cfgtest:bulkPlain"].secret);
    assert_eq!(all["cfgtest:bulkSecret"].value, "bulk-two");
    assert!(all["cfgtest:bulkSecret"].secret);
    assert_eq!(all["cfgtest:dashPlain"].value, "-value");

    stack.remove_config("plain").await.expect("remove");
    let all = stack.get_all_config().await.expect("get-all after rm");
    assert!(!all.contains_key("cfgtest:plain"));

    stack
        .workspace()
        .remove_all_config(
            "dev",
            &[
                "token",
                "bulkPlain",
                "bulkSecret",
                "dashPlain",
                "dashSecret",
            ],
            &ConfigOptions::default(),
        )
        .await
        .expect("rm-all");
    let all = stack.get_all_config().await.expect("get-all after rm-all");
    assert!(all.is_empty(), "leftover config: {all:?}");
}

/// path=true addresses nested subkeys, path=false keeps dotted keys
/// literal, secret flags survive, and removal by path leaves the
/// remaining subkeys as a JSON object.
#[tokio::test]
async fn path_based_config_addressing() {
    require_cli!();
    let env = TestEnv::new();
    let stack = local_source_stack(&env, "name: pathcfg\nruntime: yaml\n").await;
    let ws = stack.workspace();
    let path = ConfigOptions {
        path: true,
        ..Default::default()
    };

    ws.set_config("dev", "nested.one", &ConfigValue::plain("v1"), &path)
        .await
        .expect("set nested plain");
    ws.set_config("dev", "nested.two", &ConfigValue::secret("v2"), &path)
        .await
        .expect("set nested secret");
    ws.set_config(
        "dev",
        "literal.dot",
        &ConfigValue::plain("v3"),
        &ConfigOptions::default(),
    )
    .await
    .expect("set literal dotted key");

    let one = ws
        .get_config("dev", "nested.one", &path)
        .await
        .expect("get nested plain");
    assert_eq!(one.value, "v1");
    assert!(!one.secret);
    let two = ws
        .get_config("dev", "nested.two", &path)
        .await
        .expect("get nested secret");
    assert_eq!(two.value, "v2");
    assert!(two.secret, "secret flag must survive path addressing");

    // Without --path the dotted key is one literal key, not a traversal.
    let literal = ws
        .get_config("dev", "literal.dot", &ConfigOptions::default())
        .await
        .expect("get literal dotted key");
    assert_eq!(literal.value, "v3");
    let all = ws.get_all_config("dev").await.expect("get-all");
    assert!(all.contains_key("pathcfg:literal.dot"), "keys: {all:?}");
    assert!(all.contains_key("pathcfg:nested"), "keys: {all:?}");

    ws.remove_all_config("dev", &["nested.two"], &path)
        .await
        .expect("rm-all with path");
    let remaining = ws
        .get_config("dev", "nested", &ConfigOptions::default())
        .await
        .expect("get remaining object");
    assert!(!remaining.secret);
    let value: serde_json::Value =
        serde_json::from_str(&remaining.value).expect("object value reads as JSON");
    assert_eq!(value, serde_json::json!({"one": "v1"}));
}

/// ConfigOptions.config_file reads and writes Pulumi.alt.yaml or
/// Pulumi.alt.json instead of the stack's default config file.
#[tokio::test]
async fn config_file_option_targets_alternate_files() {
    require_cli!();
    let env = TestEnv::new();
    let stack = local_source_stack(&env, "name: altcfg\nruntime: yaml\n").await;
    let ws = stack.workspace();

    let yaml_path = env.project_dir().join("Pulumi.alt.yaml");
    let yaml_opts = ConfigOptions {
        config_file: Some(yaml_path.clone()),
        ..Default::default()
    };
    ws.set_config(
        "dev",
        "fromYaml",
        &ConfigValue::plain("yaml-value"),
        &yaml_opts,
    )
    .await
    .expect("set into alt yaml");
    let got = ws
        .get_config("dev", "fromYaml", &yaml_opts)
        .await
        .expect("get from alt yaml");
    assert_eq!(got.value, "yaml-value");
    let raw: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&yaml_path).expect("alt yaml exists"))
            .expect("alt yaml parses");
    assert_eq!(
        raw["config"]["altcfg:fromYaml"],
        serde_yaml_ng::Value::String("yaml-value".to_string()),
        "unexpected alt yaml: {raw:?}"
    );
    // The default stack file never saw the key.
    ws.get_config("dev", "fromYaml", &ConfigOptions::default())
        .await
        .expect_err("default config file must not hold the key");

    let json_path = env.project_dir().join("Pulumi.alt.json");
    let json_opts = ConfigOptions {
        config_file: Some(json_path.clone()),
        ..Default::default()
    };
    ws.set_config(
        "dev",
        "fromJson",
        &ConfigValue::plain("json-value"),
        &json_opts,
    )
    .await
    .expect("set into alt json");
    let got = ws
        .get_config("dev", "fromJson", &json_opts)
        .await
        .expect("get from alt json");
    assert_eq!(got.value, "json-value");
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).expect("alt json exists"))
            .expect("alt file is JSON");
    assert_eq!(
        parsed["config"]["altcfg:fromJson"],
        serde_json::json!("json-value")
    );

    ws.remove_config("dev", "fromYaml", &yaml_opts)
        .await
        .expect("rm from alt yaml");
    ws.get_config("dev", "fromYaml", &yaml_opts)
        .await
        .expect_err("key removed from alt yaml");
}

/// UpOptions/DestroyOptions.config_file run up and destroy against an
/// alternate config file holding a nested secret, mirroring Go's
/// TestUpOptsConfigFileNestedSecretLocalBackend.
#[tokio::test]
async fn up_and_destroy_honor_operation_level_config_file() {
    require_cli!();
    let env = TestEnv::new();

    let program = auto::program(|ctx| async move {
        let data = ctx.config().get("data").unwrap_or_default();
        ctx.export("data", pulumi::pv::string(data));
        Ok(())
    });
    let ws = env
        .workspace(LocalWorkspaceOptions {
            program: Some(program),
            project_settings: Some(ProjectSettings::new("altop", "rust")),
            ..Default::default()
        })
        .await;
    let stack = Stack::create("dev", ws).await.expect("stack");

    let alt = env.root.join("Pulumi.alt.yaml");
    let alt_opts = ConfigOptions {
        path: true,
        config_file: Some(alt.clone()),
    };
    stack
        .workspace()
        .set_config(
            "dev",
            "data.plain",
            &ConfigValue::plain("from-alt"),
            &alt_opts,
        )
        .await
        .expect("set nested plain");
    stack
        .workspace()
        .set_config(
            "dev",
            "data.token",
            &ConfigValue::secret("alt-secret"),
            &alt_opts,
        )
        .await
        .expect("set nested secret");
    let raw = std::fs::read_to_string(&alt).expect("alt file exists");
    assert!(raw.contains("secure:"), "no nested secret on disk: {raw}");

    let up = stack
        .up(UpOptions {
            config_file: Some(alt.clone()),
            diff: true,
            ..Default::default()
        })
        .await
        .expect("up");
    let exported = up.outputs["data"].value.as_str().expect("string output");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(exported).expect("output is JSON"),
        serde_json::json!({"plain": "from-alt", "token": "alt-secret"})
    );

    stack
        .destroy(DestroyOptions {
            config_file: Some(alt),
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}

/// Nested objects and lists in the on-disk stack file read back as
/// JSON-encoded strings with secret=false.
#[tokio::test]
async fn nested_on_disk_config_reads_as_json_strings() {
    require_cli!();
    let env = TestEnv::new();
    write_project(&env, "name: nestedcfg\nruntime: yaml\n");
    std::fs::write(
        env.project_dir().join("Pulumi.dev.yaml"),
        "config:\n  nestedcfg:list:\n    - one\n    - two\n    - three\n  nestedcfg:map:\n    lorem: ipsum\n",
    )
    .unwrap();

    let ws = env
        .workspace(LocalWorkspaceOptions {
            work_dir: Some(env.project_dir()),
            ..Default::default()
        })
        .await;
    let stack = Stack::create_or_select("dev", ws).await.expect("stack");

    let all = stack.get_all_config().await.expect("get-all");
    let list = &all["nestedcfg:list"];
    assert!(!list.secret);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&list.value).expect("list value is JSON"),
        serde_json::json!(["one", "two", "three"])
    );
    let map = &all["nestedcfg:map"];
    assert!(!map.secret);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&map.value).expect("map value is JSON"),
        serde_json::json!({"lorem": "ipsum"})
    );

    let got = stack.get_config("list").await.expect("get list");
    assert!(!got.secret);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&got.value).expect("list value is JSON"),
        serde_json::json!(["one", "two", "three"])
    );
}

/// save_stack_settings round-trips an edited config value into up outputs
/// and preserves the encryption fields on reload.
#[tokio::test]
async fn save_stack_settings_round_trip() {
    require_cli!();
    let env = TestEnv::new();
    let stack = local_source_stack(
        &env,
        "name: savetest\nruntime: yaml\nconfig:\n  bar:\n    type: string\n    default: unset\noutputs:\n  fromConfig: ${bar}\n",
    )
    .await;

    stack
        .set_config("bar", &ConfigValue::plain("initial"))
        .await
        .expect("set config");
    // A secret forces the passphrase salt into the settings file.
    stack
        .set_config("token", &ConfigValue::secret("hunter2"))
        .await
        .expect("set secret");

    let mut settings = stack
        .workspace()
        .stack_settings("dev")
        .expect("stack settings");
    assert!(
        settings.encryption_salt.is_some(),
        "passphrase stacks carry an encryption salt"
    );
    settings.config.as_mut().expect("config map").insert(
        "savetest:bar".to_string(),
        StackSettingsConfigValue::Plain(serde_yaml_ng::Value::String("baz".to_string())),
    );
    stack
        .workspace()
        .save_stack_settings("dev", &settings)
        .expect("save settings");

    let up = stack.up(UpOptions::default()).await.expect("up");
    assert_eq!(up.outputs["fromConfig"].value, serde_json::json!("baz"));

    let reloaded = stack
        .workspace()
        .stack_settings("dev")
        .expect("reload settings");
    assert_eq!(reloaded.secrets_provider, settings.secrets_provider);
    assert_eq!(reloaded.encrypted_key, settings.encrypted_key);
    assert_eq!(reloaded.encryption_salt, settings.encryption_salt);

    stack
        .destroy(DestroyOptions {
            remove: true,
            ..Default::default()
        })
        .await
        .expect("destroy");
}
