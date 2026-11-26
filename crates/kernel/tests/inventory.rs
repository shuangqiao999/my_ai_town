use crate::common;
use kernel::{KernelError, World};
use protocol::{GroupStatus, InventoryAction as Op};
use serde_json::{json, Value};
fn world() -> World {
    common::world_with(
        common::two_places(json!([])),
        420.0,
        &[("a", "甲", "loc_a"), ("b", "乙", "loc_a")],
    )
}
fn put(w: &mut World, id: &str, mut d: Value) {
    if d.get("id").is_none() {
        d["id"] = json!("player:flower");
    }
    d["version"] = json!(1);
    if d.get("name").is_none() {
        d["name"] = json!("野花");
    }
    if d.get("actions").is_none() {
        d["actions"] = json!([]);
    }
    let def: kernel::props::Definition = serde_json::from_value(d).unwrap();
    w.place_prop(serde_json::from_value(json!({"instance":{"id":id,"definition":def.reference(),"locale":"loc_a"},"definition":def})).unwrap()).unwrap();
}
fn flower(w: &mut World, id: &str) {
    put(w, id, json!({"portable":true,"stack_limit":5}));
}
fn submit(w: &mut World, who: &str, key: &str, d: Value) -> Result<(), KernelError> {
    common::run(w, who, key, d).map(|_| ())
}
fn transfer(w: &mut World, who: &str, key: &str, op: Op) -> Result<(), KernelError> {
    submit(
        w,
        who,
        key,
        json!({"intent":{"verb":op.label()},"inventory":op}),
    )
}
fn finish(w: &mut World, who: &str, key: &str) {
    common::finish(w, who, key);
    assert_eq!(
        w.action_group(who, key).unwrap().status,
        GroupStatus::Succeeded,
        "{:?}",
        w.action_group(who, key)
    );
}
fn pick(w: &mut World, id: &str) {
    transfer(
        w,
        "a",
        &format!("pick-{id}"),
        Op::PickUp {
            items: vec![id.into()],
        },
    )
    .unwrap();
    finish(w, "a", &format!("pick-{id}"));
}
/// 物品还在地图上（没被拿走、存起或消耗）。
fn on_map(w: &World, id: &str) -> bool {
    w.prop_views().iter().any(|p| p.id == id && p.on_map)
}
fn snapshot(w: &World) -> Value {
    serde_json::to_value(w.snapshot()).unwrap()
}
fn restored(w: &World) -> World {
    common::restore(w, world())
}
fn source(w: &mut World) {
    put(
        w,
        "source",
        json!({"id":"player:supply","name":"补给箱","actions":[{"id":"eat","label":"吃干粮","duration_minutes":2,"on_complete":[{"kind":"restore","need":"fullness","amount":25}],"supply":{"initial":10,"cost":1},"portion_name":"一份干粮","recipient":true}]}),
    );
}
#[test]
fn generic_items_stack_and_transfer_identity_while_fixed_furniture_is_rejected() {
    let mut w = world();
    for i in 0..6 {
        flower(&mut w, &format!("f{i}"));
    }
    put(&mut w, "bed", json!({"id":"player:bed","name":"床"}));
    assert!(transfer(
        &mut w,
        "a",
        "bed",
        Op::PickUp {
            items: vec!["bed".into()]
        }
    )
    .is_err());
    for i in 0..6 {
        pick(&mut w, &format!("f{i}"));
    }
    assert_eq!(w.inventory_view("a").used, 2);
    assert!(!on_map(&w, "f0"));
    assert!(transfer(
        &mut w,
        "b",
        "steal",
        Op::PickUp {
            items: vec!["f0".into()]
        }
    )
    .is_err());
    submit(
        &mut w,
        "a",
        "go",
        json!({"intent":{"verb":"前往","locale":"loc_b"}}),
    )
    .unwrap();
    finish(&mut w, "a", "go");
    transfer(
        &mut w,
        "a",
        "drop",
        Op::Drop {
            items: vec!["f0".into(), "f1".into()],
        },
    )
    .unwrap();
    finish(&mut w, "a", "drop");
    assert!(on_map(&w, "f0"));
    assert_eq!(w.topo.prop("f0").unwrap().locale, "loc_b");
    let mut r = restored(&w);
    assert_eq!(snapshot(&w), snapshot(&r));
    transfer(
        &mut r,
        "a",
        "again",
        Op::PickUp {
            items: vec!["f0".into()],
        },
    )
    .unwrap();
    finish(&mut r, "a", "again");
    assert!(r.inventory().carried_by("f0", "a"));
    assert_eq!(r.topo.props().iter().filter(|p| p.id == "f0").count(), 1);
}
#[test]
fn supply_take_and_direct_use_share_stock_and_portable_effects_work_elsewhere() {
    let mut w = world();
    source(&mut w);
    let op = Op::Take {
        source: "source".into(),
        action: "eat".into(),
        quantity: 3,
    };
    transfer(&mut w, "a", "take", op).unwrap();
    w.step(0.4);
    let mut w = restored(&w);
    finish(&mut w, "a", "take");
    assert_eq!(snapshot(&w)["supplies"]["source/eat"], 7);
    let bag = w.inventory_view("a");
    assert_eq!(bag.slots[0].quantity, 3);
    let item = bag.slots[0].items[0].clone();
    submit(
        &mut w,
        "a",
        "go",
        json!({"intent":{"verb":"前往","locale":"loc_b"}}),
    )
    .unwrap();
    finish(&mut w, "a", "go");
    w.set_body_values("a", Some(30.0), None, None, None)
        .unwrap();
    submit(
        &mut w,
        "a",
        "eat",
        json!({"intent":{"verb":"eat"},"prop_id":item}),
    )
    .unwrap();
    finish(&mut w, "a", "eat");
    assert!(w.actor("a").unwrap().needs.fullness.value > 50.0);
    assert!(!w.inventory().carried_by(&item, "a"));
    assert!(!on_map(&w, &item));
    submit(
        &mut w,
        "b",
        "onsite",
        json!({"intent":{"verb":"eat"},"prop_id":"source"}),
    )
    .unwrap();
    finish(&mut w, "b", "onsite");
    assert_eq!(snapshot(&w)["supplies"]["source/eat"], 6);
    assert_eq!(w.inventory_view("a").slots[0].quantity, 2);
    assert_eq!(snapshot(&restored(&w)), snapshot(&w));
}

/// 供餐时段能打包一份饭，带走后不在饭点也能吃。
#[test]
fn a_meal_portion_can_be_taken_during_the_window_and_eaten_later() {
    let mut w = world();
    w.topo.meals = vec![kernel::topology::MealWindow {
        name: "午饭".into(),
        start: 360.0,
        end: 480.0,
        menu: "米饭".into(),
    }];
    put(
        &mut w,
        "meal",
        json!({"id":"player:meal","name":"餐桌","actions":[{"id":"eat","label":"吃饭","duration_minutes":30,"completion":"meal","conditions":["meal_window"],"on_complete":[{"kind":"restore","need":"fullness","amount":50}],"supply":{"initial":10,"cost":1},"recipient":true,"portion_name":"一份饭"}]}),
    );
    transfer(
        &mut w,
        "a",
        "take-meal",
        Op::Take {
            source: "meal".into(),
            action: "eat".into(),
            quantity: 1,
        },
    )
    .unwrap();
    finish(&mut w, "a", "take-meal");
    assert_eq!(snapshot(&w)["supplies"]["meal/eat"], 9);
    let item = w.inventory_view("a").slots[0].items[0].clone();

    w.topo.meals.clear();
    w.set_body_values("a", Some(30.0), None, None, None)
        .unwrap();
    submit(
        &mut w,
        "a",
        "eat-portion",
        json!({"intent":{"verb":"吃饭"},"prop_id":item}),
    )
    .unwrap();
    finish(&mut w, "a", "eat-portion");
    assert!(w.actor("a").unwrap().needs.fullness.value > 50.0);
    assert!(!w.inventory().carried_by(&item, "a"));
}

/// 药箱也能领取一份，带走后治好伤。
#[test]
fn a_medical_portion_can_be_taken_and_used_later() {
    let mut w = world();
    put(
        &mut w,
        "kit",
        json!({"id":"player:kit","name":"救助药箱","actions":[{"id":"bandage","label":"包扎伤口","duration_minutes":5,"completion":"activity","conditions":[],"on_complete":[{"kind":"cure_condition","condition":"injury"}],"recipient":true,"supply":{"initial":5,"cost":1},"portion_name":"一卷绷带"}]}),
    );
    w.apply_condition("a", protocol::BodyConditionKind::Injury, 1.0, "摔伤")
        .unwrap();
    transfer(
        &mut w,
        "a",
        "take-kit",
        Op::Take {
            source: "kit".into(),
            action: "bandage".into(),
            quantity: 1,
        },
    )
    .unwrap();
    finish(&mut w, "a", "take-kit");
    let item = w.inventory_view("a").slots[0].items[0].clone();
    submit(
        &mut w,
        "a",
        "bandage",
        json!({"intent":{"verb":"包扎伤口"},"prop_id":item}),
    )
    .unwrap();
    finish(&mut w, "a", "bandage");
    assert!(w.actor("a").unwrap().needs.active_conditions().is_empty());
    assert!(!w.inventory().carried_by(&item, "a"));
}

/// 共用补给正在被一个人吃喝时，另一个人也能领取一份，不必等对方用完。
#[test]
fn portions_can_be_taken_while_someone_is_eating_from_the_same_supply() {
    let mut w = world();
    source(&mut w);
    common::run(
        &mut w,
        "b",
        "eat-b",
        json!({"intent":{"verb":"吃干粮"},"prop_id":"source"}),
    )
    .unwrap();
    w.step(0.1);
    assert!(w.prop_occupants("source").contains(&"b".to_string()));

    transfer(
        &mut w,
        "a",
        "take-busy",
        Op::Take {
            source: "source".into(),
            action: "eat".into(),
            quantity: 1,
        },
    )
    .unwrap();
    finish(&mut w, "a", "take-busy");
    assert_eq!(snapshot(&w)["supplies"]["source/eat"], 9);
    assert_eq!(w.inventory_view("a").slots[0].quantity, 1);
}

#[test]
fn a_full_bag_says_what_to_drop_and_how_to_make_room() {
    let mut w = world();
    for i in 0..8 {
        put(
            &mut w,
            &format!("bag{i}"),
            json!({"name":format!("物品{i}"),"portable":true,"stack_limit":1}),
        );
        pick(&mut w, &format!("bag{i}"));
    }
    put(
        &mut w,
        "bag8",
        json!({"name":"第九件","portable":true,"stack_limit":1}),
    );
    let error = transfer(
        &mut w,
        "a",
        "full-bag",
        Op::PickUp {
            items: vec!["bag8".into()],
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("背包已满"), "{error}");
    assert!(error.contains("丢弃或存放"), "{error}");
    assert!(error.contains("物品0"), "{error}");
}

#[test]
fn an_occupied_prop_receipt_names_the_user_and_the_way_out() {
    let mut w = world();
    source(&mut w);
    common::run(
        &mut w,
        "b",
        "eat-first",
        json!({"intent":{"verb":"吃干粮"},"prop_id":"source"}),
    )
    .unwrap();
    w.step(0.1);
    common::run(
        &mut w,
        "a",
        "eat-second",
        json!({"intent":{"verb":"吃干粮"},"prop_id":"source"}),
    )
    .unwrap();
    w.step(0.1);
    let group = w.action_group("a", "eat-second").unwrap();
    let error = group
        .results
        .iter()
        .find_map(|result| result.reason.clone())
        .unwrap_or_default();
    assert!(error.contains("正被其他村民使用"), "{group:?}");
    assert!(error.contains("乙"), "{group:?}");
    assert!(error.contains("等一会"), "{group:?}");
}
