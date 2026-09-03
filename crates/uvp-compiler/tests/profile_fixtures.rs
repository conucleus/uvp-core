use serde::Deserialize;
use serde_json::Value;
use uvp_compiler::{compile_request, CompileRequest};

/// Profile-level compilation fixtures live in `fixtures/{zhixu,cloud,evm}` and
/// follow the declaration shape promised by init_prd §8: name, semanticVersion,
/// input, expected, portable. This harness discovers every `*.json` file in
/// those directories, so adding a fixture never requires touching this test.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileFixture {
    name: String,
    #[allow(dead_code)]
    semantic_version: String,
    target: String,
    #[allow(dead_code)]
    portable: bool,
    input: Value,
    /// 可选 dock resolution manifest（PRD94 §5.2）：含 zhixu executor 的
    /// 可运行 fixture 必须内嵌 manifest。
    #[serde(default)]
    resolution_manifest: Option<Value>,
    expect: FixtureExpect,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureExpect {
    /// When present the fixture must fail to compile with a message containing
    /// this text.
    #[serde(default)]
    error_contains: Option<String>,
    /// Assert `plan.platform` on hook_plan success.
    #[serde(default)]
    platform: Option<String>,
    /// Assert the exact sorted set of `compiledHooks[].hookId`.
    #[serde(default)]
    hook_ids: Option<Vec<String>>,
    /// Assert `compiledHooks[].dependencies.len()` per `stageIdentifier#hookName`.
    #[serde(default)]
    hook_dependency_counts: Option<BTreeMap<String, usize>>,
    /// Assert `astJson.mode` per hookName on cloud-artifact success.
    #[serde(default)]
    cloud_hook_modes: Option<BTreeMap<String, String>>,
    /// Assert `dockRoutes.len()` on hook_plan success.
    #[serde(default)]
    dock_route_count: Option<usize>,
}

use std::collections::BTreeMap;

fn fixture_paths(profile_dir: &str) -> Vec<std::path::PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(profile_dir);
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|err| panic!("read fixture dir {}: {err}", root.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn compiles_profile_fixtures() {
    let mut discovered = 0;
    for profile_dir in ["zhixu", "cloud", "evm"] {
        for path in fixture_paths(profile_dir) {
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            let fixture: ProfileFixture = serde_json::from_str(&raw).unwrap_or_else(|err| {
                panic!(
                    "{profile_dir}/{} declares an invalid fixture: {err}",
                    path.file_name().unwrap().to_string_lossy()
                )
            });
            run_fixture(&fixture);
            discovered += 1;
        }
    }
    assert!(
        discovered >= 10,
        "profile fixture discovery regressed: only {discovered} fixtures found"
    );
}

fn run_fixture(fixture: &ProfileFixture) {
    let result = compile_request(&CompileRequest {
        target: fixture.target.clone(),
        definition: fixture.input.clone(),
        resolution_manifest: fixture.resolution_manifest.clone(),
    });
    match (&fixture.expect.error_contains, result) {
        (Some(expected), Err(err)) => {
            assert!(
                err.to_string().contains(expected.as_str()),
                "{} error {:?} did not contain {expected:?}",
                fixture.name,
                err.to_string()
            );
        }
        (Some(expected), Ok(_)) => panic!(
            "{} expected compilation error containing {expected:?}, got success",
            fixture.name
        ),
        (None, Err(err)) => panic!("{} failed to compile: {err}", fixture.name),
        (None, Ok(value)) => assert_success_fields(fixture, &value),
    }
}

fn assert_success_fields(fixture: &ProfileFixture, value: &Value) {
    if let Some(platform) = &fixture.expect.platform {
        assert_eq!(
            value["platform"]["type"].as_str().unwrap_or_default(),
            platform,
            "{} platform mismatch",
            fixture.name
        );
    }
    if let Some(expected_ids) = &fixture.expect.hook_ids {
        let mut actual_ids: Vec<String> = value["compiledHooks"]
            .as_array()
            .expect("compiledHooks should be an array")
            .iter()
            .map(|hook| hook["hookId"].as_str().unwrap_or_default().to_string())
            .collect();
        actual_ids.sort();
        assert_eq!(
            &actual_ids, expected_ids,
            "{} compiled hook ids mismatch",
            fixture.name
        );
    }
    if let Some(expected_counts) = &fixture.expect.hook_dependency_counts {
        let hooks = value["compiledHooks"]
            .as_array()
            .expect("compiledHooks should be an array");
        for (hook_id, expected_count) in expected_counts {
            let dependency_count = hooks
                .iter()
                .find(|hook| hook["hookId"].as_str().is_some_and(|id| id == hook_id))
                .map(|hook| {
                    hook["dependencies"]
                        .as_array()
                        .map(Vec::len)
                        .unwrap_or_default()
                })
                .unwrap_or_else(|| panic!("{} missing hook {hook_id}", fixture.name));
            assert_eq!(
                &dependency_count, expected_count,
                "{} dependency count mismatch for {hook_id}",
                fixture.name
            );
        }
    }
    if let Some(expected_count) = &fixture.expect.dock_route_count {
        let actual = value["dockRoutes"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();
        assert_eq!(
            &actual, expected_count,
            "{} dock route count mismatch",
            fixture.name
        );
    }
    if let Some(expected_modes) = &fixture.expect.cloud_hook_modes {
        let hooks = value["hooks"]
            .as_array()
            .expect("cloud artifact hooks should be an array");
        for (hook_name, expected_mode) in expected_modes {
            let actual_mode = hooks
                .iter()
                .find(|hook| {
                    hook["hookName"]
                        .as_str()
                        .is_some_and(|name| name == hook_name)
                })
                .map(|hook| {
                    hook["astJson"]["mode"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .unwrap_or_else(|| panic!("{} missing cloud hook {hook_name}", fixture.name));
            assert_eq!(
                &actual_mode, expected_mode,
                "{} cloud hook mode mismatch for {hook_name}",
                fixture.name
            );
        }
    }
}
