import * as React from 'react'
import { createRoot } from 'react-dom/client'
import {
  Button,
  Dialog,
  DialogBody,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogSection,
  DialogTitle,
  Input,
  Label,
} from '@ui'
import { displayNameOf, providerFeatures, providerOf } from './accounts-domain'
import type { AccountRecord } from './accounts-shared'

/**
 * 账号「并发上限」编辑弹窗。
 *
 * 这是「把既有手写 UI 换成组件库」的第一块样板，替换的是 ui/account-conc-dialog.js
 * （那份用 innerHTML 拼 .modal-mask / .modal / .field-row 那套老类名）。
 *
 * 对外接口与原实现**完全一致**：`window.wbAccountConcDialog.open(account)` /
 * `.close()`，调用方（accounts-view.js 的 ⋯ 菜单）一行都不用改。
 * 差别只在这里：弹窗结构走组件库的 Dialog 一族、表单控件走 Input / Label，
 * 样式由组件库的 Tailwind 类提供，不再依赖 ui/css 的 .modal-* 与 .field-row。
 *
 * 挂载方式沿用原实现的「点击时动态建、关闭即移除」：打开时建一个宿主 div 挂
 * React root，关闭时 unmount 并摘掉宿主，不留常驻节点。
 */

/**
 * 账号公开形态里本弹窗用到的字段（其余字段不关心）。
 *
 * 直接复用 `AccountRecord` 而不是这里另写一份窄类型：能力表的读法（下面按
 * `concurrencyDefault` 分岔那句）要走 `providerOf(account)`，而那个函数的入参
 * 就是这个类型 —— 窄类型要么过不了类型检查，要么逼出一处 cast。
 */
type Account = AccountRecord

/** 与后端 apply_patch 的封顶值一致（store_crud 的 MAX_CONCURRENT_LIMIT） */
const MAX_LIMIT = 999

type ConcDialogProps = {
  account: Account
  onClose: () => void
}

function ConcDialog({ account, onClose }: ConcDialogProps) {
  // 非受控改受控：弹窗只有一个字段，受控让「保存中禁用」这类状态更好写
  const [value, setValue] = React.useState(String(Number(account.maxConcurrent) || 0))
  const [saving, setSaving] = React.useState(false)
  const [hint, setHint] = React.useState('')
  // 与账号列同一取名口径（displayNameOf 内含 nameCustom / emailAsName 分流）
  const name = displayNameOf(account) || account.id

  /**
   * ── 「0 = 不限」这一句要按家分岔 ────────────────────────────
   * CodeArts 的 3 是**上游硬顶**（超过它上游直接回 HTTP 400，且那是账号级冲突、
   * 不会降级换号），所以那一家把 0 解释成「按默认 3」而不是「不限制」，后端公开
   * 形态也会把一个没配过的账号报成 3（见 `to_codearts_public_account`）。
   * 这条知识放在能力表的 `concurrencyDefault` 里（>0 = 本家没有「不限」这一档），
   * 这里只读不说死 provider id —— 再加一家有同样约束的提供商，改的是能力表。
   */
  const floor = Number(providerFeatures(providerOf(account)).concurrencyDefault) || 0

  /**
   * 保存：PATCH `/api/accounts/{id}` 只带 `maxConcurrent` 一个字段（后端 apply_patch
   * 是 patch 语义，没传的字段一概不动）。成功先关窗再 toast「已保存」并刷新列表
   * —— 改动已落库，刷新只是让界面跟上，刷新失败不折进「保存失败」。
   *
   * 归一在本地先做：number 输入挡不住手工键入的脏值，负数/小数/超界都 clamp 到
   * 0~999 的整数，后端 400 只该是最后防线。
   */
  async function handleSave() {
    const raw = Number(value)
    if (!Number.isFinite(raw)) {
      window.wbApp?.toast?.(`并发上限必须是 0~${MAX_LIMIT} 的整数`, 'err')
      return
    }
    const next = Math.min(MAX_LIMIT, Math.max(0, Math.round(raw)))
    setSaving(true)
    try {
      await window.workbuddyDesktop.updateAccount(account.id, { maxConcurrent: next })
      onClose()
      window.wbApp?.toast?.('✅ 已保存')
      await window.wbApp?.refresh?.()
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      window.wbApp?.toast?.(`保存失败：${message}`, 'err')
      // 脚注留一份：toast 几秒后就没了
      setHint(message)
      setSaving(false)
    }
  }

  return (
    <Dialog open onOpenChange={next => { if (!next) onClose() }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>并发上限 · {name}</DialogTitle>
        </DialogHeader>
        <DialogBody>
          <DialogSection>
            <div className='flex flex-wrap items-center gap-2.5'>
              <Label htmlFor='acct-conc-input' className='text-[12.5px] whitespace-nowrap text-subtle'>
                同时处理的请求数
              </Label>
              <Input
                id='acct-conc-input'
                type='number'
                min={0}
                max={MAX_LIMIT}
                step={1}
                value={value}
                onChange={event => setValue(event.currentTarget.value)}
                className='max-w-[130px]'
                autoFocus
              />
            </div>
            <p>
              {floor > 0 ? (
                <>
                  该账号同时最多处理的请求数。本家受上游硬顶约束，<strong>0 表示按默认 {floor}</strong>，
                  不是不限制。达到上限的账号会跳过，请求转给其他账号；全部账号都达上限时按余量挤占。
                </>
              ) : (
                <>
                  该账号同时最多处理的请求数，0 表示不限制。达到上限的账号会跳过，请求转给其他账号；
                  全部账号都达上限时按余量挤占。
                </>
              )}
            </p>
          </DialogSection>
        </DialogBody>
        <DialogFooter>
          <span className='text-[11.5px] text-muted-foreground'>{hint}</span>
          <div className='mr-auto' />
          <Button variant='outline' onClick={onClose}>
            取消
          </Button>
          <Button variant='default' onClick={handleSave} disabled={saving}>
            {saving ? '保存中…' : '保存'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ─── 命令式外壳：与旧实现的 window.wbAccountConcDialog 接口一致 ─── */

let root: ReturnType<typeof createRoot> | null = null
let host: HTMLElement | null = null

function unmountDialog() {
  if (root) {
    root.unmount()
    root = null
  }
  if (host) {
    host.remove()
    host = null
  }
}

function openDialog(account: Account) {
  if (!account?.id) return
  // 重复打开时先拆掉上一份（原实现同样是 closeDialog() 打头）
  unmountDialog()
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  root.render(<ConcDialog account={account} onClose={unmountDialog} />)
}

declare global {
  interface Window {
    workbuddyDesktop: {
      updateAccount(id: string, patch: Record<string, unknown>): Promise<unknown>
    }
    wbApp?: {
      toast?: (message: string, kind?: 'err' | 'ok') => void
      refresh?: () => Promise<void> | void
    }
    wbAccountConcDialog?: {
      open(account: Account): void
      close(): void
    }
  }
}

window.wbAccountConcDialog = { open: openDialog, close: unmountDialog }
