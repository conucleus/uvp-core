use super::*;
use serde_json::json;

fn target_payment_definition() -> Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "payment_flow" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [
                {
                    "name": "init",
                    "source": "payment",
                    "receiveSignals": { "START": "payment::init.str" },
                    "sendSignals": [{ "name": "str" }, { "name": "execute" }],
                    "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
                },
                {
                    "name": "control",
                    "source": "payment",
                    "receiveSignals": { "CANCEL": "payment::init.str" },
                    "sendSignals": [{ "name": "cxl" }],
                    "executor": { "supplierType": "organization", "supplierID": "control-org" }
                },
                {
                    "name": "settle",
                    "source": "payment",
                    "receiveSignals": { "SETTLE": "payment::init.str" },
                    "sendSignals": [{ "name": "cmp" }],
                    "executor": { "supplierType": "organization", "supplierID": "settlement-org" }
                }
            ]
        }
    })
}

#[test]
fn send_signals_total_is_uncapped() {
    let definition_with = |count: usize| {
        let signals: Vec<Value> = (0..count)
            .map(|index| json!({ "name": format!("sig{index:03}") }))
            .collect();
        json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "capability_scale" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "cap-core" },
                "stages": [{
                            "name": "work",
                            "source": "buyer",
                            "receiveSignals": { "START": "buyer::work.sig000" },
                            "sendSignals": signals,
                            "executor": { "supplierType": "organization", "supplierID": "buyer-app" }
                        }]
            }
        })
    };
    let plan =
        compile_zhixu_hook_plan(&definition_with(257)).expect("capability count is uncapped");
    assert_eq!(
        plan["signalCapabilities"].as_array().map(Vec::len),
        Some(257)
    );
    compile_cloud_artifact(&definition_with(257))
        .expect("cloud target accepts the same capability table");
}

#[test]
fn rejects_unknown_spec_and_executor_fields() {
    let mut parent = target_payment_definition();
    parent["spec"]["trigger"] = json!([]);
    let error =
        compile_zhixu_hook_plan(&parent).expect_err("unsupported spec-level trigger key must fail");
    assert!(
        error.to_string().contains("unknown field `trigger`"),
        "{error}"
    );

    let mut parent = target_payment_definition();
    parent["spec"]["externalSignals"] = json!({});
    let error = compile_zhixu_hook_plan(&parent)
        .expect_err("unsupported spec-level externalSignals key must fail");
    assert!(
        error
            .to_string()
            .contains("unknown field `externalSignals`"),
        "{error}"
    );

    let mut parent = target_payment_definition();
    parent["spec"]["stages"][2]["executor"]["handlerType"] = json!("http");
    let error = compile_zhixu_hook_plan(&parent).expect_err("unknown executor field must fail");
    assert!(
        error.to_string().contains("unknown field `handlerType`"),
        "{error}"
    );

    let mut parent = target_payment_definition();
    parent["metadata"]["description"] = json!("demo");
    let error =
        compile_zhixu_hook_plan(&parent).expect_err("metadata-level unknown field must fail");
    assert!(
        error.to_string().contains("unknown field `description`"),
        "{error}"
    );

    let mut parent = target_payment_definition();
    parent["metadata"]["uid"] = json!("zx-hand-written");
    let error = compile_zhixu_hook_plan(&parent)
        .expect_err("metadata.uid must be rejected as an unknown field");
    assert!(error.to_string().contains("unknown field `uid`"), "{error}");
}

#[test]
fn rejects_selectable_resource_outside_the_closed_file_type_set() {
    for (label, value) in [
        (
            "misspelled",
            json!({ "dataset": { "fileType": "tx_cloud" } }),
        ),
        ("padded", json!({ "dataset": { "fileType": " http" } })),
        ("not-a-map", json!(["dataset"])),
    ] {
        let mut parent = target_payment_definition();
        parent["spec"]["stages"][0]["executor"]["selectableResource"] = value;
        let error = compile_zhixu_hook_plan(&parent)
            .err()
            .unwrap_or_else(|| panic!("{label} selectableResource must be rejected"));
        assert!(
            error.to_string().contains("selectableResource")
                && (label == "not-a-map" || error.to_string().contains("fileType must be one of")),
            "{label}: {error}"
        );
    }

    let mut parent = target_payment_definition();
    parent["spec"]["stages"][0]["executor"]["selectableResource"] = json!({
        "dataset": { "fileType": "plain_text", "plainText": { "content": "x" } }
    });
    compile_zhixu_hook_plan(&parent)
        .unwrap_or_else(|err| panic!("closed-set selectableResource must compile: {err}"));
}

#[test]
fn rejects_spec_with_empty_or_missing_stages() {
    for mutate in ["empty", "missing"] {
        let mut definition = target_payment_definition();
        match mutate {
            "empty" => definition["spec"]["stages"] = json!([]),
            _ => {
                definition["spec"].as_object_mut().unwrap().remove("stages");
            }
        }
        let error = compile_zhixu_hook_plan(&definition).expect_err("stages-less spec must fail");
        assert!(
            error
                .to_string()
                .contains("must contain at least one stage"),
            "{mutate}: {error}"
        );
        let error = compile_cloud_artifact(&definition)
            .expect_err("cloud target must reject the same shape");
        assert!(
            error
                .to_string()
                .contains("must contain at least one stage"),
            "{mutate} cloud: {error}"
        );
    }
}

#[test]
fn cloud_target_rejects_send_signal_violations_like_hook_plan() {
    let mut empty = target_payment_definition();
    empty["spec"]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "" }));
    for target in ["hook_plan", "cloud"] {
        let error = (if target == "hook_plan" {
            compile_zhixu_hook_plan(&empty)
        } else {
            compile_cloud_artifact(&empty)
        })
        .expect_err("empty sendSignal must fail");
        assert!(error.to_string().contains("D026"), "{target}: {error}");
    }

    let mut duplicate = target_payment_definition();
    duplicate["spec"]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "str" }));
    for target in ["hook_plan", "cloud"] {
        let error = (if target == "hook_plan" {
            compile_zhixu_hook_plan(&duplicate)
        } else {
            compile_cloud_artifact(&duplicate)
        })
        .expect_err("duplicate sendSignal must fail");
        assert!(
            error.to_string().contains("duplicate capability"),
            "{target}: {error}"
        );
    }
}

#[test]
fn send_signal_declarations_use_a_single_exact_surface() {
    for (label, signal) in [
        ("leading whitespace bare", " str"),
        ("target separator form", "Seller::NOTED"),
        ("trailing whitespace target", "buyer::cmp "),
        ("whitespace inside target", "buy er::cmp"),
        ("double separator", "a::b::c"),
        ("empty signal half", "buyer::"),
        ("empty target half", "::cmp"),
        ("digit-leading bare", "1abc"),
        ("digit-leading target", "1buyer::cmp"),
        ("three-part canonical", "a.b.c"),
        ("four-part canonical", "a.b.c.d"),
    ] {
        let mut definition = target_payment_definition();
        definition["spec"]["stages"][0]["sendSignals"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "name": signal }));
        let error = compile_zhixu_hook_plan(&definition)
            .expect_err(&format!("{label} ({signal:?}) must be rejected"));
        assert!(
            error.to_string().contains(".sendSignals"),
            "{label} ({signal:?}): {error}"
        );
    }

    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": " str" }));
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("whitespace-padded signal must be rejected by the shape gate");
    assert!(
        !error.to_string().contains("duplicate capability"),
        "whitespace variant must fail on charset, not duplicate: {error}"
    );
}

#[test]
fn canonical_send_signals_are_explicit_self_references() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["sendSignals"] = json!([{ "name": "init.str" }]);
    let plan = compile_zhixu_hook_plan(&definition)
        .expect("canonical self-reference declaration compiles");
    let capability = plan["signalCapabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capability| capability["declaredSignal"] == json!("init.str"))
        .expect("capability carries the canonical declaration");
    assert_eq!(capability["targetSignalName"], json!("init.str"));
    assert_eq!(capability["targetSource"], json!("payment"));
    assert_eq!(capability["targetOrderRelation"], json!("current"));

    let mut dual = target_payment_definition();
    dual["spec"]["stages"].as_array_mut().unwrap().push(json!({
        "name": "echo",
        "source": "payment",
        "receiveSignals": { "GO": "payment::init.str" },
        "sendSignals": [{ "name": "init.str" }],
        "executor": { "supplierType": "organization", "supplierID": "payment-gateway" }
    }));
    let error = compile_zhixu_hook_plan(&dual)
        .expect_err("canonical declaration addressing another stage must fail");
    assert!(
        error
            .to_string()
            .contains("does not address the declaring stage"),
        "unexpected error: {error}"
    );
    let error = compile_cloud_artifact(&dual).expect_err("cloud target must reject the same shape");
    assert!(
        error
            .to_string()
            .contains("does not address the declaring stage"),
        "cloud: {error}"
    );

    let mut both = target_payment_definition();
    both["spec"]["stages"][0]["sendSignals"] = json!([{ "name": "str" }, { "name": "init.str" }]);
    let error =
        compile_zhixu_hook_plan(&both).expect_err("bare + canonical self-reference must collide");
    assert!(
        error.to_string().contains("duplicate capability"),
        "unexpected error: {error}"
    );

    let mut target = target_payment_definition();
    target["spec"]["stages"][0]["sendSignals"] = json!([{ "name": "init.str" }]);
    compile_zhixu_hook_plan(&target)
        .expect("D014 resolves canonical declarations by expanded full name");
}

#[test]
fn nucleation_id_must_be_non_blank() {
    for blank in ["", "   ", "\t"] {
        let mut definition = target_payment_definition();
        definition["spec"]["nucleation"]["id"] = json!(blank);
        let error =
            compile_zhixu_hook_plan(&definition).expect_err("blank nucleation.id must fail");
        assert!(
            error
                .to_string()
                .contains("spec.nucleation.id must be non-empty"),
            "unexpected error: {error}"
        );
        let error = compile_cloud_artifact(&definition)
            .expect_err("cloud target must reject the same shape");
        assert!(
            error
                .to_string()
                .contains("spec.nucleation.id must be non-empty"),
            "cloud: {error}"
        );
    }
    let mut missing = target_payment_definition();
    missing["spec"]
        .as_object_mut()
        .unwrap()
        .remove("nucleation");
    let error = compile_zhixu_hook_plan(&missing)
        .expect_err("missing nucleation must fail at the serde required-field gate");
    assert!(
        error.to_string().contains("nucleation"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_executor_without_supplier_id_even_when_selected_stages_anchored() {
    let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "supplier_id_required" },
            "spec": {
                "platform": { "type": "evm" },
                "nucleation": { "id": "core" },
                "stages": [{
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["main"],
                            "sendSignals": [{ "name": "executor_selected" }],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        },
    {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": {
                                "GO": "buyer::assign.executor_selected"
                            },
                            "executor": { "supplierType": "organization" }
                        }]
            }
        });
    let error =
        compile_zhixu_hook_plan(&definition).expect_err("executor without supplierID must fail");
    assert!(
        error.to_string().contains("supplierID is required"),
        "unexpected error: {error}"
    );
    let error = compile_cloud_artifact(&definition)
        .expect_err("cloud target must enforce the same requirement");
    assert!(
        error.to_string().contains("supplierID is required"),
        "unexpected cloud error: {error}"
    );
}

#[test]
fn rejects_receive_hooks_on_stage_without_static_executor() {
    let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "unmaterializable_watcher" },
            "spec": {
                "platform": { "type": "evm" },
                "nucleation": { "id": "core" },
                "stages": [{
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["main"],
                            "sendSignals": [{ "name": "executor_selected" }],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        },
    {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": {
                                "OBS": "buyer::assign.executor_selected"
                            }
                        }]
            }
        });

    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("flags=0 watcher on a selectedStages-only stage must fail");
    assert!(
        error
            .to_string()
            .contains("can never materialize the stage"),
        "unexpected error: {error}"
    );

    compile_cloud_artifact(&definition)
        .expect("cloud target must not enforce on-chain materialization");
}

#[test]
fn rejects_receive_signal_keys_with_separators() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["receiveSignals"]["BAD.KEY"] = json!("payment::init.execute");
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("receiveSignals key containing '.' must fail");
    let message = error.to_string();
    assert!(
        message.contains("receiveSignals.BAD.KEY")
            && message.contains("must not contain '.', '#' or whitespace"),
        "unexpected error: {message}"
    );

    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["receiveSignals"]["BAD#KEY"] = json!("payment::init.execute");
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("receiveSignals key containing '#' must fail");
    assert!(
        error
            .to_string()
            .contains("must not contain '.', '#' or whitespace"),
        "unexpected error: {error}"
    );

    let mut definition = target_payment_definition();
    definition["spec"]["stages"][0]["receiveSignals"]["BAD KEY"] = json!("payment::init.execute");
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("receiveSignals key containing whitespace must fail");
    assert!(
        error
            .to_string()
            .contains("must not contain '.', '#' or whitespace"),
        "unexpected error: {error}"
    );
}

fn counted_definition(stages: Vec<Value>) -> Value {
    json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "counted" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": stages
        }
    })
}

#[test]
fn error_string_is_truncated_at_the_boundary() {
    let stages = (0..256)
        .map(|index| {
            json!({
                "name": format!("s{index:03}"),
                "source": "bad source",
                "receiveSignals": { "PUBLISH": "src::main.seed" },
                "sendSignals": [{ "name": "seed" }],
                "executor": { "supplierType": "organization", "supplierID": "counter" }
            })
        })
        .collect::<Vec<_>>();
    let definition = counted_definition(stages);
    let error =
        compile_zhixu_hook_plan(&definition).expect_err("256 invalid-source stages must fail");
    let message = error.to_string();
    assert!(
        message.contains("issues truncated: error string capped at 16384 bytes"),
        "{message}"
    );
    assert!(
        message.len() <= crate::MAX_ISSUES_STRING_BYTES + 200,
        "truncated error string must stay bounded: {}",
        message.len()
    );
}

const SETTLE_ADMISSION: &str = "payment::init.str & ~(control.cxl +14d)";

#[test]
fn admissions_compile_into_both_targets() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] = json!([SETTLE_ADMISSION]);
    let plan =
        compile_zhixu_hook_plan(&definition).expect("gated sendSignal compiles for hook_plan");
    let admissions = plan["admissions"].as_array().unwrap();
    assert_eq!(
        admissions.len(),
        1,
        "only the validWhen-bearing entry admits: {admissions:?}"
    );
    let admission = &admissions[0];
    assert_eq!(admission["stageIdentifier"], json!("settle"));
    assert_eq!(admission["signalName"], json!("settle.cmp"));
    assert_eq!(admission["rawExpression"], json!([SETTLE_ADMISSION]));
    assert_eq!(
        admission["normalizedExpression"],
        json!("payment::init.str&~(control.cxl+14d)")
    );
    assert!(
        admission["ast"].is_object(),
        "hook_plan carries the parsed AST"
    );
    assert_eq!(
        admissions[0]["dependencies"],
        json!([
            { "kind": "negative", "source": "payment", "signalName": "control.cxl" },
            { "kind": "positive", "source": "payment", "signalName": "init.str" },
        ]),
        "hook_plan carries the full dependency form: {admission:?}"
    );

    let cloud =
        compile_cloud_artifact(&definition).expect("cloud target compiles the same admission");
    let cloud_admissions = cloud["admissions"].as_array().unwrap();
    assert_eq!(cloud_admissions.len(), 1);
    let cloud_admission = &cloud_admissions[0];
    assert_eq!(cloud_admission["signalName"], json!("settle.cmp"));
    assert_eq!(
        cloud_admission["cloudAst"]["schemaVersion"],
        json!("uvp.cloudAst.v4")
    );
    assert_eq!(
        cloud_admission["dependencies"],
        json!([
            { "signalName": "control.cxl", "dependencyKind": "negative" },
            { "signalName": "init.str", "dependencyKind": "positive" },
        ]),
        "cloud dependencies drop the timer dimension: {cloud_admission:?}"
    );
}

#[test]
fn admission_self_reference_is_rejected_in_both_targets() {
    for target in ["hook_plan", "cloud"] {
        for header in ["payment", "seller"] {
            let mut definition = target_payment_definition();
            definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
                json!([format!("{header}::settle.cmp & init.str")]);
            let result = if target == "hook_plan" {
                compile_zhixu_hook_plan(&definition)
            } else {
                compile_cloud_artifact(&definition)
            };
            let error = result.expect_err("self-referencing validWhen must fail");
            assert!(
                error.to_string().contains(
                    "D028 settle.sendSignals[cmp].validWhen[0]: expression addresses the declaring signal itself (settle.cmp)"
                ),
                "{target}/{header}: {error}"
            );
        }
    }

    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["seller::settle.cmp & init.str"]);
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("a same-named fact under another source class still addresses the declaring signal's fact key");
    assert!(
        error
            .to_string()
            .contains("D028 settle.sendSignals[cmp].validWhen[0]"),
        "{error}"
    );
}

#[test]
fn admission_dangling_references_are_rejected() {
    for (label, expression, needle) in [
        (
            "unknown stage",
            "payment::nothere.nosignal",
            "references unknown stage nothere",
        ),
        (
            "undeclared signal",
            "payment::init.never_declared",
            "references unknown signal init.never_declared",
        ),
        (
            "undeclared source",
            "ghost::init.str",
            "is not a declared source in this zhixu",
        ),
    ] {
        for target in ["hook_plan", "cloud"] {
            let mut definition = target_payment_definition();
            definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] = json!([expression]);
            let result = if target == "hook_plan" {
                compile_zhixu_hook_plan(&definition)
            } else {
                compile_cloud_artifact(&definition)
            };
            let error = result.expect_err(&format!("{label} ({target}) must be rejected"));
            assert!(
                error.to_string().contains(".sendSignals[cmp].validWhen[0]"),
                "{label} ({target}): {error}"
            );
            assert!(
                error.to_string().contains(needle),
                "{label} ({target}): {error}"
            );
        }
    }
}

#[test]
fn admission_name_blank_expression_duplicate_and_unknown_key_faces() {
    let mut empty_name = target_payment_definition();
    empty_name["spec"]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "" }));
    let error =
        compile_zhixu_hook_plan(&empty_name).expect_err("an entry without a name must fail");
    assert!(
        error
            .to_string()
            .contains("D026 init.sendSignals: name is required"),
        "{error}"
    );

    let mut blank = target_payment_definition();
    blank["spec"]["stages"][0]["sendSignals"][0]["validWhen"] = json!(["   "]);
    let error =
        compile_cloud_artifact(&blank).expect_err("a blank validWhen item must fail loudly");
    assert!(
        error
            .to_string()
            .contains("D027 init.sendSignals[str].validWhen[0]: must be a non-blank item"),
        "{error}"
    );

    let mut empty_array = target_payment_definition();
    empty_array["spec"]["stages"][0]["sendSignals"][0]["validWhen"] = json!([]);
    let error = compile_cloud_artifact(&empty_array)
        .expect_err("an empty validWhen array must fail loudly (drop the key instead)");
    assert!(
        error.to_string().contains(
            "D027 init.sendSignals[str].validWhen: must be a non-empty array; drop the key to declare unconditional admission"
        ),
        "{error}"
    );

    let mut duplicate = target_payment_definition();
    duplicate["spec"]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "init.str" }));
    let error = compile_zhixu_hook_plan(&duplicate)
        .expect_err("bare + canonical self-reference must collide on the expanded capability");
    assert!(
        error.to_string().contains("D031") && error.to_string().contains("duplicate capability"),
        "{error}"
    );

    let mut unknown_key = target_payment_definition();
    unknown_key["spec"]["stages"][0]["sendSignals"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "str", "validwhen": SETTLE_ADMISSION }));
    let error = compile_zhixu_hook_plan(&unknown_key)
        .expect_err("a misspelled key must fail at decode, not fold to the zero value");
    assert!(
        error.to_string().contains("unknown field") && error.to_string().contains("validwhen"),
        "{error}"
    );
}

#[test]
fn admission_invalid_expression_reports_the_declaring_signal() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["payment::a.settle.cmp"]);
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("a three-part signal name inside validWhen must fail");
    assert!(
        error
            .to_string()
            .contains("settle.sendSignals[cmp].validWhen[0] is invalid")
            && error.to_string().contains("stage.signal"),
        "{error}"
    );
}

#[test]
fn multi_item_admission_composes_one_and_root_and_unions_dependencies() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["payment::init.str", "payment::~(control.cxl +14d)",]);
    let cloud = compile_cloud_artifact(&definition)
        .expect("a two-item validWhen compiles into one admission");
    let admissions = cloud["admissions"].as_array().unwrap();
    assert_eq!(
        admissions.len(),
        1,
        "N items still produce one admission row"
    );
    let admission = &admissions[0];
    assert_eq!(
        admission["rawExpression"],
        json!(["payment::init.str", "payment::~(control.cxl +14d)"])
    );
    assert_eq!(
        admission["cloudAst"]["root"],
        json!({
            "type": "and",
            "left": { "type": "signal", "signal": "init.str" },
            "right": {
                "type": "neg",
                "expr": {
                    "type": "delay",
                    "expr": { "type": "signal", "signal": "control.cxl" },
                    "rawDuration": "14d",
                    "durationSeconds": 14 * 24 * 60 * 60
                }
            }
        }),
        "the item roots fold under one and-root: {admission}"
    );
    assert_eq!(
        admission["dependencies"],
        json!([
            { "signalName": "init.str", "dependencyKind": "positive" },
            { "signalName": "control.cxl", "dependencyKind": "negative" },
        ]),
        "dependencies are the union across items in first-seen order: {admission}"
    );

    let plan = compile_zhixu_hook_plan(&definition).expect("hook_plan composes the same items");
    let plan_admission = &plan["admissions"][0];
    assert_eq!(
        plan_admission["normalizedExpression"],
        json!("payment::init.str&~(control.cxl+14d)")
    );
    assert_eq!(
        plan_admission["ast"]["condition"]["kind"],
        json!("and"),
        "the hook_plan AST carries an and-condition over the item conditions"
    );
}

#[test]
fn multi_item_admission_requires_every_item_ready() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["payment::init.str", "payment::control.cxl",]);
    let cloud = compile_cloud_artifact(&definition).unwrap();
    let ast = cloud["admissions"][0]["cloudAst"].clone();

    let evaluate = |facts: &[&str]| {
        let request = json!({
            "profile": "cloud_compat",
            "gate": "filter",
            "ast": ast,
            "now": "2026-01-01T00:00:00Z",
            "signals": facts
                .iter()
                .map(|signal| {
                    json!({
                        "source": "payment",
                        "signalName": signal,
                        "receivedAt": "2025-12-01T00:00:00Z",
                    })
                })
                .collect::<Vec<_>>(),
        });
        let output = uvp_hook_dsl::eval_compiled_hook_json(&request.to_string());
        let parsed: Value = serde_json::from_str(&output).unwrap();
        parsed["value"]["state"].clone()
    };

    assert_eq!(
        evaluate(&["init.str", "control.cxl"]),
        json!("ready"),
        "admission passes only when every item is ready"
    );
    assert_eq!(
        evaluate(&["init.str"]),
        json!("needs_more"),
        "one unsatisfied item holds the whole admission back (no per-item voting)"
    );
    assert_eq!(
        evaluate(&["control.cxl"]),
        json!("needs_more"),
        "the other single-item subset must not pass as ready either"
    );
}

#[test]
fn admission_items_must_share_one_header_source() {
    let mut definition = target_payment_definition();
    definition["spec"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "receipts",
            "source": "seller",
            "receiveSignals": { "NOTE": "seller::receipts.mark" },
            "sendSignals": [{ "name": "mark" }],
            "executor": { "supplierType": "organization", "supplierID": "receipt-writer" }
        }));
    definition["spec"]["stages"][2]["sendSignals"][0]["validWhen"] =
        json!(["payment::init.str", "seller::receipts.mark",]);
    let error = compile_cloud_artifact(&definition)
        .expect_err("items addressing different header sources must fail");
    assert!(
        error.to_string().contains(
            "D032 settle.sendSignals[cmp].validWhen: every item must address the same header source (payment); item 1 addresses seller"
        ),
        "{error}"
    );
}

#[test]
fn rejects_metadata_name_violating_slug_pattern() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "Bad-Name" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{ "name": "main", "source": "buyer", "executor": { "supplierType": "organization", "supplierID": "org-1" } }]
        }
    });
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("metadata.name outside the slug pattern must fail");
    assert!(
        error
            .to_string()
            .contains("must match ^[a-z][a-z0-9_-]{0,99}$"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_stage_source_exceeding_36_bytes() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "source_too_long" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{ "name": "main", "source": "a".repeat(37), "executor": { "supplierType": "organization", "supplierID": "org-1" } }]
        }
    });
    let error =
        compile_zhixu_hook_plan(&definition).expect_err("stage source over 36 bytes must fail");
    assert!(
        error.to_string().contains("exceeds 36 bytes"),
        "unexpected error: {error}"
    );
    let error = compile_cloud_artifact(&definition)
        .expect_err("cloud target must enforce the same requirement");
    assert!(
        error.to_string().contains("exceeds 36 bytes"),
        "unexpected cloud error: {error}"
    );
}

#[test]
fn rejects_send_signal_exceeding_combined_limit() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "signal_too_long" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{
                    "name": "main",
                    "source": "buyer",
                    "sendSignals": [{ "name": "str" }, { "name": "s".repeat(96) }],
                    "receiveSignals": { "GO": "buyer::main.str" },
                    "executor": { "supplierType": "organization", "supplierID": "org-1" }
                }]
        }
    });
    let error = compile_cloud_artifact(&definition)
        .expect_err("combined sendSignal name over 100 bytes must fail");
    assert!(
        error
            .to_string()
            .contains("exceeds 100 bytes combined (individual_record.signal_name)"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_receive_signal_key_exceeding_36_bytes() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "hook_key_too_long" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{
                    "name": "main",
                    "source": "buyer",
                    "sendSignals": [{ "name": "str" }],
                    "receiveSignals": { "K".repeat(37): "buyer::main.str" },
                    "executor": { "supplierType": "organization", "supplierID": "org-1" }
                }]
        }
    });
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("receiveSignals key over 36 bytes must fail");
    assert!(
        error.to_string().contains("key must be 1-36 bytes"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_selected_stages_duplicate_target() {
    let definition = json!({
            "apiVersion": "uvp/v0",
            "kind": "Zhixu",
            "metadata": { "name": "dup_selected_target" },
            "spec": {
                "platform": { "type": "cloud" },
                "nucleation": { "id": "core" },
                "stages": [{
                            "name": "assign",
                            "source": "buyer",
                            "selectedStages": ["main", "main"],
                            "sendSignals": [{ "name": "executor_selected" }],
                            "executor": { "supplierType": "organization", "supplierID": "selector-org" }
                        },
    {
                            "name": "main",
                            "source": "buyer",
                            "receiveSignals": { "GO": "buyer::assign.executor_selected" },
                            "executor": { "supplierType": "organization", "supplierID": "exec-org" }
                        }]
            }
        });
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("duplicate selectedStages target must fail");
    assert!(
        error.to_string().contains("contains duplicate target main"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_executor_supplier_id_over_uuid_length() {
    let definition = json!({
        "apiVersion": "uvp/v0",
        "kind": "Zhixu",
        "metadata": { "name": "supplier_id_too_long" },
        "spec": {
            "platform": { "type": "cloud" },
            "nucleation": { "id": "core" },
            "stages": [{ "name": "main", "source": "buyer", "executor": { "supplierType": "organization", "supplierID": "a".repeat(37) } }]
        }
    });
    let error = compile_zhixu_hook_plan(&definition)
        .expect_err("executor supplierID over 36 bytes must fail");
    assert!(
        error.to_string().contains("exceeds 36 bytes"),
        "unexpected error: {error}"
    );
    let error = compile_cloud_artifact(&definition)
        .expect_err("cloud target must enforce the same requirement");
    assert!(
        error.to_string().contains("exceeds 36 bytes"),
        "unexpected cloud error: {error}"
    );
}
