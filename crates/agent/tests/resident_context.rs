use crate::support::*;
use agent::{
    budget::Limits,
    context::{self, input::ITEM_BYTES},
    conversation::{estimate_tokens, wire_bytes, Message, ModelError, Role},
    runtime::{AgentRequest, ResidentRuntime},
};
use kernel::{needs::NeedValues, World};
use serde_json::{json, Value};

fn spacious() -> Limits {
    Limits {
        tokens_per_day: 100_000_000,
        total_tokens: 1_000_000_000,
        calls_per_day: 10_000,
        ..Limits::default()
    }
}
fn bounded(q: &AgentRequest, r: &ResidentRuntime) {
    assert!(context::paired(&q.messages));
    let estimate = estimate_tokens(
        wire_bytes(&q.messages) + json!(q.tools).to_string().len() as u64,
        r.state().bytes_per_token,
    );
    assert!(
        estimate <= r.state().limits.input_budget(),
        "request estimate {estimate}"
    );
    assert!(q
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .all(|m| m.content.len() <= ITEM_BYTES));
}

#[test]
fn oversized_results_and_old_rounds_preserve_goals_sources_and_restart_continuity() {
    growing_map_and_active_chat_keep_bounded_views_and_a_stable_prefix();
    let mut w = world();
    w.enable_agent("r1").unwrap();
    let original = "阅读来源不能因压缩而丢失。".repeat(250);
    read_board(&mut w, "r1", "long-book", &original);
    let event = w
        .actor("r1")
        .unwrap()
        .journal
        .events_after(0)
        .iter()
        .find(|e| e.kind == protocol::FactKind::Readable)
        .unwrap()
        .event_id;
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    reply(
        &mut r,
        &mut w,
        &q,
        "set_goal",
        json!({"id":"promise","title":"看公告","condition":{"kind":"action","verb":"阅读公告","locale":"loc_a"},"status":"active","commitment":"读完后告诉小禾原文说了什么"}),
    );
    let q = life_request(&mut r, &mut w);
    reply(
        &mut r,
        &mut w,
        &q,
        "read_content",
        json!({"event_id":event}),
    );
    let result: Value =
        serde_json::from_str(&r.session("r1").unwrap().messages.last().unwrap().content).unwrap();
    let source = result["result"]["context_source"]
        .as_u64()
        .expect("large output must have a source");
    assert_eq!(result["ok"], true);
    assert!(
        r.session("r1")
            .unwrap()
            .messages
            .last()
            .unwrap()
            .content
            .len()
            <= ITEM_BYTES
    );
    let mut state = r.state().clone();
    let s = state.sessions.get_mut("r1").unwrap();
    // 旧存档里最后两轮本身就过大，不能依赖“至少保留两轮”。
    let legacy = "旧的工具结果与观察".repeat(15_000);
    s.messages.push(Message::text(Role::User, legacy.clone()));
    s.messages
        .push(Message::text(Role::Assistant, "旧轮次已经结束"));
    let saved_world = serde_json::to_value(w.snapshot()).unwrap();
    let saved_state = serde_json::to_value(state).unwrap();
    let groups = w.actor("r1").unwrap().actions.groups.len();
    let mut w = world();
    w.restore_snapshot(serde_json::from_value(saved_world).unwrap())
        .unwrap();
    let mut r = ResidentRuntime::restore(serde_json::from_value(saved_state).unwrap(), &w).unwrap();
    let mut q = life_request(&mut r, &mut w);
    bounded(&q, &r);
    assert!(q
        .messages
        .last()
        .unwrap()
        .content
        .contains("读完后告诉小禾原文说了什么"));
    assert!(r
        .session("r1")
        .unwrap()
        .archives()
        .values()
        .flatten()
        .any(|m| m.content == legacy));
    let mut reconstructed = String::new();
    let mut offset = 0;
    loop {
        reply(
            &mut r,
            &mut w,
            &q,
            "context_read",
            json!({"source":source,"pointer":"/reading_result/content/text","offset":offset,"limit":700}),
        );
        let result: Value =
            serde_json::from_str(&r.session("r1").unwrap().messages.last().unwrap().content)
                .unwrap();
        assert_eq!(result["ok"], true, "{result}");
        reconstructed.push_str(result["result"]["text"].as_str().unwrap());
        if result["result"]["next_offset"].is_null() {
            break;
        }
        offset = result["result"]["next_offset"].as_u64().unwrap();
        if r.session("r1").unwrap().wake_calls() == 6 {
            r.poll(&mut w, true);
            w.step(31.0);
        }
        q = life_request(&mut r, &mut w);
        bounded(&q, &r);
    }
    assert_eq!(reconstructed, original);
    assert_eq!(w.actor("r1").unwrap().actions.groups.len(), groups);
    assert!(r.session("r1").unwrap().pause_reason().is_none());
    // 持续增长回归：周期等待开启新唤醒，避免测试绕过真实调用额度。
    for _ in 0..40 {
        if r.session("r1").unwrap().wake_calls() == 6 {
            r.poll(&mut w, true);
            w.step(31.0);
        }
        let q = life_request(&mut r, &mut w);
        bounded(&q, &r);
        reply(
            &mut r,
            &mut w,
            &q,
            "wait_for",
            json!({"minutes":1,"reason":"接着观察"}),
        );
        w.step(1.1);
    }
    assert!(r.session("r1").unwrap().pause_reason().is_none());
}

fn growing_map_and_active_chat_keep_bounded_views_and_a_stable_prefix() {
    use protocol::{InteractionCommand as Command, InteractionKind};
    let mut data: Value = serde_json::from_str(include_str!("fixtures/two_places.json")).unwrap();
    data["props"] = json!(world().topo.props());
    for i in 0..100 {
        let loc = format!("loc_{i}");
        let node = format!("n_{i}");
        let edge = format!("edge_{i}");
        data["locales"].as_array_mut().unwrap().extend([
            json!({"id":loc,"name":format!("第{i}个地方"),"kind":"dwell"}),
            json!({"id":edge,"name":"道路","kind":"transit","minutes":1,"from_node":"n_a","to_node":node})
        ]);
        data["stations"].as_array_mut().unwrap().push(json!({"id":format!("st_{i}"),"name":"到达处","locale":loc,"node":node,"kind":"arrival"}));
        data["places"].as_array_mut().unwrap().push(json!({"name":format!("第{i}个地方"),"locale":loc,"arrival_station":format!("st_{i}"),"type":"t","summary":""}));
        data["passages"].as_array_mut().unwrap().extend([
            json!({"id":format!("p_{i}_a"),"a":edge,"b":"loc_a","node_a":"n_a","node_b":"n_a","minutes":0,"capabilities":["通行"]}),
            json!({"id":format!("p_{i}_b"),"a":edge,"b":loc,"node_a":node,"node_b":node,"minutes":0,"capabilities":["通行"]})
        ]);
    }
    for i in 0..200 {
        data["props"].as_array_mut().unwrap().push(json!({"id":format!("prop_{i}"),"name":format!("物品{i}"),"locale":"loc_a","verbs":[{"verb":"查看","duration_minutes":1}]}));
    }
    let topo = kernel::Topology::from_json(&data.to_string(), &catalog()).unwrap();
    let mut w = kernel::World::new(topo, 420.0, need_rules());
    w.spawn("r1", "阿树", "loc_a").unwrap();
    let r = runtime(&mut w, spacious());
    for i in 0..40 {
        w.spawn(&format!("neighbor_{i}"), &format!("邻居{i}"), "loc_a")
            .unwrap();
    }
    w.enable_agent("neighbor_0").unwrap();
    let mut state = r.state().clone();
    state
        .sessions
        .get_mut("r1")
        .unwrap()
        .messages
        .push(Message::text(Role::User, "要归档的旧观察".repeat(20_000)));
    let mut r = ResidentRuntime::restore(state, &w).unwrap();
    let q = request(&mut r, &mut w);
    bounded(&q, &r);
    let body: Value = serde_json::from_str(&q.messages.last().unwrap().content).unwrap();
    assert!(body["observation"]["visible_props_source"].is_object());
    assert!(body["observation"]["people_here_source"].is_object());
    assert!(q.messages[0].content.contains("known_places_source"));
    let event_end = body["observation"]["events"]
        .as_array()
        .unwrap()
        .last()
        .and_then(|e| e["event_id"].as_u64())
        .unwrap_or(r.session("r1").unwrap().cursors.acknowledged);
    assert_eq!(q.stamp.seen_cursor, event_end);
    reply(&mut r, &mut w, &q, "observe", json!({}));
    let next = request(&mut r, &mut w);
    assert_eq!(next.messages[0], q.messages[0], "同一地图前缀必须稳定");
    bounded(&next, &r);
    let body: Value = serde_json::from_str(&next.messages.last().unwrap().content).unwrap();
    assert!(
        body["observation"].get("visible_props").is_none(),
        "没变的慢变字段省略"
    );
    assert!(body["observation"].get("visible_props_source").is_none());
    reply(&mut r, &mut w, &next, "observe", json!({}));
    // 轮到本人发言走交谈轮次：只带本场对话与状态，工具只有 interact，不带地图和道具。
    w.interact(
        "r1",
        "chat",
        w.actor("r1").unwrap().actions.revision,
        &Command::Offer {
            partner_id: "neighbor_0".into(),
            interaction: InteractionKind::Talk,
            message: "聊聊之后的安排".into(),
        },
    )
    .unwrap();
    w.interact(
        "neighbor_0",
        "accept",
        w.actor("neighbor_0").unwrap().actions.revision,
        &Command::Reply {
            interaction_id: "chat".into(),
            accept: true,
            message: "好".into(),
        },
    )
    .unwrap();
    let turn = request(&mut r, &mut w);
    assert_eq!(turn.actor_id, "r1");
    assert_eq!(turn.tools.len(), 1);
    bounded(&turn, &r);
    let body: Value = serde_json::from_str(&turn.messages.last().unwrap().content).unwrap();
    assert_eq!(body["chat"]["id"], "chat");
    assert_eq!(body["chat"]["turn"], "r1");
    assert!(body.get("observation").is_none());
    assert!(!turn.messages[0].content.contains("known_places"));
    // 聊天中漏填 interaction_id 由运行时补上，to 写名字换成编号，不多花一次请求。
    // 心声只是把台词换个说法时不记成想法。
    let say = tool_call(
        "say-1",
        "interact",
        json!({"operation":"say","message":"我们继续刚才的安排","to":"邻居0"}),
    );
    assert!(reply_with(
        &mut r,
        &mut w,
        &turn,
        "那我们就继续刚才的安排。",
        vec![say]
    ));
    assert_ne!(
        w.actor("r1")
            .unwrap()
            .journal
            .thought
            .as_ref()
            .map(|t| t.text.as_str()),
        Some("那我们就继续刚才的安排。")
    );
    assert!(w.chat_turn("neighbor_0"));
    // 下一次轮到本人时，请求以上一轮为前缀，只在末尾追加对方新说的话；自己的话在自己的调用里。
    w.interact(
        "neighbor_0",
        "neighbor-say",
        w.actor("neighbor_0").unwrap().actions.revision,
        &Command::Say {
            interaction_id: "chat".into(),
            message: "好，听你的".into(),
            to: Some("r1".into()),
        },
    )
    .unwrap();
    w.step(1.1);
    let next = request(&mut r, &mut w);
    assert_eq!(next.messages[..turn.messages.len()], turn.messages[..]);
    let added = &next.messages[turn.messages.len()..];
    assert_eq!(
        added.iter().map(|m| m.role).collect::<Vec<_>>(),
        [Role::Assistant, Role::Tool, Role::User]
    );
    let update: Value = serde_json::from_str(&added[2].content).unwrap();
    let heard: Vec<_> = update["chat"]["new_entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    assert_eq!(heard, ["好，听你的"]);
    let mut small = ResidentRuntime::new(Limits {
        context_tokens: 16_000,
        compact_at_tokens: 10_000,
        ..spacious()
    })
    .unwrap();
    assert!(small.register(&mut w, "r1", &"规则".repeat(5_500)).is_err());
    assert!(
        small.session("r1").is_none(),
        "设定校验失败不能登记半份会话"
    );
}

#[test]
fn context_rejections_shrink_requests_persist_limits_and_stop_without_replaying_actions() {
    let mut w = world();
    let r = runtime(&mut w, spacious());
    let mut state = r.state().clone();
    for _ in 0..8 {
        state
            .sessions
            .get_mut("r1")
            .unwrap()
            .messages
            .push(Message::text(Role::User, "历史观察".repeat(450)));
        state
            .sessions
            .get_mut("r1")
            .unwrap()
            .messages
            .push(Message::text(Role::Assistant, "继续"));
    }
    let mut r = ResidentRuntime::restore(state, &w).unwrap();
    let mut q = request(&mut r, &mut w);
    let mut last_bytes = q.stamp.sent_bytes;
    for attempt in 0..3 {
        assert!(r.on_reply(
            &mut w,
            "r1",
            q.stamp.id,
            q.stamp.epoch,
            Err(ModelError::ContextWindowExceeded {
                status: 400,
                detail: "test server".into()
            }),
            1
        ));
        let before = r.session("r1").unwrap().input.ceiling;
        assert!(before.is_some());
        let encoded = serde_json::to_value(r.state()).unwrap();
        r = ResidentRuntime::restore(serde_json::from_value(encoded).unwrap(), &w).unwrap();
        assert_eq!(r.session("r1").unwrap().input.ceiling, before);
        let mut next = r.poll(&mut w, true);
        if next.is_empty() {
            assert!(r.session("r1").unwrap().pause_reason().is_some());
            break;
        }
        assert!(attempt < 2, "拒收后最多重试两次");
        q = next.remove(0);
        assert!(q.stamp.sent_bytes < last_bytes, "不得原样重发超窗请求");
        last_bytes = q.stamp.sent_bytes;
    }
    assert!(r.session("r1").unwrap().pause_reason().is_some());
    assert!(w.actor("r1").unwrap().actions.groups.is_empty());
    let old_calls = r.state().lifetime.calls;
    for _ in 0..5 {
        assert!(r.poll(&mut w, true).is_empty());
    }
    assert_eq!(r.state().lifetime.calls, old_calls);
}

/// 陌生环境：已知地图不在 system 消息里（前缀照样稳定），观察带着记得的地方和眼前出去的路，
/// 没去过的路不写名字；熟悉的世界照旧把全图放在 system 消息里。
#[test]
fn strangers_carry_their_known_map_in_the_observation_instead_of_the_prefix() {
    let mut w = world();
    let mut r = runtime(&mut w, spacious());
    let familiar = request(&mut r, &mut w);
    assert!(familiar.messages[0].content.contains("\"known_places\":"));
    w.set_strangers(true);
    let mut r = runtime(&mut w, spacious());
    let q = request(&mut r, &mut w);
    assert!(!q.messages[0].content.contains("\"known_places\":"));
    let body: Value = serde_json::from_str(&q.messages.last().unwrap().content).unwrap();
    let known = body["observation"]["known_places"].as_array().unwrap();
    assert_eq!(known.len(), 1);
    assert_eq!(known[0]["id"], "loc_a");
    let exits = body["observation"]["exits"].as_array().unwrap();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0]["to"], "loc_b");
    assert!(exits[0]["name"].is_null(), "没去过的路只知道方向");
}

/// 建镇生成的落脚点跟着观察给出，停留时 target 填它就站过去；只列这个地方的。
#[test]
fn spots_of_the_current_place_are_listed_for_staying() {
    let mut data: Value = serde_json::from_str(include_str!("fixtures/two_places.json")).unwrap();
    data["stations"].as_array_mut().unwrap().extend([
        json!({"id":"spot_loc_a_1","locale":"loc_a","name":"水边","kind":"slot"}),
        json!({"id":"spot_loc_b_1","locale":"loc_b","name":"北边","kind":"slot"}),
    ]);
    let mut w = kernel::World::new(
        kernel::Topology::from_json(&data.to_string(), &catalog()).unwrap(),
        420.0,
        need_rules(),
    );
    w.spawn("r1", "阿树", "loc_a").unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = request(&mut r, &mut w);
    let body: Value = serde_json::from_str(&q.messages.last().unwrap().content).unwrap();
    assert_eq!(
        body["observation"]["spots"],
        json!([{"id":"spot_loc_a_1","name":"水边"}])
    );
    assert!(
        body["observation"].get("tide").is_none(),
        "没开玩法时观察里没有玩法那一栏，内置模型的题面不变"
    );
}

#[test]
fn waking_after_a_long_chat_starts_from_what_just_happened() {
    use protocol::{InteractionCommand as Command, InteractionKind};
    // 交谈轮次不推进生活这边的游标；聊得久，聊完醒来要先看到刚发生的事，而不是从最早一句补看
    // （回声湾：到了灯塔还以为在老屋）。更早的只报条数，游标直接到最新。
    let mut w = world();
    w.spawn("neighbor_0", "阿禾", "loc_a").unwrap();
    w.enable_agent("neighbor_0").unwrap();
    let mut r = runtime(&mut w, spacious());
    let revision = |w: &kernel::World, id: &str| w.actor(id).unwrap().actions.revision;
    let offer = Command::Offer {
        partner_id: "neighbor_0".into(),
        interaction: InteractionKind::Talk,
        message: "聊聊老屋".into(),
    };
    w.interact("r1", "chat", revision(&w, "r1"), &offer)
        .unwrap();
    let accept = Command::Reply {
        interaction_id: "chat".into(),
        accept: true,
        message: "好".into(),
    };
    w.interact("neighbor_0", "accept", revision(&w, "neighbor_0"), &accept)
        .unwrap();
    for i in 0..16 {
        let turn = request(&mut r, &mut w);
        let say = tool_call(
            &format!("say-{i}"),
            "interact",
            json!({"operation":"say","message":format!("老屋第{i}件事")}),
        );
        assert!(reply_with(&mut r, &mut w, &turn, "", vec![say]));
        let answer = Command::Say {
            interaction_id: "chat".into(),
            message: format!("听你说第{i}件"),
            to: Some("r1".into()),
        };
        w.interact(
            "neighbor_0",
            &format!("n{i}"),
            revision(&w, "neighbor_0"),
            &answer,
        )
        .unwrap();
        w.step(1.1);
    }
    let leave = Command::Leave {
        interaction_id: "chat".into(),
        message: "我先走了，傍晚灯塔见".into(),
    };
    w.interact("neighbor_0", "bye", revision(&w, "neighbor_0"), &leave)
        .unwrap();
    let q = life_request(&mut r, &mut w);
    let body: Value = serde_json::from_str(&q.messages.last().unwrap().content).unwrap();
    let observation = &body["observation"];
    let events = observation["events"].as_array().unwrap();
    let texts: Vec<_> = events.iter().map(|e| e["text"].as_str().unwrap()).collect();
    let newest = w.actor("r1").unwrap().journal.cursor();
    assert_eq!(events.last().unwrap()["event_id"], newest);
    assert!(texts.iter().any(|t| t.contains("傍晚灯塔见")), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("老屋第0件事")));
    assert!(observation["earlier_events"]["omitted"].as_u64().unwrap() > 0);
    assert!(observation["earlier_events"]["speech"].as_u64().unwrap() > 0);
    assert_eq!(observation["earlier_events"]["with"], json!(["阿禾"]));
    assert!(!observation["earlier_events"]["places"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(q.stamp.seen_cursor, newest, "更早的不再一页页补看");
}

#[test]
fn a_prop_someone_else_is_using_says_so_before_anyone_tries() {
    // 一台咖啡机三个人抢：看不出有人在用，只能试了才知道，每次白花一次模型调用。
    let mut w = world();
    w.spawn("neighbor_0", "阿禾", "loc_a").unwrap();
    w.enable_agent("neighbor_0").unwrap();
    let mut r = runtime(&mut w, spacious());
    let plan: Vec<protocol::ActionDraft> = serde_json::from_value(
        json!([{"prop_id":"board","intent":{"verb":"查看公告栏","duration_minutes":30}}]),
    )
    .unwrap();
    let revision = w.actor("neighbor_0").unwrap().actions.revision;
    w.run_actions("neighbor_0", "look", revision, &plan)
        .unwrap();
    w.step(0.5);
    let q = life_request(&mut r, &mut w);
    let body: Value = serde_json::from_str(&q.messages.last().unwrap().content).unwrap();
    let props = body["observation"]["visible_props"].as_array().unwrap();
    let board = props.iter().find(|p| p["id"] == "board").unwrap();
    let users = board["in_use"].as_str().unwrap();
    assert!(users.starts_with("阿禾正在查看公告栏"), "{users}");
    let stand = props.iter().find(|p| p["id"] == "fixture-board").unwrap();
    assert!(stand.get("in_use").is_none(), "没人用的道具和原来一字不差");
}

fn place(w: &mut World, id: &str, locale: &str, definition: Value) {
    let definition: kernel::props::Definition = serde_json::from_value(definition).unwrap();
    w.place_prop(
        serde_json::from_value(json!({
            "instance":{"id":id,"definition":definition.reference(),"locale":locale},
            "definition":definition
        }))
        .unwrap(),
    )
    .unwrap();
}

/// 在 loc_b 放一个饮用水桶。定义照 fixtures/props.json 的 test:water 抄，补给 200 份。
fn place_water(w: &mut World, id: &str, locale: &str) {
    place_water_supply(w, id, locale, 200);
}

fn place_water_supply(w: &mut World, id: &str, locale: &str, initial: u32) {
    place(
        w,
        id,
        locale,
        json!({
            "id":"test:water","version":1,"name":"饮用水桶","description":"可饮用的清水。",
            "actions":[{"id":"drink","label":"饮水","duration_minutes":3,"during":[],"completion":"activity",
                "conditions":[],"on_complete":[{"kind":"restore","need":"water","amount":35}],
                "recipient":true,"portion_name":"一壶水","supply":{"initial":initial,"cost":1}}]
        }),
    );
}

fn place_meal(w: &mut World, id: &str, locale: &str) {
    place(
        w,
        id,
        locale,
        json!({
            "id":"test:meal","version":1,"name":"饭桌",
            "actions":[{"id":"eat","label":"吃饭","duration_minutes":30,
                "during":[{"kind":"restore_need","need":"fullness","per_minute":1.0,"pause_decay":false}],
                "completion":"meal","conditions":["meal_window"]}]
        }),
    );
}

fn place_kit(w: &mut World, id: &str, locale: &str) {
    place(
        w,
        id,
        locale,
        json!({
            "id":"test:kit","version":1,"name":"救助药箱",
            "actions":[{"id":"bandage","label":"包扎伤口","duration_minutes":5,"during":[],
                "completion":"activity","conditions":[],
                "on_complete":[{"kind":"cure_condition","condition":"injury"}],
                "recipient":true,"supply":{"initial":20,"cost":1}}]
        }),
    );
}

fn observed(q: &AgentRequest) -> Value {
    serde_json::from_str::<Value>(&q.messages.last().unwrap().content).unwrap()["observation"]
        .clone()
}

fn visible_prop(q: &AgentRequest, id: &str) -> Value {
    observed(q)["visible_props"]
        .as_array()
        .unwrap()
        .iter()
        .find(|prop| prop["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("observation 里没有 {id}"))
}

fn thirst() -> NeedValues<Option<f64>> {
    NeedValues {
        water: Some(20.0),
        ..Default::default()
    }
}

/// 渴到紧急档时，观察里要给出本人知道的补水地点：带地点 id、需求和动词，模型据此前往；
/// 熟悉世界的静态地图另有一份不被 known_places 额度截掉的补给索引。
#[test]
fn urgent_thirst_lists_known_water_places_for_the_model() {
    let mut w = world();
    place_water(&mut w, "water_b", "loc_b");
    w.adjust_body("r1", thirst()).unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    assert!(
        q.messages[0].content.contains("\"supply_places\""),
        "静态地图要有补给索引"
    );
    let hints = observed(&q)["survival_hints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let water = hints
        .iter()
        .find(|hint| hint["place"] == "loc_b")
        .expect("要指向乙地的水桶");
    assert_eq!(water["need"], "水");
    assert_eq!(water["prop_id"], "water_b");
    assert_eq!(water["verb"], "饮水");
    assert_eq!(water["gain"], 35);
    assert_eq!(water["remaining"], 200);
    assert_eq!(water["take_action"], "drink");
    assert_eq!(water["portion"], "一壶水");
    assert_eq!(water["minutes"], 2, "甲地到乙地要走两分钟");
}

/// 身体正常时不多写生存线索，题面和加这个字段之前一样。
#[test]
fn a_steady_body_has_no_survival_hints() {
    let mut w = world();
    place_water(&mut w, "water_b", "loc_b");
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    assert!(observed(&q).get("survival_hints").is_none());
}

/// 陌生环境不白送全图补给：只列到过的地方，没去过的乙地不能提前出现。
#[test]
fn strangers_only_see_supplies_in_places_they_know() {
    let mut w = world();
    place_water(&mut w, "water_b", "loc_b");
    w.set_strangers(true);
    w.adjust_body("r1", thirst()).unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    assert!(
        !q.messages[0].content.contains("\"supply_places\""),
        "陌生环境不直接给全图补给索引"
    );
    let hints = observed(&q)["survival_hints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(hints.is_empty(), "没去过的乙地不该出现：{hints:?}");
}

/// 又渴又饿时先找水；带供餐时段的食物在非饭点要标出来，别让模型以为随时有饭。
#[test]
fn urgent_thirst_comes_before_food_and_closed_meals_say_so() {
    let mut w = world();
    place_water(&mut w, "water_b", "loc_b");
    place_meal(&mut w, "meal_b", "loc_b");
    w.topo.meals = vec![kernel::topology::MealWindow {
        name: "午饭".into(),
        start: 690.0,
        end: 810.0,
        menu: "米饭".into(),
    }];
    w.adjust_body(
        "r1",
        NeedValues {
            water: Some(20.0),
            fullness: Some(20.0),
            ..Default::default()
        },
    )
    .unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    let hints = observed(&q)["survival_hints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(hints[0]["need"], "水", "缺水更急，排在最前：{hints:?}");
    let food = hints
        .iter()
        .find(|hint| hint["need"] == "食物")
        .expect("也要给出食物线索");
    assert_eq!(food["open"], false, "早上七点不在午饭时段");
    assert!(food["serving"].as_str().unwrap().contains("午饭"));
}

/// 受伤时给出能治伤的地点，带路程和剩余补给。
#[test]
fn injury_lists_known_treatment_places() {
    let mut w = world();
    place_kit(&mut w, "kit_b", "loc_b");
    w.apply_condition("r1", protocol::BodyConditionKind::Injury, 1.0, "摔伤")
        .unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    let hints = observed(&q)["treatment_hints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let treatment = hints
        .iter()
        .find(|hint| hint["place"] == "loc_b")
        .expect("要指向乙地的药箱");
    assert_eq!(treatment["condition"], "受伤");
    assert_eq!(treatment["prop_id"], "kit_b");
    assert_eq!(treatment["verb"], "包扎伤口");
    assert_eq!(treatment["minutes"], 2);
    assert_eq!(treatment["remaining"], 20);
}

/// 求生决策的自述在日记里单独标成「生理紧急」。
#[test]
fn survival_thoughts_are_tagged_in_the_diary() {
    let mut w = world();
    place_water(&mut w, "water_b", "loc_b");
    w.adjust_body("r1", thirst()).unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    reply(&mut r, &mut w, &q, "observe", json!({}));
    let thought = w.actor("r1").unwrap().journal.thought.as_ref().unwrap();
    assert_eq!(thought.trigger_info.text, "生理紧急");
}

/// 用光的补给点也列出来，但排在能用点后面；模型能看见 remaining:0。
#[test]
fn exhausted_supplies_are_marked_zero_after_usable_ones() {
    let mut w = world();
    place_water(&mut w, "water_b", "loc_b");
    place_water_supply(&mut w, "water_dry", "loc_a", 0);
    w.adjust_body("r1", thirst()).unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    let hints = observed(&q)["survival_hints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(hints[0]["place"], "loc_b", "能用的排前面：{hints:?}");
    let dry = hints
        .iter()
        .find(|hint| hint["prop_id"] == "water_dry")
        .expect("用光的点也要列出");
    assert_eq!(dry["remaining"], 0);
}

/// 眼前能领取的道具要列出 portions；居民据此把一份装进口袋。
#[test]
fn portions_are_listed_on_the_prop_in_observation() {
    let mut w = world();
    place_water(&mut w, "water_a", "loc_a");
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    let portions = visible_prop(&q, "water_a")["portions"].clone();
    assert_eq!(portions[0]["action_id"], "drink");
    assert_eq!(portions[0]["name"], "一壶水");
    assert_eq!(portions[0]["remaining"], 200);
}

/// 食堂动作在观察里直接标出此刻能不能开始，并给供餐时段。
#[test]
fn meal_actions_say_whether_they_can_start_now() {
    let mut w = world();
    place_meal(&mut w, "meal_a", "loc_a");
    w.topo.meals = vec![kernel::topology::MealWindow {
        name: "午饭".into(),
        start: 690.0,
        end: 810.0,
        menu: "米饭".into(),
    }];
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    let eat = visible_prop(&q, "meal_a")["verbs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|verb| verb["verb"] == "吃饭")
        .cloned()
        .unwrap();
    assert_eq!(eat["available"], false, "早上七点还没到午饭");
    assert!(eat["serving"].as_str().unwrap().contains("午饭"));
    reply(&mut r, &mut w, &q, "observe", json!({}));

    w.step(300.0);
    let q = life_request(&mut r, &mut w);
    let eat = visible_prop(&q, "meal_a")["verbs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|verb| verb["verb"] == "吃饭")
        .cloned()
        .unwrap();
    assert_eq!(eat["available"], true, "中午十二点应该能吃饭");
}

#[test]
fn a_failed_prop_action_resends_visible_props() {
    let mut w = world();
    w.enable_agent("r1").unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    assert!(observed(&q).get("visible_props").is_some());
    reply(
        &mut r,
        &mut w,
        &q,
        "run_actions",
        json!({"actions":[{"intent":{"verb":"阅读公告"}}]}),
    );
    let q = life_request(&mut r, &mut w);
    assert!(
        observed(&q).get("visible_props").is_some(),
        "道具提交失败后应重新带出当前道具 id"
    );
}

#[test]
fn three_failed_action_rounds_postpone_before_retrying_the_same_format() {
    let mut w = world();
    w.enable_agent("r1").unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    reply(
        &mut r,
        &mut w,
        &q,
        "run_actions",
        json!({"actions":[{"intent":{"verb":"阅读公告"}}]}),
    );
    let q = life_request(&mut r, &mut w);
    reply(
        &mut r,
        &mut w,
        &q,
        "run_actions",
        json!({"actions":[{"intent":{"verb":"阅读公告"},"prop_id":"missing"}]}),
    );
    let q = life_request(&mut r, &mut w);
    reply(
        &mut r,
        &mut w,
        &q,
        "run_actions",
        json!({"actions":[{"intent":{"verb":"阅读公告"},"prop_id":"bench"}]}),
    );
    let s = r.session("r1").unwrap();
    assert!(s.sleep().is_some(), "连续三次动作失败后应先搁置");
    assert_eq!(s.streaks.action_failures, 0);
}

#[test]
fn a_changed_place_clears_the_action_failure_streak() {
    let mut w = world();
    w.enable_agent("r1").unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    reply(
        &mut r,
        &mut w,
        &q,
        "run_actions",
        json!({"actions":[{"intent":{"verb":"阅读公告"}}]}),
    );
    assert_eq!(r.session("r1").unwrap().streaks.action_failures, 1);

    let revision = w.actor("r1").unwrap().actions.revision;
    let move_draft: Vec<protocol::ActionDraft> = serde_json::from_value(json!([
        {"intent":{"verb":"前往","locale":"loc_b"}}
    ]))
    .unwrap();
    w.run_actions("r1", "move-after-failure", revision, &move_draft)
        .unwrap();
    while w.action_group("r1", "move-after-failure").unwrap().status
        == protocol::GroupStatus::Running
    {
        w.step(1.0);
    }

    let _q = life_request(&mut r, &mut w);
    assert_eq!(r.session("r1").unwrap().streaks.action_failures, 0);
}

#[test]
fn survival_hint_gain_follows_the_scarcity_scale() {
    let mut w = world();
    place_water(&mut w, "water_b", "loc_b");
    w.tune_needs(
        "r1",
        protocol::NeedsTuning {
            restore_scale: 0.4,
            ..kernel::default_tuning()
        },
    )
    .unwrap();
    w.adjust_body("r1", thirst()).unwrap();
    let mut r = runtime(&mut w, spacious());
    let q = life_request(&mut r, &mut w);
    let hints = observed(&q)["survival_hints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let water = hints
        .iter()
        .find(|hint| hint["prop_id"] == "water_b")
        .expect("要指向乙地的水桶");
    assert_eq!(water["gain"], 14);
}
