<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api/modules/proxies'
import type { TicketAccount, TicketMode, TicketPanel, TicketPolicy, TicketProxyInput, TicketSettings } from '@/api/modules/tickets'
import { ChevronLeft, ChevronRight, Plus, Trash2 } from '@lucide/vue'
import { useEventListener, useIntervalFn, useNow } from '@vueuse/core'
import { computed, onMounted, ref, toRaw, watch } from 'vue'
import { getProxies } from '@/api/modules/proxies'
import { clearTicketLogs, continuousTicket, getTicketPanel, probeTicket, sampleTicketExit, saveTicketSettings } from '@/api/modules/tickets'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import BaseTag from '@/components/base/BaseTag.vue'
import { toast } from '@/components/base/BaseToast'

const panel = ref<TicketPanel | null>(null)
const draft = ref<TicketSettings | null>(null)
const draftRevision = ref(0)
let generation = 0
let polling = false
const loading = ref(false)
const saving = ref(false)
const probing = ref('')
const controlling = ref('')
const samplingExit = ref('')
const exitSamples = computed(() => new Map((panel.value?.proxies ?? []).map(p => [p.id, p.exitSample])))
watch(() => draft.value?.requireTicket, (enabled) => {
  if (enabled && draft.value)
    draft.value.inject = true
})
const manualProxies = ref<Record<string, string>>({})
const continuousIntervals = ref<Record<string, string>>({})
const manualProxyOptions = computed(() => [
  { label: '代理池轮换', value: 'pool' },
  ...(panel.value?.proxies ?? []).map(p => ({ label: `${p.name}${p.enabled ? '' : '（已禁用）'}`, value: p.id, disabled: !p.enabled })),
])
watch(() => panel.value?.revision, () => {
  manualProxies.value = {}
})
const proxy = ref('')
const clearProxy = ref(false)
const poolDraft = ref<(TicketProxyInput & { key: number, endpoint: string, hasAuthentication: boolean })[]>([])
let proxyKey = 0
const savedProxies = ref<OutboundProxyRecord[]>([])
const selectedProxy = ref('')
const importing = ref(false)
const savedProxyOptions = computed(() => savedProxies.value.map(p => ({ value: p.id, label: `${p.name} · ${p.endpoint}` })))
function addProxy() {
  poolDraft.value.push({ key: ++proxyKey, name: `代理 ${poolDraft.value.length + 1}`, url: '', endpoint: '', hasAuthentication: false, enabled: true, concurrency: 1 })
}
async function loadSavedProxies() {
  importing.value = true
  try {
    const items: OutboundProxyRecord[] = []
    for (let page = 1; ; page++) {
      const result = await getProxies({ page, pageSize: 100 })
      items.push(...result.items)
      if (page >= result.page.totalPages)
        break
    }
    savedProxies.value = items
    if (!items.length)
      toast.warning('RS 代理管理中暂无已保存代理')
  }
  catch {}
  finally { importing.value = false }
}
function importProxy() {
  const item = savedProxies.value.find(p => p.id === selectedProxy.value)
  if (!item)
    return
  poolDraft.value.push({ key: ++proxyKey, name: item.name, savedProxyId: item.id, endpoint: item.endpoint, hasAuthentication: item.hasAuthentication, enabled: true, concurrency: 1 })
  selectedProxy.value = ''
}
const search = ref('')
const logSearch = ref('')
const logOutcome = ref('all')
const logPage = ref(1)
const logPageSize = ref('25')
const logPageSizeOptions = [10, 25, 50, 100].map(n => ({ label: `每页 ${n} 条`, value: String(n) }))
const effectivePageSize = computed(() => [10, 25, 50, 100].includes(Number(logPageSize.value)) ? Number(logPageSize.value) : 25)
const confirmClearLogs = ref(false)
const clearingLogs = ref(false)
const lastStatusRefresh = ref<number | null>(null)
const statusRefreshFailed = ref(false)
const pendingNewLogs = ref(false)
const activeContinuous = computed(() => (panel.value?.accounts ?? []).flatMap(a => a.models.filter(m => m.continuous).map(m => ({ account: a.name, ...m }))))
const logOptions = [{ label: '全部结果', value: 'all' }, { label: '已命中', value: 'matched' }, { label: '未命中 / 失败', value: 'failed' }]
const filteredLogs = computed(() => (panel.value?.logs ?? []).filter((log) => {
  const text = `${log.accountName} ${log.result.accountId} ${log.result.model} ${log.proxyName ?? ''} ${log.proxyEndpoint} ${log.result.httpStatus} ${log.result.message}`.toLowerCase()
  return text.includes(logSearch.value.toLowerCase()) && (logOutcome.value === 'all' || log.result.matched === (logOutcome.value === 'matched'))
}))
const logPages = computed(() => Math.max(1, Math.ceil(filteredLogs.value.length / effectivePageSize.value)))
const visibleLogs = computed(() => filteredLogs.value.slice((logPage.value - 1) * effectivePageSize.value, logPage.value * effectivePageSize.value))
watch(() => panel.value?.logs[0]?.id, (id, previous) => {
  if (id && previous && id !== previous && logPage.value > 1)
    pendingNewLogs.value = true
})
const proxyStats = computed(() => {
  const stats = new Map<string, { endpoint: string, count: number, matched: number }>()
  for (const log of panel.value?.logs ?? []) {
    const item = stats.get(log.proxyEndpoint) ?? { endpoint: log.proxyEndpoint, count: 0, matched: 0 }
    item.count++
    item.matched += Number(log.result.matched)
    stats.set(item.endpoint, item)
  }
  return [...stats.values()].sort((a, b) => b.matched - a.matched || b.count - a.count)
})
watch([logSearch, logOutcome, logPageSize], () => {
  logPage.value = 1
})
watch(logPages, (pages) => {
  logPage.value = Math.min(logPage.value, pages)
})
const failure = ref('')
const models = ref('')
const policies = ref<Record<string, TicketPolicy>>({})
const customLengths = ref<Record<string, string>>({})
const clock = useNow({ interval: 1000 })
const modeOptions = [
  { label: '关闭打标', value: 'off' },
  { label: '仅手动', value: 'manual' },
  { label: '自动打标', value: 'auto' },
]
const rows = computed(() => (panel.value?.accounts ?? []).filter(a => `${a.name} ${a.id} ${a.plan ?? ''}`.toLowerCase().includes(search.value.toLowerCase())))
const readyCount = computed(() => panel.value?.accounts.reduce((n, a) => n + a.models.filter(ticketReady).length, 0) ?? 0)
const autoAccountCount = computed(() => Object.values(policies.value).filter(p => p.mode === 'auto').length)

function accept(data: TicketPanel) {
  lastStatusRefresh.value = Date.now() / 1000
  statusRefreshFailed.value = false
  panel.value = data
  draftRevision.value = data.revision
  draft.value = structuredClone(data.settings)
  draft.value.manualIntervalSeconds ??= 0
  draft.value.proxyPoolEnabled ??= true
  draft.value.activityOnly ??= false
  draft.value.idleSeconds ??= 120
  draft.value.requireTicket ??= false
  models.value = data.settings.models.join(', ')
  policies.value = Object.fromEntries(data.accounts.map(a => [a.id, { ...a.policy }]))
  customLengths.value = Object.fromEntries(data.accounts.map(a => [a.id, a.policy.targetLength?.toString() ?? '']))
  proxy.value = ''
  poolDraft.value = (data.proxies ?? []).map(p => ({ ...p, key: ++proxyKey, url: '', enabled: p.enabled ?? true, concurrency: p.concurrency ?? 1 }))
  clearProxy.value = false
}
async function load() {
  generation += 1
  loading.value = true
  failure.value = ''
  try {
    accept(await getTicketPanel())
  }
  catch { failure.value = '读取打标状态失败，请重试。' }
  finally { loading.value = false }
}
function setMode(id: string, value: string) {
  const old = policies.value[id]
  policies.value[id] = { mode: value as TicketMode, targetLength: old?.targetLength ?? null }
}
async function save() {
  if (!draft.value || !panel.value)
    return
  const settings = structuredClone(toRaw(draft.value))
  settings.models = models.value.split(/[,，\s]+/).filter(Boolean)
  settings.accounts = Object.fromEntries(Object.entries(policies.value).map(([id, policy]) => [id, {
    ...policy,
    targetLength: customLengths.value[id]?.trim() ? Number(customLengths.value[id]) : null,
  }]))
  const lengths = [settings.plusProLength, settings.businessLength, settings.defaultLength, ...Object.values(settings.accounts).flatMap(p => p.targetLength === null ? [] : [p.targetLength])]
  if (lengths.some(n => !Number.isInteger(n) || n < 64 || n > 4096)) {
    toast.warning('目标长度应为 64–4096 的整数')
    return
  }
  const ranges: [number, number, number, string][] = [
    [settings.intervalSeconds, 10, 86400, '自动探测间隔应为 10–86400 秒的整数'],
    [settings.manualIntervalSeconds, 0, 86400, '手动探测间隔应为 0–86400 秒的整数'],
    [settings.ttlSeconds, 60, 3600, '有效期应为 60–3600 秒的整数'],
    [settings.refreshBeforeSeconds, 0, settings.ttlSeconds - 1, '提前刷新时间必须为非负整数且小于有效期'],
    [settings.idleSeconds, 10, 3600, '空闲暂停时间应为 10–3600 秒'],
  ]
  for (const [value, min, max, message] of ranges) {
    if (!Number.isInteger(value) || value < min || value > max) {
      toast.warning(message)
      return
    }
  }
  if (!settings.models.length || settings.models.length > 8 || new Set(settings.models).size !== settings.models.length || settings.models.some(model => !/^[\w.-]{1,128}$/.test(model))) {
    toast.warning('请填写 1–8 个不重复的有效模型名称')
    return
  }
  const proxyPool = proxy.value.split(/\r?\n/).map(value => value.trim()).filter(Boolean)
  const proxies: TicketProxyInput[] = poolDraft.value.map(p => ({ id: p.id, name: p.name.trim(), url: p.url?.trim() || undefined, savedProxyId: p.savedProxyId, enabled: p.enabled ?? true, concurrency: p.concurrency ?? 1 }))
  proxies.push(...proxyPool.map((url, i) => ({ name: `代理 ${poolDraft.value.length + i + 1}`, url })))
  if (!clearProxy.value && proxies.length > 64) {
    toast.warning('代理池最多 64 个代理')
    return
  }
  if (!clearProxy.value && proxies.some(p => !p.name || (!p.id && !p.savedProxyId && !p.url))) {
    toast.warning('请填写代理名称和新代理完整地址')
    return
  }
  if (proxies.some(p => p.concurrency !== undefined && (!Number.isInteger(p.concurrency) || p.concurrency < 1 || p.concurrency > 3))) {
    toast.warning('每个代理入口并发应为 1–3')
    return
  }
  if (settings.enabled && (clearProxy.value || !proxies.length)) {
    toast.warning('启用打标需要配置代理池；清除代理池前请关闭打标')
    return
  }
  saving.value = true
  generation += 1
  try {
    accept(await saveTicketSettings({ revision: draftRevision.value, settings, proxies: clearProxy.value ? [] : proxies }))
    toast.success('策略已保存，仍符合规则的有效票已保留')
  }
  catch {}
  finally { saving.value = false }
}
async function probe(account: TicketAccount, model: string) {
  generation += 1
  probing.value = `${account.id}/${model}`
  try {
    const result = await probeTicket(manualInput(account.id, model))
    if (result.matched)
      toast.success(`${account.name}：已匹配，长度 ${result.length}`)
    else toast.warning(`${account.name}：HTTP ${result.httpStatus}，长度 ${result.length}，${result.message}`)
    // 仅更新状态，保留尚未保存的策略草稿。
    panel.value = await getTicketPanel()
  }
  catch {}
  finally { probing.value = '' }
}
function ticketRemaining(expiresAt: number | null | undefined) {
  return Math.max(0, Math.ceil((expiresAt ?? 0) - clock.value.getTime() / 1000))
}
function ticketReady(status: { ready: boolean, expiresAt: number | null }) {
  return status.ready && ticketRemaining(status.expiresAt) > 0
}
function countdown(expiresAt: number | null | undefined) {
  const seconds = ticketRemaining(expiresAt)
  if (!seconds)
    return '已到期'
  const hours = Math.floor(seconds / 3600)
  const minutes = Math.floor(seconds % 3600 / 60)
  const remainder = String(seconds % 60).padStart(2, '0')
  return hours ? `${hours}小时 ${String(minutes).padStart(2, '0')}分 ${remainder}秒` : `${minutes}分 ${remainder}秒`
}
function time(value: number | null | undefined) {
  return value ? new Date(value * 1000).toLocaleString() : '—'
}
async function sampleExit(id: string) {
  if (!panel.value || samplingExit.value)
    return
  samplingExit.value = id
  try {
    const sample = await sampleTicketExit({ proxyId: id, revision: panel.value.revision })
    const current = panel.value.proxies.find(p => p.id === id)
    if (current)
      current.exitSample = sample
    if (sample.ip)
      toast.success('已取得出口采样，结果缓存 10 分钟')
    else toast.warning(sample.message)
  }
  catch {}
  finally { samplingExit.value = '' }
}
function manualInput(accountId: string, model: string) {
  const proxyId = manualProxies.value[`${accountId}/${model}`] ?? 'pool'
  return { accountId, model, proxyId: proxyId === 'pool' ? undefined : proxyId, revision: panel.value?.revision }
}
async function continuous(account: TicketAccount, model: string, stop = false) {
  const key = `${account.id}/${model}`
  const interval = Number(continuousIntervals.value[key] ?? '10')
  if (!stop && (!Number.isInteger(interval) || interval < 10 || interval > 86400)) {
    toast.warning('持续打标间隔应为 10–86400 秒')
    return
  }
  generation += 1
  controlling.value = key
  try {
    panel.value = await continuousTicket({ ...manualInput(account.id, model), intervalSeconds: stop ? null : interval })
    if (!stop)
      showLatestLogs()
    toast.success(stop ? '持续打标已停止' : '已提交持续打标，命中后自动停止')
  }
  catch {}
  finally { controlling.value = '' }
}
onMounted(load)
function showLatestLogs() {
  logPage.value = 1
  logSearch.value = ''
  logOutcome.value = 'all'
  pendingNewLogs.value = false
}
async function clearLogs() {
  generation += 1
  clearingLogs.value = true
  try {
    panel.value = await clearTicketLogs()
    showLatestLogs()
    confirmClearLogs.value = false
    toast.success('历史记录已清理，有效票和正在运行的任务已保留')
  }
  catch {}
  finally { clearingLogs.value = false }
}
async function refreshStatus() {
  if (loading.value || saving.value || probing.value || controlling.value || clearingLogs.value || !panel.value || polling || document.hidden)
    return
  polling = true
  const started = generation
  try {
    const data = await getTicketPanel({ silent: true })
    if (started === generation) {
      panel.value = data
      lastStatusRefresh.value = Date.now() / 1000
      statusRefreshFailed.value = false
    }
  }
  catch { statusRefreshFailed.value = true }
  finally { polling = false }
}
useIntervalFn(refreshStatus, computed(() => activeContinuous.value.length ? 3000 : 10000))
useEventListener(document, 'visibilitychange', () => {
  if (!document.hidden)
    void refreshStatus()
})
</script>

<template>
  <div class="flex flex-col gap-6">
    <BasePageHeader title="打标管理" description="按账号管理 Turn-State 探测与复用">
      <template #actions>
        <BaseButton :disabled="loading || saving || !!probing" @click="load">
          刷新 / 重置草稿
        </BaseButton>
        <BaseButton variant="primary" :disabled="!draft || saving || !!probing" @click="save">
          {{ saving ? '保存中…' : '保存策略' }}
        </BaseButton>
      </template>
    </BasePageHeader>
    <p v-if="failure" role="alert" class="text-cp-error-text">
      {{ failure }}
    </p>
    <p v-if="loading && !panel" role="status" class="text-cp-text-secondary">
      正在读取账号与票状态…
    </p>
    <template v-if="panel && draft">
      <p v-if="draftRevision !== panel.revision" role="status" class="text-cp-warning-text">
        策略已在其他页面更新，请刷新后再编辑保存。
      </p>
      <BaseCard class="p-5">
        <div class="flex flex-wrap items-center justify-between gap-4">
          <div>
            <h2 class="text-xl font-semibold">
              运行策略
            </h2><p class="mt-1 text-cp-text-secondary">
              {{ panel.accounts.length }} 个 OAuth 账号 · {{ readyCount }} 张有效票
            </p>
          </div>
          <div class="flex flex-wrap gap-5">
            <BaseSwitch v-model="draft.enabled" label="启用打标" show-label />
            <BaseSwitch v-model="draft.inject" label="业务请求使用有效票" show-label :disabled="draft.requireTicket" />
          </div>
        </div>
        <div class="mt-4 flex flex-wrap gap-5">
          <BaseSwitch v-model="draft.activityOnly" label="有调用才自动打票" show-label />
          <BaseSwitch v-model="draft.requireTicket" label="6 / 5.6 无票禁止调用" show-label />
        </div>
        <p class="mt-3 text-xs text-cp-text-secondary">
          {{ draft.activityOnly ? `自动模式：空闲 ${draft.idleSeconds} 秒后休息，有新调用再开始。` : '自动模式：按设定间隔补票。' }}
          {{ draft.requireTicket ? '本页 OAuth 账号的 6 / 5.6 系列需有效票才能转发；目标模型需加入下方列表。' : '未获票时维持原转发。' }}
        </p>
        <p v-if="draft.activityOnly && !autoAccountCount" class="mt-2 text-xs text-cp-warning-text">
          当前账号未设为自动模式；请在下方选择需要自动补票的账号并保存。
        </p>
        <details class="mt-4 rounded-lg bg-cp-fill-quaternary p-3">
          <summary class="cursor-pointer font-semibold">
            高级规则与时效
          </summary>
          <p class="mt-2 text-xs text-cp-text-secondary">
            长度是匹配规则，不是官方质量指标。票内时间按 Fernet 格式读取创建时间，未验签，也不能据此推导真实过期时间。
          </p>
          <div class="mt-4 grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
            <label class="flex flex-col gap-2" for="ticket-idle">空闲暂停（秒）<input id="ticket-idle" v-model.number="draft.idleSeconds" type="number" min="10" max="3600" class="rounded-lg bg-cp-fill-tertiary p-3"></label>
            <label class="flex flex-col gap-2" for="ticket-plus">Plus / Pro 默认长度<input id="ticket-plus" v-model.number="draft.plusProLength" type="number" min="64" max="4096" class="rounded-lg bg-cp-fill-tertiary p-3"></label>
            <label class="flex flex-col gap-2" for="ticket-business">Business / Team 默认长度<input id="ticket-business" v-model.number="draft.businessLength" type="number" min="64" max="4096" class="rounded-lg bg-cp-fill-tertiary p-3"></label>
            <label class="flex flex-col gap-2" for="ticket-default">其他套餐默认长度<input id="ticket-default" v-model.number="draft.defaultLength" type="number" min="64" max="4096" class="rounded-lg bg-cp-fill-tertiary p-3"></label>
            <label class="flex flex-col gap-2" for="ticket-interval">自动探测间隔（秒）<input id="ticket-interval" v-model.number="draft.intervalSeconds" type="number" min="10" max="86400" step="1" class="rounded-lg bg-cp-fill-tertiary p-3"></label>
            <label class="flex flex-col gap-2" for="ticket-manual-interval">手动探测间隔（秒，0 为无等待）<input id="ticket-manual-interval" v-model.number="draft.manualIntervalSeconds" type="number" min="0" max="86400" step="1" class="rounded-lg bg-cp-fill-tertiary p-3"></label>
            <label class="flex flex-col gap-2" for="ticket-ttl">有效期（秒，最多 3600）<input id="ticket-ttl" v-model.number="draft.ttlSeconds" type="number" min="60" max="3600" class="rounded-lg bg-cp-fill-tertiary p-3"></label>
            <label class="flex flex-col gap-2" for="ticket-refresh">提前刷新（秒）<input id="ticket-refresh" v-model.number="draft.refreshBeforeSeconds" type="number" min="0" :max="draft.ttlSeconds - 1" class="rounded-lg bg-cp-fill-tertiary p-3"></label>
          </div>
          <div class="mt-4 flex flex-col gap-2">
            <span>目标模型（逗号分隔）</span><BaseInput id="ticket-models" v-model="models" aria-label="目标模型（逗号分隔）" />
          </div>
        </details>
        <details class="mt-3 rounded-lg bg-cp-fill-quaternary p-3" :open="!panel.proxyCount">
          <summary class="cursor-pointer font-semibold">
            代理与出口 · {{ panel.proxies.filter(p => p.enabled).length }} / {{ panel.proxyCount }} 已启用
          </summary>
          <div class="mt-4">
            <label class="flex flex-col gap-2" for="ticket-proxy">
              <span>批量追加代理</span><textarea id="ticket-proxy" v-model="proxy" aria-label="批量追加代理" autocomplete="off" spellcheck="false" placeholder="每行一个完整代理地址" class="min-h-24 rounded-lg bg-cp-fill-tertiary p-3 font-mono text-sm" :disabled="clearProxy" />
            </label>
          </div>
          <div class="mt-4 flex flex-wrap items-center gap-3">
            <h3 class="font-semibold">
              已保存 {{ panel.proxyCount }} 个代理
            </h3>
            <BaseSwitch v-model="draft.proxyPoolEnabled" label="启用打标代理池" show-label />
            <BaseButton :disabled="clearProxy || poolDraft.length >= 64" title="新增代理" aria-label="新增代理" @click="addProxy">
              <Plus :size="16" />
            </BaseButton>
            <BaseButton :disabled="importing" @click="loadSavedProxies">
              {{ importing ? '读取中…' : '读取 RS 已有代理' }}
            </BaseButton>
            <BaseSelect v-if="savedProxies.length" v-model="selectedProxy" :options="savedProxyOptions" aria-label="选择已有代理" class="w-full sm:w-80" />
            <BaseButton v-if="savedProxies.length" :disabled="!selectedProxy || clearProxy" @click="importProxy">
              加入打标池
            </BaseButton>
          </div>
          <div v-for="entry in poolDraft" :key="entry.key" class="mt-3 flex flex-wrap items-center gap-3">
            <BaseSwitch v-model="entry.enabled" :label="`启用 ${entry.name || '代理'}`" :disabled="clearProxy" />
            <BaseInput v-model="entry.name" aria-label="代理名称" class="w-full sm:w-44" :disabled="clearProxy" />
            <span class="min-w-0 flex-1 break-all font-mono text-sm">{{ entry.endpoint || '新代理' }} · {{ entry.hasAuthentication ? '已保存认证' : '无已保存认证' }}</span>
            <BaseInput v-if="!entry.savedProxyId" v-model="entry.url" :aria-label="`${entry.name} 代理地址`" type="password" autocomplete="new-password" :placeholder="entry.id !== undefined ? '留空保留地址及认证；输入完整 URL 替换' : 'socks5://用户名:密码@主机:端口'" class="w-full sm:w-80" :disabled="clearProxy" />
            <span v-else class="text-sm text-cp-text-secondary">保存时导入认证</span>
            <BaseSelect :model-value="String(entry.concurrency ?? 1)" :options="[{ label: '并发 1', value: '1' }, { label: '并发 2', value: '2' }, { label: '并发 3', value: '3' }]" :aria-label="`${entry.name} 并发数`" :disabled="clearProxy" class="w-28" @update:model-value="entry.concurrency = Number($event)" />
            <BaseButton :aria-label="`检测 ${entry.name} 出口 IP`" :disabled="entry.id === undefined || !!samplingExit || saving || draftRevision !== panel.revision || !!entry.url?.trim()" @click="sampleExit(entry.id!)">
              {{ samplingExit === entry.id ? '检测中…' : '检测出口 IP' }}
            </BaseButton>
            <span v-if="exitSamples.get(entry.id ?? '')" class="w-full text-xs text-cp-text-secondary">
              出口采样：{{ exitSamples.get(entry.id ?? '')?.ip || exitSamples.get(entry.id ?? '')?.message }} · {{ time(exitSamples.get(entry.id ?? '')?.checkedAt) }}
              {{ (exitSamples.get(entry.id ?? '')?.checkedAt ?? 0) + 600 <= clock.getTime() / 1000 ? '（采样已过期）' : '（独立连接，非本次打标确认）' }}
            </span>
            <BaseButton :aria-label="`移除 ${entry.name}`" title="移除代理" :disabled="clearProxy" @click="poolDraft = poolDraft.filter(p => p.key !== entry.key)">
              <Trash2 :size="16" />
            </BaseButton>
          </div>
          <div class="mt-4">
            <p class="mb-3 text-sm text-cp-text-secondary">
              暂停代理池或禁用单个入口会保留配置和已获有效票，已发出的探测会正常结束。并发按代理入口计算，整个服务最多同时 12 个探测。
            </p>
            <BaseSwitch v-model="clearProxy" label="清除已保存的代理（需同时关闭打标）" show-label />
          </div>
        </details>
      </BaseCard>
      <BaseCard class="p-5">
        <div class="mb-5 flex flex-wrap items-center justify-between gap-4">
          <h2 class="text-xl font-semibold">
            账号与手动探测
          </h2>
          <BaseInput v-model="search" aria-label="搜索账号" placeholder="搜索账号、套餐或 ID" class="w-full sm:w-72" />
        </div>
        <p class="mb-4 text-sm text-cp-text-secondary">
          模式变更需保存；持续模式按入口并发配置探测，命中后停止。后台检查：{{ time(panel.workerCheckedAt) }}。
        </p>
        <p v-if="!rows.length" class="py-8 text-center text-cp-text-secondary">
          暂无匹配的 OAuth 账号
        </p>
        <div v-else class="flex flex-col gap-4">
          <article v-for="account in rows" :key="account.id" class="rounded-xl bg-cp-fill-quaternary p-4">
            <div class="flex flex-wrap items-start justify-between gap-4">
              <div class="min-w-0">
                <h3 class="break-all font-semibold">
                  {{ account.name }}
                </h3><p class="mt-1 text-sm text-cp-text-secondary">
                  {{ account.plan || '未知套餐' }} · 已保存目标 {{ account.targetLength }} · {{ account.eligible ? '可探测' : '账号当前不可探测' }}
                </p>
              </div>
              <div class="flex flex-wrap gap-3">
                <BaseSelect :model-value="policies[account.id]?.mode ?? 'off'" :options="modeOptions" :aria-label="`${account.name} 打标模式`" class="w-36" @update:model-value="setMode(account.id, $event)" />
                <BaseInput v-model="customLengths[account.id]" :aria-label="`${account.name} 自定义长度`" placeholder="留空跟随套餐" type="number" min="64" max="4096" class="w-40" />
              </div>
            </div>
            <div class="mt-4 grid gap-3 lg:grid-cols-2">
              <div v-for="status in account.models" :key="status.model" class="rounded-lg p-4" :class="ticketReady(status) ? 'bg-cp-success-container' : 'bg-cp-bg-container'">
                <div class="flex flex-wrap items-center justify-between gap-2">
                  <span class="font-mono text-sm">{{ status.model }}</span>
                  <BaseTag :type="ticketReady(status) ? 'success' : status.blocked ? 'warning' : 'neutral'" round>
                    {{ ticketReady(status) ? '✓ 已打到 · 有效票' : status.blocked ? '无票 · 调用拦截' : '暂无有效票' }}
                  </BaseTag>
                </div>
                <p class="mt-2 text-sm text-cp-text-secondary">
                  最近：{{ status.lastResult ? `HTTP ${status.lastResult.httpStatus} / 长度 ${status.lastResult.length}` : '尚未探测' }}
                </p>
                <p class="mt-1 text-xs text-cp-text-secondary">
                  本地到期：{{ time(status.expiresAt) }}
                  <span v-if="status.expiresAt" class="ml-2 inline-block font-semibold tabular-nums" :class="ticketRemaining(status.expiresAt) > 60 ? 'text-cp-success-text' : 'text-cp-warning-text'">{{ ticketRemaining(status.expiresAt) ? `剩余 ${countdown(status.expiresAt)}` : '已到期' }}</span>
                  <span v-if="status.manualRetryAt"> · 下次可手动探测：{{ time(status.manualRetryAt) }}</span>
                </p>
                <p v-if="account.policy.mode === 'auto' && panel.settings.activityOnly" class="mt-1 text-xs text-cp-text-secondary">
                  {{ status.autoPaused ? '自动休息 · 等待业务调用' : '近期有调用 · 自动补票已激活' }}
                </p>
                <details v-if="status.tokenIssuedAt || status.lastRequestedAt" class="mt-2 text-xs text-cp-text-secondary">
                  <summary class="cursor-pointer">
                    票据时间与最近调用
                  </summary>
                  <p class="mt-1">
                    票内创建时间（未验签）：{{ time(status.tokenIssuedAt) }}
                  </p>
                  <p class="mt-1">
                    最近调用：{{ time(status.lastRequestedAt) }}
                  </p>
                </details>
                <div class="mt-3 flex flex-wrap gap-3">
                  <BaseSelect :model-value="status.continuous ? (status.continuous.proxyId ?? 'pool') : (manualProxies[`${account.id}/${status.model}`] ?? 'pool')" :options="manualProxyOptions" :disabled="!!status.continuous || status.busy" :aria-label="`${account.name} ${status.model} 手动代理`" class="min-w-40 flex-1" @update:model-value="manualProxies[`${account.id}/${status.model}`] = $event" />
                  <BaseInput :model-value="status.continuous?.intervalSeconds.toString() ?? continuousIntervals[`${account.id}/${status.model}`] ?? '10'" :disabled="!!status.continuous" :aria-label="`${account.name} ${status.model} 持续间隔（秒）`" type="number" min="10" max="86400" class="w-28" @update:model-value="continuousIntervals[`${account.id}/${status.model}`] = String($event)" />
                </div>
                <p class="mt-1 text-xs text-cp-text-secondary">
                  右侧为批次间隔（秒），默认 10。
                </p>
                <div class="mt-3 flex flex-wrap gap-2">
                  <BaseButton :disabled="!!probing || saving || !!controlling || !!status.continuous || !panel.settings.enabled || !panel.settings.proxyPoolEnabled || !panel.proxies.some(p => p.enabled) || !account.eligible || account.policy.mode === 'off' || status.busy || !!(status.manualRetryAt && status.manualRetryAt > clock.getTime() / 1000)" @click="probe(account, status.model)">
                    {{ probing === `${account.id}/${status.model}` || status.busy ? '探测中…' : '手动打一张' }}
                  </BaseButton>
                  <BaseButton v-if="status.continuous" :disabled="!!controlling" @click="continuous(account, status.model, true)">
                    {{ controlling === `${account.id}/${status.model}` ? '停止中…' : '停止持续打标' }}
                  </BaseButton>
                  <BaseButton v-else variant="primary" :disabled="!!probing || saving || !!controlling || !panel.settings.enabled || !panel.settings.proxyPoolEnabled || !panel.proxies.some(p => p.enabled) || !account.eligible || account.policy.mode === 'off' || status.busy || ticketReady(status)" @click="continuous(account, status.model)">
                    持续打标
                  </BaseButton>
                </div>
                <p v-if="status.continuous" role="status" class="mt-2 text-xs text-cp-text-secondary">
                  {{ status.busy ? '持续打标正在探测…' : `持续打标等待中 · 下次检查 ${time(status.continuous.nextProbeAt)}` }}
                </p>
              </div>
            </div>
          </article>
        </div>
      </BaseCard>
      <section aria-labelledby="ticket-log-title" class="min-w-0">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <h2 id="ticket-log-title" class="text-xl font-semibold">
            打标日志
          </h2>
          <span class="text-sm text-cp-text-secondary">最近 {{ panel.logs?.length ?? 0 }} / {{ panel.logLimit ?? 1000 }} 条</span>
          <div class="flex gap-2">
            <BaseButton :disabled="polling || clearingLogs" @click="refreshStatus">
              刷新记录
            </BaseButton>
            <BaseButton :disabled="!panel.logs?.length || clearingLogs" @click="confirmClearLogs = true">
              清理记录
            </BaseButton>
          </div>
        </div>
        <p class="mt-2 text-sm text-cp-text-secondary">
          出口 IP 是独立连接采样，不是本次打标的出口证明；在代理设置中按需检测，缓存 10 分钟。
        </p>
        <p class="mt-2 text-xs text-cp-text-secondary" role="status">
          {{ statusRefreshFailed ? '状态刷新失败，请点击刷新记录重试。' : `最近刷新：${time(lastStatusRefresh)}` }}
          记录在每次探测完成后出现；等待间隔和退避期间不会新增记录。
        </p>
        <p v-for="job in activeContinuous" :key="`${job.account}/${job.model}`" class="mt-2 text-sm text-cp-text-secondary">
          {{ job.account }} · {{ job.model }} · {{ job.busy ? '持续打标请求进行中…' : `持续打标等待中，下次检查 ${time(job.continuous?.nextProbeAt)}` }}
        </p>
        <BaseButton v-if="pendingNewLogs || logSearch || logOutcome !== 'all' || logPage > 1" class="mt-2" @click="showLatestLogs">
          {{ pendingNewLogs ? '有新记录，查看最新' : '清除筛选并查看最新' }}
        </BaseButton>
        <details v-if="proxyStats.length" class="mt-3">
          <summary class="cursor-pointer text-sm text-cp-text-secondary">
            代理命中统计
          </summary>
          <div class="mt-2 overflow-x-auto">
            <table class="w-full text-left text-sm">
              <thead>
                <tr>
                  <th class="p-2">
                    代理地址
                  </th><th class="p-2">
                    次数
                  </th><th class="p-2">
                    命中
                  </th><th class="p-2">
                    命中率
                  </th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="item in proxyStats" :key="item.endpoint">
                  <td class="break-all p-2 font-mono">
                    {{ item.endpoint }}
                  </td><td class="p-2">
                    {{ item.count }}
                  </td><td class="p-2">
                    {{ item.matched }}
                  </td><td class="p-2">
                    {{ (100 * item.matched / item.count).toFixed(1) }}%
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </details>
        <div class="my-4 flex flex-wrap gap-3">
          <BaseInput v-model="logSearch" aria-label="搜索打标日志" placeholder="账号、模型、代理 IP 或状态" class="w-full sm:w-80" />
          <BaseSelect v-model="logOutcome" :options="logOptions" aria-label="打标日志结果筛选" class="w-44" />
        </div>
        <p v-if="!visibleLogs.length" class="py-6 text-cp-text-secondary">
          暂无匹配的打标记录
        </p>
        <div v-else class="overflow-x-auto">
          <table class="w-full min-w-[960px] text-left text-sm">
            <thead>
              <tr>
                <th class="p-3">
                  时间 / 触发
                </th><th class="p-3">
                  账号 / 模型
                </th><th class="p-3">
                  代理地址
                </th><th class="p-3">
                  HTTP / 长度
                </th><th class="p-3">
                  结果 / 耗时
                </th><th class="p-3">
                  详情
                </th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="log in visibleLogs" :key="log.id" :class="log.result.matched ? 'bg-cp-success-container/50' : 'odd:bg-cp-fill-quaternary'">
                <td class="p-3 align-top">
                  {{ time(log.startedAt) }}<div class="mt-1 text-cp-text-secondary">
                    {{ log.trigger === 'auto' ? '自动' : log.continuous ? '手动持续' : '手动单次' }}
                  </div>
                </td>
                <td class="max-w-56 break-all p-3 align-top">
                  {{ log.accountName }}<div class="mt-1 font-mono text-xs">
                    {{ log.result.accountId }}
                  </div><div class="mt-1 font-mono text-xs">
                    {{ log.result.model }}
                  </div>
                </td>
                <td class="max-w-64 break-all p-3 align-top font-mono text-xs">
                  <div v-if="log.proxyName" class="mb-1 font-sans">
                    {{ log.proxyName }}
                  </div>
                  {{ log.proxyEndpoint }}
                  <div v-if="log.exitSample?.ip" class="mt-1 font-sans text-cp-text-secondary">
                    采样 IP：{{ log.exitSample.ip }}<br>{{ time(log.exitSample.checkedAt) }}（非本次确认）
                  </div>
                </td>
                <td class="p-3 align-top">
                  {{ log.result.httpStatus || '无响应' }}<div class="mt-1">
                    {{ log.result.length }} / {{ log.targetLength }}
                  </div>
                </td>
                <td class="p-3 align-top">
                  <BaseTag :type="log.result.matched ? 'success' : 'warning'" round>
                    {{ log.result.matched ? '✓ 已命中' : '未命中' }}
                  </BaseTag><div class="mt-1">
                    {{ log.durationMs }} ms
                  </div>
                </td>
                <td class="max-w-80 break-words p-3 align-top">
                  {{ log.result.message }}<div v-if="log.tokenIssuedAt" class="mt-1 text-xs">
                    票内创建：{{ time(log.tokenIssuedAt) }}（未验签）
                  </div><div v-if="log.retryAt" class="mt-1 text-xs">
                    自动退避至 {{ time(log.retryAt) }}
                  </div><details class="mt-1 text-xs text-cp-text-tertiary">
                    <summary class="cursor-pointer">
                      记录编号
                    </summary><span class="break-all font-mono">{{ log.id }}</span>
                  </details>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div class="mt-4 flex flex-wrap items-center justify-end gap-3">
          <BaseSelect v-model="logPageSize" :options="logPageSizeOptions" aria-label="每页打标记录数" class="w-36" />
          <span class="text-sm text-cp-text-secondary">{{ filteredLogs.length }} 条 · {{ logPage }} / {{ logPages }}</span>
          <BaseButton aria-label="上一页日志" title="上一页日志" :disabled="logPage <= 1" @click="logPage--">
            <ChevronLeft :size="16" />
          </BaseButton>
          <BaseButton aria-label="下一页日志" title="下一页日志" :disabled="logPage >= logPages" @click="logPage++">
            <ChevronRight :size="16" />
          </BaseButton>
        </div>
      </section>
      <BaseConfirmModal v-model="confirmClearLogs" title="清理打标记录" confirm-text="确认清理" destructive :loading="clearingLogs" @confirm="clearLogs">
        清理全部历史记录及其统计，不可撤销。有效票、代理设置、退避和持续任务保持不变，之后完成的探测会生成新记录。
      </BaseConfirmModal>
    </template>
  </div>
</template>
