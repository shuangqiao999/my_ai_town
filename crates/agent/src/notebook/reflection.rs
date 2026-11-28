//! reflect 工具的提交：先全部校验，再一次写入；一次失败不留下半份摘要或推进游标。
use super::{day_path, valid_path, NoteStatus, Page, ReflectionBatch};
use crate::{event, memory::MemoryKind, prompts, resident, session::Session, tools::limits};
use kernel::World;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReflectionUpdate {
    pub through_event: u64,
    pub summary: String,
    pub notes: Vec<NoteUpdate>,
    #[serde(default)]
    pub memory_writes: Vec<MemoryWrite>,
    #[serde(default)]
    pub lessons: Vec<LessonUpdate>,
    #[serde(default)]
    pub relationship_updates: Vec<RelationshipUpdate>,
    #[serde(default)]
    pub self_model_update: Option<SelfModelUpdate>,
    #[serde(default)]
    pub goal_adjustment: Option<GoalAdjustment>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryWrite {
    pub text: String,
    pub importance: f64,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LessonUpdate {
    pub text: String,
    pub evidence: String,
    pub confidence: f64,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipUpdate {
    pub target: String,
    pub judgment: String,
    pub trust_delta: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelfModelUpdate {
    #[serde(default)]
    pub strengths: Vec<String>,
    #[serde(default)]
    pub weaknesses: Vec<String>,
    #[serde(default)]
    pub identity_note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalAdjustment {
    pub needed: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub suggested_long_term_goals: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteUpdate {
    pub path: String,
    pub title: String,
    pub text: String,
    pub event_ids: Vec<u64>,
    pub status: NoteStatus,
    #[serde(default)]
    pub reading_quotes: Vec<ReadingQuote>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadingQuote {
    pub event_id: u64,
    pub text: String,
}

fn check_note(
    world: &World,
    session: &Session,
    batch: &ReflectionBatch,
    note: &NoteUpdate,
) -> Result<(), String> {
    let actor = resident(world, &session.actor_id).map_err(|e| e.to_string())?;
    if !valid_path(&note.path)
        || note.title.trim().is_empty()
        || note.title.chars().count() > limits::NOTE_TITLE_CHARS
        || note.text.trim().is_empty()
        || note.text.chars().count() > limits::NOTE_TEXT_CHARS
        || note.event_ids.is_empty()
        || note.event_ids.len() > limits::NOTE_EVENT_IDS_MAX
    {
        return Err("主题路径、正文或来源数量无效".into());
    }
    if note
        .event_ids
        .iter()
        .any(|id| *id > batch.through || event(actor, *id).is_none_or(|e| e.text.trim().is_empty()))
        || !note.event_ids.iter().any(|id| batch.event_ids.contains(id))
    {
        return Err("提炼必须引用本人已有的原始经历，且包含本批新证据".into());
    }
    let cites_reading = note
        .event_ids
        .iter()
        .any(|id| event(actor, *id).is_some_and(|e| e.readable.is_some()));
    if cites_reading && note.reading_quotes.is_empty() {
        return Err("引用阅读内容须提供 reading_quotes，核对实际读过版本的原文".into());
    }
    if note.reading_quotes.len() > limits::QUOTES_MAX {
        return Err(format!("每条笔记最多 {} 段摘录", limits::QUOTES_MAX));
    }
    for quote in &note.reading_quotes {
        let reading = world
            .contents()
            .reading_result(&session.actor_id, quote.event_id)
            .map_err(|e| e.to_string())?;
        if !note.event_ids.contains(&quote.event_id)
            || quote.text.trim().is_empty()
            || quote.text.chars().count() > limits::QUOTE_CHARS
            || !reading.content.text.contains(&quote.text)
        {
            return Err("摘录不属于引用的实际阅读正文".into());
        }
    }
    let occurrences = session
        .memory
        .records
        .iter()
        .filter(|m| {
            m.kind == MemoryKind::Episode
                && m.event_ids.iter().any(|id| note.event_ids.contains(id))
        })
        .count();
    if note.status == NoteStatus::Supported && occurrences < 2 {
        return Err("一件事的重复回执不是重复验证；支持中的认识至少需要两次不同经历".into());
    }
    Ok(())
}

fn check_update(
    world: &World,
    session: &Session,
    batch: &ReflectionBatch,
    update: &ReflectionUpdate,
) -> Result<(), String> {
    if update.through_event != batch.through
        || update.summary.trim().is_empty()
        || update.summary.chars().count() > limits::REFLECT_SUMMARY_CHARS
        || update.notes.len() > limits::NOTES_MAX
    {
        return Err("整理进度、摘要或主题数量无效".into());
    }
    if update.memory_writes.len() > 8
        || update.lessons.len() > 5
        || update.relationship_updates.len() > 5
    {
        return Err("整理提炼条数超过上限".into());
    }
    for write in &update.memory_writes {
        if write.text.trim().is_empty()
            || write.text.chars().count() > 160
            || !write.importance.is_finite()
            || !(0.0..=1.0).contains(&write.importance)
        {
            return Err("长期记忆候选无效".into());
        }
    }
    for lesson in &update.lessons {
        if lesson.text.trim().is_empty()
            || !lesson.confidence.is_finite()
            || !(0.0..=1.0).contains(&lesson.confidence)
        {
            return Err("经验规律无效".into());
        }
    }
    for relation in &update.relationship_updates {
        if relation.target.trim().is_empty() || !(-1.0..=1.0).contains(&relation.trust_delta) {
            return Err("关系更新无效".into());
        }
    }
    let mut paths = BTreeSet::new();
    for note in &update.notes {
        if !paths.insert(&note.path) {
            return Err("主题路径、正文或来源数量无效".into());
        }
        check_note(world, session, batch, note)?;
    }
    Ok(())
}

pub fn apply_reflection(
    world: &World,
    session: &mut Session,
    update: ReflectionUpdate,
) -> Result<(), String> {
    let batch = session
        .notebook
        .request
        .clone()
        .ok_or("当前没有待提交的增量整理")?;
    check_update(world, session, &batch, &update)?;
    let now = world.minutes();
    for note in update.notes {
        replace(
            session,
            Page {
                path: note.path,
                title: note.title,
                text: note.text,
                evidence: note.event_ids,
                status: note.status,
                updated_at: now,
                revision: 0,
            },
        );
    }
    let path = day_path(batch.day);
    let previous = session.notebook.pages.get(&path);
    let text = match previous {
        Some(p) => {
            p.text.clone()
                + &prompts::day_summary_section(batch.from + 1, batch.through, &update.summary)
        }
        None => update.summary.clone(),
    };
    let mut evidence = previous.map(|p| p.evidence.clone()).unwrap_or_default();
    evidence.extend(batch.event_ids.iter().copied());
    evidence.sort_unstable();
    evidence.dedup();
    replace(
        session,
        Page {
            path,
            title: prompts::day_summary_title(batch.day),
            text,
            evidence,
            status: NoteStatus::Candidate,
            updated_at: now,
            revision: 0,
        },
    );
    let book = &mut session.notebook;
    book.through_event = batch.through;
    book.last_reflection_at = now;
    book.failures = 0;
    book.retry_after = now;
    book.request = None;
    book.reading_dialogue.clear();

    let growth = !update.memory_writes.is_empty()
        || !update.lessons.is_empty()
        || !update.relationship_updates.is_empty()
        || update.self_model_update.is_some()
        || update.goal_adjustment.is_some();
    for write in &update.memory_writes {
        session.memory.write_consolidated(
            &session.actor_id,
            write.text.clone(),
            write.importance,
            write.tags.clone(),
            batch.event_ids.clone(),
            now,
        );
    }
    if let Some(update) = &update.self_model_update {
        if !update.strengths.is_empty() {
            session.self_model.strengths = update.strengths.clone();
        }
        if !update.weaknesses.is_empty() {
            session.self_model.weaknesses = update.weaknesses.clone();
        }
        if !update.identity_note.is_empty() {
            session.self_model.identity_note = update.identity_note.clone();
        }
    }
    session.pending_goal_adjustment = update
        .goal_adjustment
        .as_ref()
        .filter(|goal| goal.needed)
        .map(|goal| protocol::PendingGoalAdjustmentView {
            reason: goal.reason.clone(),
            suggested_long_term_goals: goal.suggested_long_term_goals.clone(),
        });
    let lessons: Vec<protocol::ConsolidatedLessonView> = update
        .lessons
        .iter()
        .map(|lesson| protocol::ConsolidatedLessonView {
            text: lesson.text.clone(),
            evidence: lesson.evidence.clone(),
            confidence: lesson.confidence,
            tags: lesson.tags.clone(),
        })
        .collect();
    let relationship_updates: Vec<protocol::RelationshipUpdateView> = update
        .relationship_updates
        .iter()
        .map(|relation| protocol::RelationshipUpdateView {
            target: relation.target.clone(),
            judgment: relation.judgment.clone(),
            trust_delta: relation.trust_delta,
        })
        .collect();
    let record = protocol::ConsolidationRecordView {
        sim_minutes: now,
        clock: kernel::clock_string(now),
        period_summary: update.summary.clone(),
        lessons,
        relationship_updates,
        self_model: session.self_model.clone(),
        goal_adjustment: session.pending_goal_adjustment.clone(),
    };
    session.push_consolidation(record);
    session.needs_calibration = growth;
    Ok(())
}
fn replace(session: &mut Session, mut page: Page) {
    session.notebook.revision += 1;
    page.revision = session.notebook.revision;
    if let Some(old) = session.notebook.pages.insert(page.path.clone(), page) {
        session.notebook.history.push(old);
    }
}
