import { toast } from '@/components/base/BaseToast'

export interface RequestOptions { signal?: AbortSignal, silent?: boolean }
let serial = 0
const pending = new Map<string, { resolve: (v: any) => void, reject: (e: Error) => void, timer: ReturnType<typeof setTimeout> }>()
addEventListener('message', (event) => {
  if (event.source !== parent || event.data?.type !== 'rs-plugin-result')
    return
  const entry = pending.get(event.data.id)
  if (!entry)
    return
  clearTimeout(entry.timer)
  pending.delete(event.data.id)
  if (event.data.error)
    entry.reject(new Error('插件操作失败，请刷新后重试'))
  else entry.resolve(event.data.data)
})
function invoke<T>(method: string, input: unknown): Promise<T> {
  return new Promise((resolve, reject) => {
    const id = String(++serial)
    const timer = setTimeout(() => {
      pending.delete(id)
      reject(new Error('插件响应超时'))
    }, 10000)
    pending.set(id, { resolve, reject, timer })
    parent.postMessage({ type: 'rs-plugin-call', id, method, input }, '*')
  })
}
export default async function request<T = any>(options: RequestOptions & { url: string, method?: string, data?: unknown }): Promise<T> {
  const methods: Record<string, string> = { '/api/admin/tickets': 'admin.panel', '/api/admin/tickets/probe': 'admin.probe', '/api/admin/tickets/continuous': 'admin.continuous', '/api/admin/tickets/logs/clear': 'admin.clear_logs', '/api/admin/tickets/proxy-exit': 'admin.exit', '/api/admin/proxies': 'admin.proxies' }
  const method = options.url === '/api/admin/tickets' && options.method === 'POST' ? 'admin.update' : methods[options.url]
  try {
    let value: any = await invoke(method, options.data ?? {})
    if (value?.jobId) {
      const id = value.jobId
      const deadline = Date.now() + 40000
      do {
        if (Date.now() > deadline)
          throw new Error('任务等待超时，请刷新日志查看结果')
        await new Promise(r => setTimeout(r, 500))
        value = await invoke('admin.job', { id })
      } while (value.pending)
      if (value.error)
        throw new Error(value.error)
      value = value.result
    }
    return value as T
  }
  catch (error) {
    if (!options.silent)
      toast.error(error instanceof Error ? error.message : '插件操作失败')
    throw error
  }
}
