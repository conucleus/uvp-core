//! YAML 标量 parity 语料（fixtures/yaml/yaml-scalar-parity.v1.json）Rust 线
//! 消费：钉各 token 经 uvp-cli read_definition 同路径（serde_yaml_ng ->
//! serde_json::Value）装载后的形态。

use serde_json::Value;

const CORPUS: &str = include_str!("../../../fixtures/yaml/yaml-scalar-parity.v1.json");

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParityCorpus {
    schema_version: String,
    cases: Vec<ParityCase>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParityCase {
    name: String,
    yaml: String,
    path: String,
    rust: Option<TrackExpectation>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackExpectation {
    form: String,
    #[serde(default)]
    value: Option<Value>,
}

#[test]
fn yaml_scalars_load_to_the_pinned_rust_forms() {
    let corpus: ParityCorpus =
        serde_json::from_str(CORPUS).expect("yaml scalar parity corpus should decode");
    assert_eq!(
        corpus.schema_version, "uvp.yamlScalarParity.v1",
        "parity corpus schemaVersion drifted; migrate every consumer before shipping"
    );
    for case in &corpus.cases {
        let Some(expectation) = &case.rust else {
            continue;
        };
        let loaded: serde_json::Value = serde_yaml_ng::from_str(&case.yaml)
            .unwrap_or_else(|err| panic!("{} must load: {err}", case.name));
        assert!(
            loaded
                .as_object()
                .unwrap_or_else(|| panic!("{} must load to a mapping", case.name))
                .contains_key(&case.path),
            "{}: 装载结果缺少路径 {:?} 的键（form=null 只接受显式 null，不接受键缺失）",
            case.name,
            case.path
        );
        let actual = &loaded[&case.path];
        let expected = match expectation.form.as_str() {
            "string" | "number" | "boolean" | "object" => {
                expectation.value.clone().unwrap_or(Value::Null)
            }
            "null" => Value::Null,
            other => panic!("{} carries unknown form {other}", case.name),
        };
        assert_eq!(
            actual, &expected,
            "{}: rust 装载形态漂移（路径 {:?}）",
            case.name, case.path
        );
    }
}
