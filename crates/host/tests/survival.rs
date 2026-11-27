//! 生存参数的启动路径：地图 survival 决定新居民默认值，旧默认值随读档迁移，改过的值保留。
use crate::support::*;
use serde_json::{json, Value};

fn legacy_tuning() -> Value {
    json!({
        "fullness_decay": 0.05,
        "energy_decay": 0.025,
        "labor_rate": 0.04,
        "voiced": 60.0,
        "ring": 35.0,
        "ring_reset": 45.0,
        "collapse": 15.0
    })
}

#[test]
fn old_default_tuning_migrates_but_custom_values_stay() {
    let mut town = Town::new();
    let mut ws = town.start();
    let old = join_id(&mut ws, "tpl_a");
    let custom = join_id(&mut ws, "tpl_b");
    town.shutdown();
    drop(ws);

    let mut save: Value = read_json(&town.save());
    let mut custom_tuning = legacy_tuning();
    custom_tuning["fullness_decay"] = json!(0.045);
    save["world"]["actors"][&old]["needs"]["tuning"] = legacy_tuning();
    save["world"]["actors"][&custom]["needs"]["tuning"] = custom_tuning.clone();
    save["population"]["residents"][&old]["needs"] = legacy_tuning();
    save["population"]["residents"][&custom]["needs"] = custom_tuning;
    std::fs::write(town.save(), serde_json::to_vec(&save).unwrap()).unwrap();

    let mut ws = town.start();
    until(&mut ws, |m| m["type"] == "roster");
    town.shutdown();
    drop(ws);

    let save: Value = read_json(&town.save());
    let migrated = &save["world"]["actors"][&old]["needs"]["tuning"];
    assert_eq!(migrated["fullness_decay"], 0.04);
    assert_eq!(migrated["voiced"], 50.0);
    assert_eq!(migrated["ring"], 25.0);
    assert_eq!(migrated["ring_reset"], 35.0);
    assert_eq!(migrated["collapse"], 10.0);
    assert_eq!(
        save["population"]["residents"][&old]["needs"]["voiced"],
        50.0
    );

    let kept = &save["world"]["actors"][&custom]["needs"]["tuning"];
    assert_eq!(kept["fullness_decay"], 0.045);
    assert_eq!(kept["voiced"], 60.0);
    assert_eq!(kept["ring"], 35.0);
    assert_eq!(
        save["population"]["residents"][&custom]["needs"]["fullness_decay"],
        0.045
    );
}

#[test]
fn a_scarce_map_sets_the_restore_scale_for_new_residents() {
    let mut town = Town::new();
    let mut doc = read_json(&town.dir.join(MAP_2D));
    doc["survival"] = json!("scarce");
    doc["content_hash"] = json!(host::maps::hash(&doc));
    town.write(MAP_2D, doc);
    town.args = map_args(MAP_2D);

    let mut ws = town.start();
    let id = join_id(&mut ws, "tpl_a");
    town.shutdown();
    drop(ws);

    let save: Value = read_json(&town.save());
    let tuning = &save["world"]["actors"][&id]["needs"]["tuning"];
    assert_eq!(tuning["restore_scale"], 0.4);
    assert_eq!(tuning["fullness_decay"], 0.04);
    assert!(tuning.get("water_decay").is_none());
}
