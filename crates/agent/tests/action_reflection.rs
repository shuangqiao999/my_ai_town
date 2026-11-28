use crate::support::*;
use agent::{budget::Limits, runtime::ResidentRuntime};
use serde_json::{json, Value};

fn reviewing_runtime(w: &mut kernel::World) -> ResidentRuntime {
    let mut r = runtime(w, Limits::default());
    r.set_action_reflection("r1", true);
    r
}

fn move_draft() -> (agent::conversation::ToolCall, serde_json::Value) {
    (
        tool_call(
            "draft-move",
            "run_actions",
            json!({"actions":[{"intent":{"verb":"前往","locale":"loc_b"}}]}),
        ),
        json!({"action_id":"run_actions","action_params":{"actions":[{"intent":{"verb":"前往","locale":"loc_b"}}]}}),
    )
}

fn reflection_result(
    calls: Vec<Value>,
    correction: bool,
    confidence: f64,
) -> agent::conversation::ModelReply {
    model_reply(
        "",
        vec![tool_call(
            "self-review",
            "submit_action_reflection",
            json!({
                "internal_thought": "这一步和眼下的目标一致吗？先想清楚再走。",
                "reflection": {
                    "self_question": "现在就走，是不是最要紧的事？",
                    "predicted_outcome": "会到乙地，按计划推进。",
                    "goal_alignment": "high",
                    "risk": "low",
                    "risk_note": "路上没有明显危险。",
                    "alternative_considered": "先在原地确认目标。",
                    "correction_needed": correction
                },
                "decision": {
                    "calls": calls,
                    "confidence": confidence,
                    "reason": "按角色的优先顺序做。"
                },
                "memory_note": "先确认再行动。"
            }),
        )],
    )
}

fn reflection_call(
    calls: Vec<Value>,
    correction: bool,
    confidence: f64,
) -> agent::conversation::ToolCall {
    reflection_result(calls, correction, confidence)
        .calls
        .into_iter()
        .next()
        .unwrap()
}

fn draft_decision(q: &agent::runtime::AgentRequest) -> Vec<Value> {
    let body: Value = serde_json::from_str(&q.messages.last().unwrap().content).unwrap();
    body["draft"]["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|call| {
            json!({
                "action_id": call["name"],
                "action_params": call["arguments"]
            })
        })
        .collect()
}

#[test]
fn a_world_action_gets_one_confirmation_round_and_then_executes() {
    let mut w = world();
    let mut r = reviewing_runtime(&mut w);
    let q = life_request(&mut r, &mut w);
    let (draft, _) = move_draft();
    assert!(reply_with(&mut r, &mut w, &q, "先去乙地。", vec![draft]));

    let review = request(&mut r, &mut w);
    assert_eq!(
        review.stamp.kind,
        protocol::RequestKind::ActionReflection,
        "生活动作应先进入自省"
    );
    let body: Value = serde_json::from_str(&review.messages.last().unwrap().content).unwrap();
    assert_eq!(body["draft"]["calls"][0]["name"], "run_actions");
    assert!(body["legal_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["action_id"] == "run_actions"));

    assert!(reply_with(
        &mut r,
        &mut w,
        &review,
        "",
        vec![reflection_call(draft_decision(&review), false, 0.9)]
    ));
    w.step(3.0);
    assert_eq!(w.actor("r1").unwrap().locus.locale, "loc_b");
    let reflection = r
        .session("r1")
        .unwrap()
        .last_action_reflection
        .as_ref()
        .expect("角色卡应留下这次自省");
    assert!(reflection.self_question.contains("最要紧"));
}

#[test]
fn a_valid_correction_replaces_the_primary_action() {
    let mut w = world();
    let mut r = reviewing_runtime(&mut w);
    let q = life_request(&mut r, &mut w);
    let (draft, _) = move_draft();
    assert!(reply_with(&mut r, &mut w, &q, "先去乙地。", vec![draft]));
    let review = request(&mut r, &mut w);
    let correction = json!([{
        "action_id": "wait_for",
        "action_params": {"minutes":10,"reason":"先在原地想清楚"}
    }]);
    assert!(reply_with(
        &mut r,
        &mut w,
        &review,
        "",
        vec![reflection_call(
            correction.as_array().unwrap().to_vec(),
            true,
            0.9
        )]
    ));
    w.step(3.0);
    assert_eq!(w.actor("r1").unwrap().locus.locale, "loc_a");
    assert!(r
        .session("r1")
        .unwrap()
        .pending
        .iter()
        .any(|p| matches!(p, agent::session::PendingTool::Wait { .. })));
}

#[test]
fn illegal_or_low_confidence_corrections_fall_back_to_the_draft() {
    for (calls, confidence) in [
        (
            json!([{"action_id":"invented_action","action_params":{}}]),
            0.9,
        ),
        (
            json!([{"action_id":"wait_for","action_params":{"minutes":10,"reason":"等一下"}}]),
            0.2,
        ),
    ] {
        let mut w = world();
        let mut r = reviewing_runtime(&mut w);
        let q = life_request(&mut r, &mut w);
        let (draft, _) = move_draft();
        assert!(reply_with(&mut r, &mut w, &q, "先去乙地。", vec![draft]));
        let review = request(&mut r, &mut w);
        assert!(reply_with(
            &mut r,
            &mut w,
            &review,
            "",
            vec![reflection_call(
                calls.as_array().unwrap().to_vec(),
                true,
                confidence
            )]
        ));
        w.step(3.0);
        assert_eq!(w.actor("r1").unwrap().locus.locale, "loc_b");
    }
}

#[test]
fn wait_only_actions_skip_reflection() {
    let mut w = world();
    let mut r = reviewing_runtime(&mut w);
    let q = life_request(&mut r, &mut w);
    assert!(reply_with(
        &mut r,
        &mut w,
        &q,
        "先等等。",
        vec![tool_call(
            "wait",
            "wait_for",
            json!({"minutes":10,"reason":"先等等"})
        )]
    ));
    assert!(r.poll(&mut w, true).is_empty(), "等待不需要第二次模型请求");
}

#[test]
fn a_reflection_discards_the_draft_when_the_world_changes() {
    let mut w = world();
    let mut r = reviewing_runtime(&mut w);
    let q = life_request(&mut r, &mut w);
    let (draft, _) = move_draft();
    assert!(reply_with(&mut r, &mut w, &q, "先去乙地。", vec![draft]));
    let review = request(&mut r, &mut w);
    w.interrupt_actions("r1", "外部安排改变");
    assert!(reply_with(
        &mut r,
        &mut w,
        &review,
        "",
        vec![reflection_call(draft_decision(&review), false, 0.9)]
    ));
    assert_eq!(w.actor("r1").unwrap().locus.locale, "loc_a");
    assert!(r.session("r1").unwrap().last_action_reflection.is_none());
}
