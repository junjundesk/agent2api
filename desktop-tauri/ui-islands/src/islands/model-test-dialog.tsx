/**
 * Agent2API · 「测试模型」两层弹窗 —— React 岛。
 *
 * 入口在**模型管理页操作列**那颗「测试」（models-page.tsx 的 `case 'act'`）。入口只开这一处：
 * 账号页一行是登录态、不携带模型清单，从那边发起测试要再造一个模型选择器，而账号页操作列已经
 * 是六项；账号维度由本弹窗的「测试账号」多选覆盖 —— 那是同一个动作的另一个参数，不是另一件事。
 *
 * ── 两层：参数在下、结果在上 ──────────────────────────────────
 *   1. **参数层**（外层 Dialog）：目标行 + 系统提示词 / 用户提示词 / 思考等级 / 流式 / 测试账号。
 *      默认值刻意取「最小请求」——用户提示词「你好」、流式开、思考等级跟随映射；
 *   2. **结果层**（内层 Dialog，按账号分块）：测试中 / 一成一败 / 全部失败三态都住在这一层。
 *
 * 内层是**嵌在外层 DialogContent 的 children 里**的（照 add-account-modal.tsx 的双层形态）：
 * React 父子关系是 Base UI 认「嵌套弹窗」的判据，内层开着时外层的 Esc 与遮罩按压才不生效；
 * 位置也必须是外层内容的子树，内层 Portal 才会排在父层 Popup 之后、落在上面。内层要给
 * `overlayForceRender` —— 它要压暗下层（含参数弹窗），而「点空白处关掉这一层」认的正是遮罩本身。
 * 关掉结果层 = 回到参数层继续改（参数不重填）；关掉参数层才整个收口。
 *
 * ── 口径：走真实转发链路，按账号钉住 ──────────────────────────
 * 每勾一个账号就并发发一次 `POST /api/models/test`（一个账号一条请求），后端把这一家与这一个
 * 账号**钉住**再走 `UpstreamService::forward` —— token 刷新、出站脱敏、提示词模式、思考等级注入、
 * 请求日志与调试报文全都覆盖到，所以结论与生产一致。也因此**会消耗少量额度**，且每次测试都带
 * 「测试」来源标记记入请求日志（报表统计里排除，见后端 `is_test`）。参数弹窗底部那行小字就是
 * 把这三件事说清。
 *
 * ── 中止：没有 abort 桥，靠同一个关联 id ──────────────────────
 * 桌面壳的桥（`invoke('api_request')`）**没有 abort**：一次已经在跑的调用取消不掉。所以 id 由
 * **前端**生成（`test_id`）随请求一起发上去，中止时用同一个 id 去调既有的「终止请求」
 * （`terminateStatsRequest` → `POST /api/stats/requests/terminate?id=`）置位取消令牌，转发链当场
 * 断开上游。三处对齐：结果块、请求日志里那一行、中止用的把手，都是这一个 id。
 * 中止后回到参数层（`slots` 清空、seq 自增让在途回调作废），不把半截结果留在屏幕上。
 *
 * ── 两处刻意的「不显示」─────────────────────────────────────
 *   · **不摆原始报文**：发出去与收回来的原文在请求日志里能看能搜，堆进结果弹窗只会把「成没成、
 *     为什么不成」淹掉；
 *   · **不解释网关的换号口径**：测试按指定账号发、不顺延，那是测试的语义，不是转发策略的说明。
 */

import * as React from 'react'
import {
  Badge, Button, Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle,
  Label, MultiSelect, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Spinner,
  Switch, Textarea,
} from '@ui'
import {
  DEFAULT_PROVIDER_ID, byPriorityOrder, displayNameOf, isRateLimited, positionMap, providerFeatures,
} from './accounts-domain'
import type { AccountRecord } from './accounts-shared'
import * as customSource from './models-custom-source'
import type { ManageModel } from './models-custom-source'
import { levels as reasoningLevels } from './models-reasoning'
import {
  bindingsOf, errorMessage, getSnapshot, levelOf, modelRowOf, providerLabelOf, toast,
} from './models-panel-state'

/* ─── 类型 ─────────────────────────────────── */

/** 被测试的那一行：只带 `(provider, id)` 两个定位键（照 CapabilityContext 的形状） */
export type ModelTestTarget = { provider: string; id: string }

/** 一次测试的请求体（字段名与后端 `api::model_test` 逐字对齐，snake_case） */
type TestModelRequest = {
  provider: string
  model: string
  account_id?: string
  prompt?: string
  system_prompt?: string
  /** 本次指定的思考等级；空 = 不写进请求体（映射上绑的等级因此照常生效） */
  reasoning?: string
  stream?: boolean
  /** 前端生成的关联 id（请求日志那一行的 id，也是中止用的把手） */
  test_id?: string
}

/** 一次测试的响应（POST /api/models/test 永远 2xx，失败结论在 `status` / `error` 里） */
type TestModelResponse = {
  success?: boolean
  status?: number
  error?: string | null
  reply?: string
  reasoning?: string
  provider?: string
  model?: string
  account_id?: string | null
  account_name?: string | null
  upstream_model?: string | null
  upstream_reasoning?: string | null
  duration_ms?: number
  ttfb_ms?: number | null
  attempts?: number
  prompt_tokens?: number
  completion_tokens?: number
  total_tokens?: number
}

/**
 * 一个账号一条的测试槽位。
 *
 * `label` / `position` 是**发起时**的快照：结果回来时那一条账号可能已被改名或删掉，而用户要看的
 * 是「我刚才点的那一条」的结论 —— 改用结果里的 `account_name` 会让块标题中途变脸。
 */
type Slot = {
  accountId: string
  label: string
  position: number
  testId: string
  startedAt: number
  state: 'running' | 'done'
  result?: TestModelResponse
  /** 桥接层失败（本机接口不可达 / 超时）：与「上游给出的失败结论」分开表达 */
  transport?: string
}

/**
 * window 上由其它脚本 / 其它岛挂载的共享桥。
 *
 * 刻意用「局部窄类型 + 转型读取」而不是 declare global 往 Window 上加属性：workbuddyDesktop /
 * wbApp 是多个岛共用的桥，各岛各 declare 一份会因同名属性类型不一致直接报 TS2717 —— 并行迁移时
 * 必然互相撞车。本文件只声明自己用到的那几个成员（其余桥走 models-panel-state / accounts-domain
 * 里已有的那份）。
 */
type WindowBridge = {
  workbuddyDesktop?: {
    testModel(payload: TestModelRequest): Promise<TestModelResponse | null | undefined>
    /** 「终止请求」：按关联 id 置位取消令牌（测试的中止走它，见模块头） */
    terminateStatsRequest?(id: string): Promise<unknown>
  }
  wbApp?: {
    getState?: () => { accounts?: { accounts?: AccountRecord[] } } | null | undefined
  }
}

function bridge(): WindowBridge {
  return window as unknown as WindowBridge
}

/* ─── 常量 ─────────────────────────────────── */

/**
 * 用户提示词留空时后端用的默认值（后端 `api::model_test::DEFAULT_TEST_PROMPT`）。
 * 两处必须是同一句：取「你好」而不是一句有信息量的话 —— 测试要的是**最小请求**，越短越省额度、
 * 越少触发上游的内容策略，也越容易看出「通不通」这件事本身。
 */
const DEFAULT_PROMPT = '你好'

/** 提示词长度上限（与后端 `MAX_PROMPT_CHARS` 同值；超了后端会截断，这里先挡一道） */
const MAX_PROMPT_CHARS = 4000

/** 模型行的「来源」徽章（与模型表同一套口径；自定义家的 source 是空串，见 ManageModel） */
const SOURCE_LABEL: Record<string, string> = {
  remote: '远程目录',
  builtin: '内置清单',
  manual: '手动登记',
}

/**
 * 失败原因 → 下一步。**只按 HTTP 状态分类**，不猜上游的文案：429 与 401 指向完全不同的处置
 * （一个等一会儿自己会好，一个换账号也救不回来），5xx / 404 又是另外两件事。
 * 后端给的 `error` 原样展示在上一行，这里只补「接下来做什么」。
 */
const STATUS_HINT: Array<{ test: (status: number) => boolean; hint: string }> = [
  {
    test: status => status === 429,
    hint: '该账号对这个模型正在限额冷却，等一会儿自己会好；也可以先测别的账号。',
  },
  {
    test: status => status === 401 || status === 403,
    hint: '上游拒绝了这条登录态：重新登录一次，或把这条账号删掉。',
  },
  {
    test: status => status === 404,
    hint: '上游不认识这个模型名：先点「获取模型」刷新清单，或核对默认绑定指向的上游模型。',
  },
  {
    test: status => status === 504,
    hint: '上游没在预算内给完回答。可以只勾一个账号再测一次，看是普遍慢还是某一个账号慢。',
  },
  {
    test: status => status >= 500,
    hint: '多半是上游自己的问题，过一会儿再试。同一个错误出现在所有账号上时，先查模型目录与映射。',
  },
]

/* ─── 纯函数工具 ─────────────────────────────── */

/** 全部账号（主状态快照；账号页与本弹窗读的是同一份） */
function allAccounts(): AccountRecord[] {
  return bridge().wbApp?.getState?.()?.accounts?.accounts || []
}

/**
 * 该家可用于测试的账号：过滤 = 启用 + 有凭证（后端口径，与「模型来源」下拉同源），
 * 排序用账号页同一条 `byPriorityOrder`（全局一条队列，本弹窗只取这一家的子集）。
 */
function usableAccounts(provider: string): AccountRecord[] {
  return allAccounts()
    .filter(account => (account.provider || DEFAULT_PROVIDER_ID) === provider
      && account.available !== false && account.enabled !== false)
    .sort(byPriorityOrder)
}

/**
 * 账号的展示名：与「模型来源」下拉（models-fetch-modal.tsx 的 accountLabel）同一口径 ——
 * 以邮箱报名字的家（Qoder / AutoClaw 国际版）用邮箱，其余用账号页的展示名。
 */
function accountLabel(account: AccountRecord): string {
  const email = String(account.email || '').trim()
  if (email && providerFeatures(account.provider).emailAsName) return email
  return displayNameOf(account) || email || '未命名账号'
}

/** 关联 id：优先 `crypto.randomUUID`（WebView2 与 localhost 都是安全上下文），退化到自拼的 v4 形态 */
function newTestId(): string {
  const uuid = globalThis.crypto?.randomUUID?.()
  if (uuid) return uuid
  const hex = (count: number): string =>
    Array.from({ length: count }, () => Math.floor(Math.random() * 16).toString(16)).join('')
  return `${hex(8)}-${hex(4)}-4${hex(3)}-a${hex(3)}-${hex(12)}`
}

/** 毫秒读数：1 秒以内给整数毫秒，再长给秒（两位小数；十秒以上一位就够） */
function formatMs(value: unknown): string {
  const time = Number(value)
  if (!Number.isFinite(time) || time <= 0) return '—'
  return time < 1000 ? `${Math.round(time)} ms` : `${(time / 1000).toFixed(time < 10000 ? 2 : 1)} s`
}

/**
 * 「测试」那颗按钮的两条门禁，都写进按钮的悬停说明，不做成静默失败：
 *   · 这一行的**默认绑定**（名字与模型 ID 相同的那条）关着 → 测试正是以这个名字发出去的，
 *     后端会把这种请求判成「模型已在网关中关闭」，那句话对用户毫无指引；
 *   · 该家**没有可用账号**（启用 + 凭证完整）→ 一行都发不出去。
 * 返回空串 = 可以测；否则返回要挂在按钮 title 上的原因。
 *
 * 为什么判的是默认绑定而不是「有没有任意一条映射开着」：**别名不参与**这次测试（下游模型名
 * 就是本名），所以只开着别名映射时以本名发出去的路由仍然是断的 —— 那种情况下按钮该置灰并说清
 * 要打开哪一条，而不是让用户测出一次莫名其妙的 404。
 */
export function testBlockReason(provider: string, model: ManageModel): string {
  const bindings = bindingsOf(model)
  const sameName = bindings.find(binding => binding.isDefault)
  if (sameName && !sameName.enabled) {
    return bindings.some(binding => binding.enabled)
      ? '这一行的默认绑定（与模型 ID 同名的那条）是关着的，而测试就以这个名字发出去 —— 先打开它（别名映射不参与本次测试）'
      : '这一行的映射全部关着，下游请求根本路由不到它 —— 先打开默认绑定那一条'
  }
  if (!usableAccounts(provider).length) {
    return '该提供商没有可用账号（要在账号页启用一个、且凭证完整），一行都发不出去'
  }
  return ''
}

/* ─── 小件 ─────────────────────────────────── */

/** 「本次将发 N 次…」那条口径说明（圈的用法与页面上其它提示一致：一个 i + 一句话） */
function NoteBlock({ children }: { children: React.ReactNode }) {
  return (
    <p className='flex gap-2 rounded-md border border-border bg-surface-2 px-3 py-2.5 text-xs leading-[1.65] text-subtle'>
      <span aria-hidden='true'
        className='mt-px size-4 flex-none rounded-full border border-border-strong text-center text-[10px] leading-[14px]'>i</span>
      <span>{children}</span>
    </p>
  )
}

/**
 * 一个可折叠块（思考过程）。头是组件库的 ghost 按钮而不是自绘 button ——
 * tokens.css 只给 button 做了 `font / color: inherit`，没清浏览器默认的描边与底色。
 * 左对齐靠「末件 ml-auto」而不是 `justify-start`：那是同一个属性的两张工具类，
 * 谁赢由 Tailwind 的产出顺序决定（center 在后），不能指望覆盖。
 */
function Fold({ open, label, text, onToggle }: {
  open: boolean; label: string; text: string; onToggle: () => void
}) {
  return (
    <div className='overflow-hidden rounded-sm border border-border'>
      <Button variant='ghost' size='xs' className='w-full' aria-expanded={open} onClick={onToggle}>
        <span className='truncate'>{label}</span>
        <span aria-hidden='true' className='ml-auto text-[10px]'>{open ? '▴' : '▾'}</span>
      </Button>
      {open ? (
        <div className='border-t border-hairline bg-surface px-2.5 py-2'>
          <pre className='whitespace-pre-wrap font-mono text-[11.5px] leading-[1.65] text-subtle'>{text}</pre>
        </div>
      ) : null}
    </div>
  )
}

/** 「已等待 1.4s」的读秒：只在跑着的时候开定时器（停表后不再空转） */
function useClock(active: boolean): number {
  const [now, setNow] = React.useState(() => Date.now())
  React.useEffect(() => {
    if (!active) return
    setNow(Date.now())
    const timer = window.setInterval(() => setNow(Date.now()), 400)
    return () => window.clearInterval(timer)
  }, [active])
  return now
}

/** 成功判据：没有错误结论、且状态码是 2xx（后端把「上游失败」也放在 2xx 的响应体里） */
function isOk(slot: Slot): boolean {
  if (slot.transport || !slot.result) return false
  if (slot.result.error) return false
  const status = Number(slot.result.status) || 0
  return status >= 200 && status < 300
}

/* ─── 弹窗本体 ───────────────────────────────── */

export function ModelTestDialog({ target, onClose }: { target: ModelTestTarget; onClose: () => void }) {
  const provider = target.provider
  const model = modelRowOf(provider, target.id)

  const usable = usableAccounts(provider)
  const positions = positionMap(allAccounts())

  const [systemPrompt, setSystemPrompt] = React.useState('')
  const [prompt, setPrompt] = React.useState(DEFAULT_PROMPT)
  const [reasoning, setReasoning] = React.useState('')
  const [stream, setStream] = React.useState(true)
  /**
   * 默认只勾**队列里最靠前的那个可测账号**（真实链路的第一个候选）。
   * 对当前模型正在冷却的账号靠后站：拿它测只会第一跳就拿回 429，而用户要的是「这个模型通不通」。
   */
  const [picked, setPicked] = React.useState<string[]>(() => {
    const first = usable.find(account => !isRateLimited(account, target.id)) || usable[0]
    return first ? [first.id] : []
  })
  const [slots, setSlots] = React.useState<Slot[]>([])
  const [folds, setFolds] = React.useState<ReadonlySet<string>>(() => new Set())

  /** 本次运行的序号：中止 / 重测会让在途回调作废（迟到的结果不许写进新一轮） */
  const seq = React.useRef(0)
  const slotsRef = React.useRef<Slot[]>([])
  slotsRef.current = slots

  const running = slots.some(slot => slot.state === 'running')
  const now = useClock(running)

  /** 中止在途请求：seq 自增（作废回调）+ 逐个置位取消令牌，然后清空槽位回到参数层 */
  const abort = React.useCallback((quiet = false) => {
    const inflight = slotsRef.current.filter(slot => slot.state === 'running')
    seq.current += 1
    for (const slot of inflight) {
      void bridge().workbuddyDesktop?.terminateStatsRequest?.(slot.testId)?.catch(() => {})
    }
    setSlots([])
    if (inflight.length && !quiet) toast(`已中止在途测试（${inflight.length} 个账号）`)
  }, [])

  // 弹窗被卸掉（用户关掉参数层 / 模型行在目录刷新后消失）时补一刀：已经放弃的在途请求不该继续
  // 占上游额度。清理里不再动状态（组件已经没了），所以走 quiet。
  React.useEffect(() => () => { abort(true) }, [abort])

  /** 发一次测试：结果按 testId 对号入座；整轮的序号变了就直接丢弃（中止 / 重测） */
  async function send(slot: Slot, runId: number): Promise<void> {
    const body: TestModelRequest = {
      provider,
      model: target.id,
      account_id: slot.accountId,
      prompt: prompt.trim(),
      system_prompt: systemPrompt.trim(),
      reasoning,
      stream,
      test_id: slot.testId,
    }
    const settle = (patch: Partial<Slot>): void => {
      if (seq.current !== runId) return
      setSlots(previous => previous.map(item => (item.testId === slot.testId ? { ...item, ...patch } : item)))
    }
    try {
      const result = await bridge().workbuddyDesktop?.testModel(body)
      if (!result) {
        settle({ state: 'done', transport: '桥接调用返回空（本机网关没有响应这次测试）' })
        return
      }
      settle({ state: 'done', result })
    } catch (error) {
      settle({ state: 'done', transport: errorMessage(error) })
    }
  }

  /** 发起一轮：勾了几个账号就并发发几次（一个账号一条请求，互不影响） */
  function run(): void {
    const chosen = usable.filter(account => picked.includes(account.id))
    if (!chosen.length) {
      toast('至少选一个测试账号', 'err')
      return
    }
    const runId = seq.current + 1
    seq.current = runId
    const startedAt = Date.now()
    const next: Slot[] = chosen.map(account => ({
      accountId: account.id,
      label: accountLabel(account),
      position: positions.get(account.id)?.position || 0,
      testId: newTestId(),
      startedAt,
      state: 'running',
    }))
    setFolds(new Set())
    setSlots(next)
    for (const slot of next) void send(slot, runId)
  }

  /** 结果层关掉：跑着的时候 = 中止（参数留着，回下层改完就能重测） */
  function closeResults(): void {
    if (running) abort()
    else setSlots([])
  }

  /** 顶栏那个模型 ID 小字（两层标题共用） */
  const modelTag = <span className='ml-2 font-mono text-[12px] font-normal text-subtle'>{target.id}</span>

  /** 目标行不在清单里了（模型被移除，或目录刷新后上游不再提供它）——与「模型能力」同一处置 */
  if (!model) {
    return (
      <Dialog open onOpenChange={next => { if (!next) onClose() }}>
        <DialogContent>
          <DialogHeader><DialogTitle>测试模型{modelTag}</DialogTitle></DialogHeader>
          <DialogBody>
            <p className='text-sm leading-[1.7] text-subtle'>
              这一行已不在当前清单里（模型被移除、或目录刷新后上游不再提供它）。关闭后刷新列表再试。
            </p>
          </DialogBody>
          <DialogFooter>
            <div className='mr-auto' />
            <Button variant='outline' onClick={onClose}>关闭</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    )
  }

  // 本名直发时生效的思考等级只认「名字不变的那条按家映射」（见 model_rules::reasoning 第 4 条），
  // 而那正是这一行的默认绑定 —— 与表格里默认 chip 上显示的是同一个值
  const boundLevel = levelOf(model.id, model.id, provider)
  const aliases = bindingsOf(model)
    .filter(binding => !binding.isDefault)
    .map(binding => binding.alias)
  // 「关闭思考」两档（off / none）不给：它们在**映射**上的含义是「不注入」，而写进**请求体**
  // 就是一个上游不认识的档位 —— CatPaw 的 resolve_effort 对表外值当场 400。要「不注入」，
  // 选「跟随映射」这一项（映射上没绑等级时它本来就不注入）
  const levelOptions = reasoningLevels(getSnapshot().data)
    .filter(level => level !== 'off' && level !== 'none')
  const followLabel = boundLevel ? `跟随映射（当前 ${boundLevel}）` : '跟随映射（未绑定等级）'
  const accountOptions = usable.map(account => ({
    value: account.id,
    // 「限额中」按**这个模型**判（限额是按模型记的：一个账号可能对 A 模型限额、对 B 模型正常）
    label: `#${positions.get(account.id)?.position || 0} ${accountLabel(account)}`
      + (isRateLimited(account, target.id) ? ' · 限额中' : ''),
  }))

  /** 一个账号的结果块（成功：回复 + 思考过程折叠；失败：错误结论 + 下一步） */
  function slotBlock(slot: Slot): React.ReactNode {
    const head = (badge: React.ReactNode, metrics: string): React.ReactNode => (
      <div key='head' className='flex items-center gap-2.5 border-b border-hairline px-3 py-2'>
        {badge}
        <b className='min-w-0 truncate text-[12.5px] text-foreground' title={slot.label}>{slot.label}</b>
        {slot.position ? <span className='flex-none text-xs text-subtle'>#{slot.position}</span> : null}
        <span className='ml-auto flex-none font-mono text-[11.5px] tabular-nums text-subtle'>{metrics}</span>
      </div>
    )

    if (slot.state === 'running') {
      const waited = (now - slot.startedAt) / 1000
      return (
        <div key={slot.testId}
          className='flex items-center gap-2.5 rounded-md border border-border bg-surface-2 px-3 py-2.5 text-xs text-subtle'>
          <Spinner className='flex-none' />
          <span className='min-w-0 truncate'>
            账号 <b className='text-foreground'>{slot.label}</b> 已发出，等待上游首帧…
          </span>
          <span className='ml-auto flex-none font-mono tabular-nums'>已等待 {waited.toFixed(1)}s</span>
        </div>
      )
    }

    const result = slot.result
    if (!isOk(slot)) {
      const status = Number(result?.status) || 0
      const hint = STATUS_HINT.find(item => item.test(status))?.hint
        ?? (slot.transport ? '确认桌面端还在运行，再重试一次。' : '')
      return (
        <div key={slot.testId} className='rounded-md border border-border bg-surface-2'>
          {head(
            <Badge shape='tag' variant='destructive' className='flex-none'>{status || '失败'}</Badge>,
            `用时 ${formatMs(result?.duration_ms)}`,
          )}
          <div className='flex flex-col gap-1.5 px-3 py-2.5'>
            <div className='text-[12.5px] leading-[1.7]'>
              {slot.transport
                ? <><b className='text-destructive'>本机网关没有返回结论</b>：{slot.transport}</>
                : <><b className='text-destructive'>测试未通过</b>：{result?.error || '上游没有给出可读的错误说明'}</>}
              {Number(result?.attempts) > 1 ? `（中间共尝试 ${Number(result?.attempts)} 次）` : ''}
            </div>
            {hint ? <p className='text-xs leading-[1.65] text-subtle'>{hint}</p> : null}
          </div>
        </div>
      )
    }

    const reply = String(result?.reply ?? '')
    const reasoningText = String(result?.reasoning ?? '')
    const foldKey = `think-${slot.testId}`
    const metaLine = [
      String(result?.upstream_model || target.id),
      result?.upstream_reasoning ? `思考等级 ${result.upstream_reasoning}` : '',
      Number(result?.attempts) > 1 ? `尝试 ${Number(result?.attempts)} 次` : '',
    ].filter(Boolean).join(' · ')
    return (
      <div key={slot.testId} className='rounded-md border border-border bg-surface-2'>
        {head(
          <Badge shape='tag' variant='success' className='flex-none'>{Number(result?.status) || 200}</Badge>,
          `用时 ${formatMs(result?.duration_ms)} · 首字 ${formatMs(result?.ttfb_ms)}`,
        )}
        <div className='flex flex-col gap-2 px-3 py-2.5'>
          <div className='flex items-center gap-2 text-[11.5px] text-subtle'>
            <span className='flex-none'>回复</span>
            <span className='min-w-0 truncate' title={metaLine}>上游 {metaLine}</span>
            {/* 复制走 clipboard.js 的全局委托（data-copy），与模型名那枚复制同一套 */}
            <Button variant='ghost' size='2xs' className='ml-auto flex-none' data-copy={reply}>复制</Button>
          </div>
          <div className='max-h-[220px] overflow-y-auto whitespace-pre-wrap rounded-sm border border-border bg-surface px-3 py-2 text-[12.5px] leading-[1.7]'>
            {reply || <span className='text-subtle'>（上游这一次返回了空正文）</span>}
          </div>
          {reasoningText ? (
            <Fold open={folds.has(foldKey)} label={`思考过程（${reasoningText.length} 字）`} text={reasoningText}
              onToggle={() => setFolds(previous => {
                const next = new Set(previous)
                if (next.has(foldKey)) next.delete(foldKey)
                else next.add(foldKey)
                return next
              })} />
          ) : null}
        </div>
      </div>
    )
  }

  /**
   * 结果区：测试中把已回来的账号**就地换成结果块**（只有还在等的留骨架）—— 结果本来就是一个
   * 账号一条回来的，全憋到最后一起显示等于白等；出结果后每行一个块，外加一条成功 / 失败读数。
   */
  function results(): React.ReactNode {
    if (running) {
      const done = slots.length - slots.filter(slot => slot.state === 'running').length
      return (
        <>
          <p className='text-xs text-subtle'>
            正在测 <b className='text-foreground'>{slots.length}</b> 个账号（每个账号各发一次最小请求）
            {done ? <>，已完成 <b className='text-foreground'>{done}</b></> : null}。
          </p>
          {slots.map(slot => slotBlock(slot))}
        </>
      )
    }
    const okCount = slots.filter(isOk).length
    return (
      <>
        <p className='text-xs text-subtle'>
          本次 <b className='text-foreground'>{slots.length}</b> 个账号：
          <b className='text-foreground'>{okCount}</b> 成功 ·
          <b className='text-foreground'> {slots.length - okCount}</b> 失败
        </p>
        {slots.map(slot => slotBlock(slot))}
        {okCount === 0 ? (
          <NoteBlock>
            两边原因不一样时各自处理。如果<b className='text-foreground'>同一条错误出现在所有账号上</b>，
            通常就不是账号问题了：先确认这个模型还在上游目录里（「获取模型」刷一次），
            再看默认绑定指向的上游模型对不对。
          </NoteBlock>
        ) : null}
      </>
    )
  }

  return (
    // 参数层：受控 open（恒为 true）。本弹窗是「打开时建、关闭即卸」，关窗一律由 onClose 收口
    // （Esc / 点遮罩 / ✕ 都由 Base UI 汇到 onOpenChange）。
    <Dialog open onOpenChange={next => { if (!next) onClose() }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>测试模型{modelTag}</DialogTitle>
        </DialogHeader>
        <DialogBody>
          {/* 目标行：测的是哪一条（与「模型能力」弹窗的预览行同一用意） */}
          <div className='flex flex-col gap-2'>
            <div className='flex items-center gap-2 rounded-md border border-border bg-surface-inset px-3 py-2.5'>
              <span className='min-w-0 flex-1 truncate font-mono text-[12.5px] font-semibold text-primary-fg'
                title={model.id}>{model.id}</span>
              <Badge shape='tag' variant='brand'>{providerLabelOf(provider)}</Badge>
              <Badge shape='tag' variant='outline'>
                {customSource.isCustom(provider) ? '自定义家' : (SOURCE_LABEL[model.source] || '来源未知')}
              </Badge>
            </div>
            <p className='text-xs leading-[1.65] text-subtle'>
              本次以默认绑定的名字 <code className='text-foreground'>{model.id}</code> 发出去
              {aliases.length
                ? `（这一行另有 ${aliases.length} 条别名映射：${aliases.join('、')}，别名不参与本次测试）`
                : ''}
              ；映射上绑定的思考等级{boundLevel
                ? <> 是 <b className='text-foreground'>{boundLevel}</b>，选「跟随映射」时按它注入</>
                : '未绑定 —— 选「跟随映射」等于这次不注入等级'}。
            </p>
          </div>

          <div className='flex flex-col gap-1.5'>
            <Label htmlFor='model-test-system'>系统提示词</Label>
            <Textarea id='model-test-system' rows={2} maxLength={MAX_PROMPT_CHARS}
              placeholder='留空则本次不额外携带系统提示词（设置页的提示词模式仍按原样生效）'
              value={systemPrompt}
              onChange={event => setSystemPrompt(event.currentTarget.value)} />
            <span className='text-xs text-subtle'>留空 = 请求里只有下面那条用户消息；填了则排在它前面。</span>
          </div>

          <div className='flex flex-col gap-1.5'>
            <Label htmlFor='model-test-prompt'>用户提示词</Label>
            <Textarea id='model-test-prompt' rows={2} maxLength={MAX_PROMPT_CHARS}
              value={prompt}
              onChange={event => setPrompt(event.currentTarget.value)} />
            <span className='text-xs text-subtle'>留空则用默认的一句问候（最小请求）。</span>
          </div>

          <div className='grid grid-cols-2 gap-4'>
            <div className='flex flex-col gap-1.5'>
              <Label htmlFor='model-test-reasoning'>思考等级</Label>
              <Select value={reasoning} onValueChange={next => setReasoning(String(next))}>
                <SelectTrigger id='model-test-reasoning' className='w-full'>
                  <SelectValue>{reasoning ? `${reasoning}（本次指定）` : followLabel}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value=''>{followLabel}</SelectItem>
                  {levelOptions.map(level => (
                    <SelectItem key={level} value={level}>{level}（本次指定）</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className='flex flex-col gap-1.5'>
              <Label htmlFor='model-test-stream'>流式请求</Label>
              <div className='flex items-center gap-2'>
                <Switch id='model-test-stream' checked={stream}
                  onCheckedChange={value => setStream(Boolean(value))} />
                <span className='text-xs text-subtle'>默认开，与真实请求一致：能同时验证 SSE 链路与首字延迟</span>
              </div>
            </div>
          </div>

          <div className='flex flex-col gap-1.5'>
            <div className='flex items-center gap-2'>
              <Label htmlFor='model-test-accounts'>测试账号</Label>
              <span className='text-xs text-subtle'>（勾几个就并行测几次；默认勾队列里最靠前的那个）</span>
              <Button variant='ghost' size='xs' className='ml-auto'
                disabled={!usable.length || picked.length === usable.length}
                onClick={() => setPicked(usable.map(account => account.id))}>
                勾全部启用（{usable.length}）
              </Button>
            </div>
            <MultiSelect id='model-test-accounts' value={picked} onValueChange={setPicked}
              options={accountOptions} placeholder='选一个账号（至少一个）'
              searchPlaceholder='搜索账号…'
              emptyHint='这家没有可用账号（要在账号页启用一个、且凭证完整）' />
          </div>

          <NoteBlock>
            本次将发 <b className='text-foreground'>{picked.length}</b> 次最小请求
            （{picked.length} 个账号 × 1 个模型），走真实转发链路、
            <b className='text-foreground'>会消耗少量额度</b>；每次测试带「测试」来源标记记入请求日志，
            <b className='text-foreground'>不计入报表统计</b>。
          </NoteBlock>
        </DialogBody>
        <DialogFooter>
          <span className='mr-auto text-xs text-subtle'>开始后结果会在上一层弹窗里给出，按账号分开列。</span>
          <Button variant='outline' onClick={onClose}>关闭</Button>
          <Button variant='default' disabled={!picked.length || !usable.length} onClick={run}>开始测试</Button>
        </DialogFooter>

        {/* ── 结果层：叠在参数层之上的一层（位置与 overlayForceRender 的理由见模块头）── */}
        <Dialog open={slots.length > 0} onOpenChange={next => { if (!next) closeResults() }}>
          <DialogContent overlayForceRender className='w-[min(680px,calc(100vw-48px))]'>
            <DialogHeader>
              <DialogTitle>{running ? '测试中…' : '测试结果'}{modelTag}</DialogTitle>
            </DialogHeader>
            <DialogBody>{results()}</DialogBody>
            <DialogFooter>
              {running ? (
                <>
                  <span className='mr-auto text-xs text-subtle'>关掉这层 = 中止在途请求，不会继续占上游额度。</span>
                  <Button variant='outline' onClick={() => abort()}>中止测试</Button>
                  <Button variant='default' disabled>测试中…</Button>
                </>
              ) : (
                <>
                  <span className='mr-auto text-xs text-subtle'>
                    {slots.some(slot => !isOk(slot))
                      ? '每个账号按各自的原因给下一步；改参数请关掉这层。'
                      : '结果按账号分开：谁通谁不通一眼可辨。'}
                  </span>
                  <Button variant='outline' onClick={closeResults}>关闭</Button>
                  <Button variant='default' onClick={run}>再测一次</Button>
                </>
              )}
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </DialogContent>
    </Dialog>
  )
}
