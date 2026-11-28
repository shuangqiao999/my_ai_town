import type { ActionReflectionView, ResidentAgentView, ResidentDetailView } from '../protocol'
import { el as text } from '../dom'
import { clock, playerText } from '../format'
import {
  agentStateLabels,
  goalStatusLabels,
  interactionStatusLabels,
  memoryKindLabels,
  memorySourceLabels,
  memoryStatusLabels,
} from '../labels'

type Goal = ResidentDetailView['goals'][number]
type Result = ResidentDetailView['results'][number]
type Interaction = ResidentDetailView['interactions'][number]
type Memory = ResidentDetailView['memories'][number]
type Outcome = NonNullable<ResidentDetailView['outcome_records']>[number]
type Consolidation = NonNullable<ResidentDetailView['consolidation_records']>[number]
type Calibration = NonNullable<ResidentDetailView['calibration_records']>[number]

const resultStatusLabels = {
  succeeded: '完成',
  failed: '失败',
  interrupted: '中断',
  cancelled: '取消',
}

const alignmentLabels = { high: '高', medium: '中', low: '低' }
const riskLabels = { none: '无', low: '低', medium: '中', high: '高' }
const accuracyLabels = {
  accurate: '预判准确',
  optimistic: '预判乐观',
  pessimistic: '预判悲观',
  unknown: '未知',
}
const progressLabels = {
  advanced: '推进了目标',
  stalled: '原地踏步',
  regressed: '倒退了',
  unknown: '未知',
}

function outcomeRow(o: Outcome): HTMLElement {
  const row = text('article', '', 'agent-outcome')
  row.append(
    text('strong', `${o.clock} · ${accuracyLabels[o.prediction_accuracy]}`),
    text('p', `「${playerText(o.summary)}」`),
    text('p', `${progressLabels[o.goal_progress]} · ${playerText(o.goal_note)}`, 'dim')
  )
  for (const effect of o.unexpected_effects)
    row.append(text('p', `意外：${playerText(effect)}`, 'dim'))
  if (o.lesson) row.append(text('p', `学到：${playerText(o.lesson)}`, 'dim'))
  return row
}

function consolidationRow(c: Consolidation): HTMLElement {
  const row = text('article', '', 'agent-consolidation')
  row.append(text('strong', c.clock), text('p', `「${playerText(c.period_summary)}」`))
  for (const lesson of c.lessons) row.append(text('p', `经验：${playerText(lesson.text)}`, 'dim'))
  for (const rel of c.relationship_updates)
    row.append(text('p', `${playerText(rel.target)}：${playerText(rel.judgment)}`, 'dim'))
  return row
}

function calibrationRow(c: Calibration): HTMLElement {
  const row = text('article', '', 'agent-calibration')
  row.append(text('strong', c.clock), text('p', `「${playerText(c.summary)}」`))
  if (c.personality_note) row.append(text('p', playerText(c.personality_note), 'dim'))
  return row
}

function reflectionSection(r: ActionReflectionView): HTMLElement[] {
  const rows = [
    text('h4', `刚才的自问 · ${r.clock}`),
    text('p', `「${playerText(r.internal_thought)}」`),
    text('p', `自问：${playerText(r.self_question)}`, 'dim'),
    text('p', `预测：${playerText(r.predicted_outcome)}`, 'dim'),
    text(
      'p',
      `目标对齐：${alignmentLabels[r.goal_alignment]} · 风险：${riskLabels[r.risk]}`,
      'dim'
    ),
  ]
  if (r.risk_note) rows.push(text('p', playerText(r.risk_note), 'dim'))
  if (r.alternative_considered && r.alternative_considered !== '无')
    rows.push(text('p', `考虑过：${playerText(r.alternative_considered)}`, 'dim'))
  rows.push(text('p', `决定：${playerText(r.reason)}`, 'dim'))
  if (r.memory_note) rows.push(text('p', `记下：${playerText(r.memory_note)}`, 'dim'))
  return rows
}

/** 状态和原因；调用次数、token 这些在「消耗」页里 */
function header(view: ResidentAgentView): HTMLElement[] {
  return [
    text('strong', agentStateLabels[view.state]),
    text('p', playerText(view.reason), 'dim'),
    text('p', 'AI 用了多少、花了多少，在「消耗」里看', 'agent-meter'),
  ]
}

/** 小标题 + 空提示 + 每项一行 */
function section<T>(title: string, items: T[], empty: string, row: (item: T) => HTMLElement) {
  const nodes: HTMLElement[] = [text('h4', title)]
  if (!items.length) nodes.push(text('p', empty, 'dim'))
  for (const item of items) nodes.push(row(item))
  return nodes
}

function goalRow(goal: Goal) {
  const row = text('article', '', 'agent-goal')
  row.dataset.goalId = goal.id
  row.append(text('strong', `${goal.title} · ${goalStatusLabels[goal.status]}`))
  const ongoing = goal.status === 'active' || goal.status === 'paused'
  if (goal.commitment)
    row.append(text('p', `${ongoing ? '记着的事' : '当时的约定'}：${playerText(goal.commitment)}`))
  if (goal.reason) row.append(text('p', playerText(goal.reason), 'dim'))
  if (ongoing && goal.next_steps.length)
    row.append(text('p', `接下来：${playerText(goal.next_steps.join('；'))}`))
  return row
}

function resultRow(result: Result) {
  const row = text('article', '', 'agent-result')
  row.append(
    text('strong', `${result.verb} · ${resultStatusLabels[result.status]}`),
    text(
      'p',
      `${clock(result.ended_at)} · ${playerText(result.changes.join('；')) || '没有新的世界变化'}`,
      'dim'
    )
  )
  if (result.reason) row.append(text('p', playerText(result.reason)))
  return row
}

function interactionRow(interaction: Interaction) {
  const row = text('article', '', 'agent-result')
  row.dataset.interactionId = interaction.id
  row.append(
    text('strong', `${interaction.title} · ${interactionStatusLabels[interaction.status]}`),
    text('p', interaction.participants.join('、')),
    text('p', playerText(interaction.result), 'dim')
  )
  return row
}

/** 记忆一行；来源放在 details 里，opened 是上一次渲染时已展开的记忆 id */
function memoryRow(memory: Memory, opened: Set<string | undefined>) {
  const row = text('article', '', 'agent-memory')
  row.append(
    // 经历最常见，不写；记得好好的也不写，只写来源和改过看法的
    text(
      'p',
      [
        memory.kind === 'episode' ? '' : memoryKindLabels[memory.kind],
        memorySourceLabels[memory.source],
        memory.status === 'active' ? '' : memoryStatusLabels[memory.status],
      ]
        .filter(Boolean)
        .join(' · '),
      'agent-memory-meta'
    ),
    text('p', playerText(memory.text))
  )
  const details = document.createElement('details')
  details.dataset.memoryId = String(memory.memory_id)
  details.open = opened.has(details.dataset.memoryId)
  details.append(text('summary', `查看来源 · ${clock(memory.at)}`))
  if (memory.replaces.length)
    details.append(text('p', `修正了之前的 ${memory.replaces.length} 条记忆`))
  for (const event of memory.events)
    details.append(text('p', `${clock(event.at)}\n${playerText(event.text)}`, 'agent-source'))
  row.append(details)
  return row
}

/**
 * 只读投影，分两截：状态和原因跟着名册每拍来（render）；目标、动作结果、互动和记着的事是世界单独发来的
 * 细节（detail），变了才来一份，更新时保留已展开的来源。recorded：重演录像，没有世界可问，细节不显示。
 */
export function createAgentView(box: HTMLElement, recorded = false) {
  box.classList.add('resident-agent')
  const head = text('div')
  const lists = text('div')
  /** 上次画的状态和原因；空串是还没画过这个人 */
  let drawn = ''
  function clear() {
    drawn = ''
    lists.replaceChildren()
    box.replaceChildren(text('p', '该村民没有 Agent 会话', 'dim'))
  }
  function render(view?: ResidentAgentView) {
    if (!view) {
      if (drawn) clear()
      return
    }
    const key = `${view.state}\n${view.reason}`
    if (key === drawn) return
    if (!drawn) {
      box.replaceChildren(head, lists)
      // 重演时只有名册：说清这一截为什么空着
      if (recorded) lists.replaceChildren(text('p', '重演录像时不显示目标与记忆', 'dim'))
    }
    drawn = key
    head.replaceChildren(...header(view))
  }
  /** 选中这位的细节到了；null 是世界说他没有（没有 Agent 会话） */
  function detail(view: ResidentDetailView | null) {
    if (!view) return lists.replaceChildren()
    const opened = new Set(
      Array.from(lists.querySelectorAll<HTMLDetailsElement>('details[open]')).map(
        (d) => d.dataset.memoryId
      )
    )
    const fragment = document.createDocumentFragment()
    if (view.reflection) fragment.append(...reflectionSection(view.reflection))
    if (view.personality) {
      const p = view.personality
      fragment.append(
        text('h4', '人格倾向'),
        text(
          'p',
          `谨慎 ${p.caution.toFixed(2)} · 信任 ${p.trust_baseline.toFixed(2)} · 探索 ${p.exploration.toFixed(2)} · 社交 ${p.social_initiative.toFixed(2)} · 反思 ${p.reflection_confidence.toFixed(2)}`,
          'dim'
        )
      )
    }
    if (view.self_model?.identity_note)
      fragment.append(text('h4', '自我认识'), text('p', playerText(view.self_model.identity_note)))
    fragment.append(
      ...section('最近回顾', view.outcome_records ?? [], '还没有行动回顾', outcomeRow),
      ...section('最近整理', view.consolidation_records ?? [], '还没有周期整理', consolidationRow),
      ...section('最近校准', view.calibration_records ?? [], '还没有人格校准', calibrationRow),
      ...section('持续目标', view.goals, '还没有记录目标', goalRow),
      ...section('最近动作结果', view.results, '还没有动作结果', resultRow)
    )
    if (view.interactions.length)
      fragment.append(text('h4', '互动与聊天'), ...view.interactions.map(interactionRow))
    fragment.append(
      ...section('记着的事', view.memories, '还没有记下什么', (memory) => memoryRow(memory, opened))
    )
    lists.replaceChildren(fragment)
  }
  clear()
  return { render, detail, clear, reset: clear }
}
