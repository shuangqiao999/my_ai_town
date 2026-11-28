use crate::support::*;
use agent::{budget::Limits, runtime::ResidentRuntime};
use serde_json::json;

fn reviewing_runtime(w: &mut kernel::World) -> ResidentRuntime {
    let mut r = runtime(w, Limits::default());
    r.set_action_reflection("r1", true);
    r
}

fn reflection_call(
    calls: Vec<serde_json::Value>,
    correction: bool,
) -> agent::conversation::ToolCall {
    tool_call(
        "self-review",
        "submit_action_reflection",
        json!({
            "internal_thought": "先去乙地。",
            "reflection": {
                "self_question": "现在就走吗？",
                "predicted_outcome": "到乙地。",
                "goal_alignment": "high",
                "risk": "low",
                "risk_note": "没危险。",
                "alternative_considered": "无",
                "correction_needed": correction
            },
            "decision": {"calls": calls, "confidence": 0.9, "reason": "按计划走。"},
            "memory_note": ""
        }),
    )
}

fn post_call(importance: f64) -> agent::conversation::ToolCall {
    tool_call(
        "post",
        "submit_post_review",
        json!({
            "outcome_record": {
                "summary": "到了乙地。",
                "prediction_accuracy": "accurate",
                "goal_progress": "advanced",
                "goal_note": "离目标更近了。",
                "unexpected_effects": [],
                "lesson": "先确认再走更稳。",
                "confidence_delta": 0.05
            },
            "memory_candidate": {"text": "先确认目的地再出发。", "importance": importance, "tags": ["goal"]}
        }),
    )
}

#[test]
fn a_reviewed_action_gets_a_post_review_and_writes_an_outcome() {
    let mut w = world();
    let mut r = reviewing_runtime(&mut w);
    let q = life_request(&mut r, &mut w);
    assert!(reply_with(
        &mut r,
        &mut w,
        &q,
        "先去乙地。",
        vec![tool_call(
            "draft-move",
            "run_actions",
            json!({"actions":[{"intent":{"verb":"前往","locale":"loc_b"}}]})
        )]
    ));
    let review = request(&mut r, &mut w);
    assert_eq!(review.stamp.kind, protocol::RequestKind::ActionReflection);
    let body: serde_json::Value =
        serde_json::from_str(&review.messages.last().unwrap().content).unwrap();
    let confirm = body["draft"]["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|call| json!({"action_id": call["name"], "action_params": call["arguments"]}))
        .collect::<Vec<_>>();
    assert!(reply_with(
        &mut r,
        &mut w,
        &review,
        "",
        vec![reflection_call(confirm, false)]
    ));

    w.step(3.0);
    assert_eq!(w.actor("r1").unwrap().locus.locale, "loc_b");
    assert!(r.poll(&mut w, true).is_empty(), "先结算动作并登记事后评估");
    let post = request(&mut r, &mut w);
    assert_eq!(post.stamp.kind, protocol::RequestKind::PostActionReview);
    assert!(reply_with(&mut r, &mut w, &post, "", vec![post_call(0.6)]));
    let s = r.session("r1").unwrap();
    assert_eq!(s.outcome_records.len(), 1);
    assert_eq!(
        s.outcome_records[0].prediction_accuracy,
        protocol::PredictionAccuracy::Accurate
    );
    assert!(s
        .memory_candidates
        .iter()
        .any(|c| c.importance >= 0.3 && !c.text.is_empty()));
}

#[test]
fn consolidation_writes_consolidated_memory_and_marks_calibration() {
    use agent::notebook::{apply_reflection, ReflectionBatch, ReflectionUpdate};
    use agent::session::Session;
    let w = world();
    let mut s = Session::new(1, "r1", "先确认再走。");
    s.notebook.request = Some(ReflectionBatch {
        from: 0,
        through: 1,
        day: 0,
        event_ids: vec![1],
        reason: "测试整理".into(),
    });
    let update: ReflectionUpdate = serde_json::from_value(json!({
        "through_event": 1,
        "summary": "先确认再走更稳。",
        "notes": [],
        "memory_writes": [{"text": "先确认目的地再出发。", "importance": 0.7, "tags": ["goal"]}],
        "lessons": [],
        "relationship_updates": [],
        "self_model_update": {"strengths": ["谨慎"], "weaknesses": [], "identity_note": "我做事先想清楚。"},
        "goal_adjustment": {"needed": false, "reason": "", "suggested_long_term_goals": []}
    }))
    .unwrap();
    apply_reflection(&w, &mut s, update).unwrap();
    assert!(s
        .memory
        .records
        .iter()
        .any(|m| m.source == protocol::EvidenceSource::Consolidated && m.importance > 0.0));
    assert!(s.needs_calibration);
    assert!(!s.self_model.strengths.is_empty());
}

#[test]
fn a_chat_speech_gets_reflection_and_post_review() {
    use protocol::InteractionCommand;
    let mut w = world();
    let mut r = reviewing_runtime(&mut w);
    w.spawn("r2", "小禾", "loc_a").unwrap();
    w.enable_agent("r2").unwrap();
    let rev = w.actor("r1").unwrap().actions.revision;
    w.interact(
        "r1",
        "chat-offer",
        rev,
        &InteractionCommand::Offer {
            partner_id: "r2".into(),
            interaction: protocol::InteractionKind::Talk,
            message: "聊两句？".into(),
        },
    )
    .unwrap();
    let rev = w.actor("r2").unwrap().actions.revision;
    w.interact(
        "r2",
        "chat-accept",
        rev,
        &InteractionCommand::Reply {
            interaction_id: "chat-offer".into(),
            accept: true,
            message: "好".into(),
        },
    )
    .unwrap();
    let q = request(&mut r, &mut w);
    assert_eq!(q.stamp.kind, protocol::RequestKind::Chat);
    let say = tool_call(
        "say",
        "interact",
        json!({"operation":"say","interaction_id":"chat-offer","message":"边走边聊。"}),
    );
    assert!(reply_with(&mut r, &mut w, &q, "先打个招呼。", vec![say]));
    let review = request(&mut r, &mut w);
    assert_eq!(review.stamp.kind, protocol::RequestKind::ActionReflection);
    let body: serde_json::Value =
        serde_json::from_str(&review.messages.last().unwrap().content).unwrap();
    assert!(body["chat"].is_object(), "交谈自省要带当前对话");
    let confirm = json!([{
        "action_id": "interact",
        "action_params": {"operation":"say","interaction_id":"chat-offer","message":"边走边聊。"}
    }]);
    assert!(reply_with(
        &mut r,
        &mut w,
        &review,
        "",
        vec![reflection_call(confirm.as_array().unwrap().to_vec(), false)]
    ));
    let post = request(&mut r, &mut w);
    assert_eq!(post.stamp.kind, protocol::RequestKind::PostActionReview);
    assert!(reply_with(&mut r, &mut w, &post, "", vec![post_call(0.6)]));
    let s = r.session("r1").unwrap();
    assert_eq!(s.outcome_records.len(), 1);
    assert!(w.active_chat("r1").is_some());
}
