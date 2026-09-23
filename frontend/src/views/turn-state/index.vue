<script setup lang="ts">
import type { getAccounts, TurnStateInstallation, TurnStateObservation, TurnStateStatus } from '@/api'
import { Check, Cookie, Copy, Eye, LockKeyhole, Play, Plus, RefreshCw, Settings2, Trash2, Unlock } from '@lucide/vue'
import { useIntervalFn } from '@vueuse/core'
import { computed, onMounted, ref, watch } from 'vue'
import { applyTurnState, applyTurnStateCookie, copyTurnState, copyTurnStateCookie, defaultTurnStateConfig, getAccounts as fetchAccounts, getTurnStateStatus, probeTurnState, removeTurnState, removeTurnStateCookie } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { toast } from '@/components/base/BaseToast'
import { useCopyText } from '@/composables/useCopyText'
import TurnStateConfigModal from './TurnStateConfigModal.vue'

const buckets = ref<TurnStateStatus[]>([])
const accountFilter = ref('')
const selectedKey = ref('')
const accounts = ref<Awaited<ReturnType<typeof getAccounts>>['items']>([])
const error = ref('')
const autoRefresh = ref(true)
const tab = ref('probe')
const page = ref(1)
const pageSize = ref(20)
const showConfig = ref(false)
const editing = ref<TurnStateStatus | null>(null)
const probing = ref(new Set<string>())
const applying = ref(false)
const removing = ref(false)
const now = ref(Date.now() / 1000)
const loading = ref(false)
const copying = ref(false)
const cookieApplying = ref(false)
const cookieRemoving = ref(false)
const copyText = useCopyText()
let loadVersion = 0
const tableBuckets = computed<TurnStateStatus[]>(() => {
  const configured = new Set(buckets.value.map(bucket => bucket.accountId))
  const unconfigured = accounts.value.filter(account => account.authenticationKind === 'oauth' && !configured.has(account.id) && (!accountFilter.value || account.id === accountFilter.value)).map(account => ({
    accountId: account.id,
    accountName: account.name,
    accountEmail: account.email,
    model: 'gpt-6-astra',
    config: defaultTurnStateConfig(),
    tokenLength: null,
    issuedAt: null,
    ageSeconds: null,
    active: false,
    hasInstalledState: false,
    businessStatus: !account.enabled ? 'manual_disabled' as const : account.status === 'normal' ? 'ready' as const : account.status === 'error' || account.status === 'disabled' ? 'account_error' as const : account.status,
    accountEnabled: account.enabled,
    huntAttempts: 0,
    nextProbeAt: null,
    observations: [],
    installations: [],
    configured: false,
  }))
  return [...buckets.value.map((bucket) => {
    const account = accounts.value.find(account => account.id === bucket.accountId)
    return { ...bucket, accountEmail: bucket.accountEmail ?? account?.email }
  }), ...unconfigured]
})
const selection = computed(() => tableBuckets.value.find(bucket => bucketKey(bucket) === selectedKey.value))
const accountOptions = computed(() => [{ label: '全部账号', value: '' }, ...accounts.value.map(account => ({ label: `${account.email?.trim() || account.name} · ${account.id}`, value: account.id }))])
const bucketOptions = computed(() => tableBuckets.value.map(bucket => ({ label: `${bucket.accountEmail?.trim() || bucket.accountName} · ${bucket.accountId} / ${bucket.model}`, value: bucketKey(bucket) })))
const observations = computed(() => selection.value?.observations.filter(item => item.source === tab.value) ?? [])
const installations = computed(() => selection.value?.installations ?? [])
const total = computed(() => tab.value === 'history' ? installations.value.length : observations.value.length)
const distribution = computed(() => {
  const counts = new Map<string, number>()
  for (const item of selection.value?.observations.filter(item => item.source === 'probe' && item.probeId) ?? []) {
    const label = item.tokenLength !== null ? String(item.tokenLength) : outcome(item.outcome)
    counts.set(label, (counts.get(label) ?? 0) + 1)
  }
  return [...counts.entries()]
})
const columns = defineTableColumns<TurnStateStatus>([
  { key: 'accountName', label: '账号 / 模型', kind: 'identity', size: '3xl' },
  { key: 'enabled', label: '账号 / 自动探测', kind: 'status', size: 'lg' },
  { key: 'businessStatus', label: '业务调度', kind: 'status', size: 'xl' },
  { key: 'state', label: '当前路由票', kind: 'custom', size: 'lg' },
  { key: 'recentProbe', label: '最近探测', kind: 'custom', size: 'lg' },
  { key: 'nextProbeAt', label: '预计下次探测', kind: 'datetime', format: (value, row) => row.manualProbeRequestedAt != null ? '手动探测待执行' : value !== null && Number(value) <= now.value ? '即将开始' : date(value as number | null) },
  { key: 'actions', label: '操作', kind: 'actions', size: 'xl' },
])
const logColumns = defineTableColumns<TurnStateObservation>([
  { key: 'observedAt', label: '时间', kind: 'datetime', format: value => date(value as number) },
  { key: 'outcome', label: '结果', kind: 'text', size: 'xl', format: value => outcome(String(value)) },
  { key: 'requestStateSource', label: '请求 state 来源', kind: 'text', size: 'xl', format: value => requestStateSource(value as string | null) },
  { key: 'responseSource', label: '观测位置', kind: 'text', size: 'lg', format: value => responseSource(value as string | null) },
  { key: 'probeTrigger', label: '触发方式', kind: 'text', size: 'sm', format: value => value === 'manual' ? '手动' : value === 'scheduled' ? '自动' : '-' },
  { key: 'httpStatus', label: 'HTTP', kind: 'numeric', size: 'sm' },
  { key: 'tokenLength', label: '实际长度', kind: 'numeric', size: 'sm' },
  { key: 'oailbHost', label: 'Cookie pod', kind: 'text', size: '3xl', format: value => value ?? '-' },
  { key: 'cookieExpiresAt', label: 'Cookie 到期', kind: 'datetime', format: value => date(value as number | null) },
  { key: 'reportedModel', label: '上游模型', kind: 'text', size: 'lg', format: value => value ?? '-' },
  { key: 'egress', label: '出口', kind: 'text' },
  { key: 'shape', label: 'Shape', kind: 'text', size: 'sm' },
  { key: 'effort', label: 'Effort', kind: 'text', size: 'sm' },
  { key: 'elapsedMs', label: '耗时（ms）', kind: 'numeric' },
  { key: 'stopMode', label: '中断策略', kind: 'text', size: 'lg', format: value => value === 'headers' ? '获得 state' : value === 'first_output' ? '获得回应' : '-' },
  { key: 'stopReason', label: '中断原因', kind: 'text', size: 'lg' },
  { key: 'answer', label: '当前答案', kind: 'custom', size: '3xl' },
  { key: 'probeId', label: '请求 ID', kind: 'mono', size: '3xl' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'lg' },
])
const historyColumns = defineTableColumns<TurnStateInstallation>([
  { key: 'installedAt', label: '安装时间', kind: 'datetime', format: value => date(value as number) },
  { key: 'issuedAt', label: '签发时间', kind: 'datetime', format: value => date(value as number) },
  { key: 'tokenLength', label: '长度', kind: 'numeric' },
  { key: 'acquiredAt', label: '获取时间', kind: 'datetime', format: value => date(value as number) },
  { key: 'attempts', label: '尝试次数', kind: 'numeric', format: (value, row) => row.source === 'probe' && row.attempts === 0 ? '未记录' : value },
  { key: 'huntSeconds', label: '获取耗时（秒）', kind: 'numeric', format: (value, row) => row.source === 'probe' && row.attempts === 0 ? '未记录' : value },
  { key: 'source', label: '来源', kind: 'text', format: value => value === 'probe' ? '主动探测' : '被动采集' },
])

function bucketKey(bucket: TurnStateStatus) {
  return `${bucket.accountId}/${bucket.model}`
}
function date(value: number | null) {
  return value === null ? '-' : new Date(value * 1000).toLocaleString()
}
function age(value: number | null) {
  return value === null ? '-' : `${Math.floor(value / 60)} 分 ${value % 60} 秒`
}
function expiresAt(bucket: TurnStateStatus) {
  if (bucket.config.cookieLockEnabled)
    return bucket.routingCookie?.expiresAt ?? null
  return bucket.issuedAt === null ? null : bucket.issuedAt + bucket.config.ttlSeconds
}
function expired(bucket: TurnStateStatus) {
  const expiry = expiresAt(bucket)
  return expiry !== null && expiry <= now.value
}
function businessStatus(bucket: TurnStateStatus) {
  if (!bucket.accountEnabled)
    return 'manual_disabled'
  if (bucket.businessStatus === 'ready' && bucket.config.missingStatePolicy === 'pause' && (!bucket.active || expired(bucket)))
    return 'waiting_for_state'
  return bucket.businessStatus
}
function businessLabel(bucket: TurnStateStatus) {
  const labels: Record<TurnStateStatus['businessStatus'], string> = { ready: '正常调度', manual_disabled: '手动停用', waiting_for_state: bucket.config.cookieLockEnabled ? '等待 Cookie' : '等待 state', model_denied: '模型权限禁止', quota_exhausted: '额度耗尽', rate_limited: '限流 / 冷却中', account_error: '账号不可用' }
  return labels[businessStatus(bucket)]
}
function recentProbe(bucket: TurnStateStatus) {
  return bucket.observations.find(item => item.source === 'probe')
}
function clippedAnswer(value: string) {
  const limit = 120
  return value.length > limit ? `${value.slice(0, limit)}…` : value
}
function requestStateSource(value: string | null) {
  const labels: Record<string, string> = { none: '未携带', client: '客户端 / 会话', automatic_override: '模型桶自动安装', bucket_manual_override: '模型桶手动应用', account_override: '账号通用覆盖', manual_override: '手动覆盖（历史记录）' }
  return value ? labels[value] ?? value : '未记录'
}
function responseSource(value: string | null) {
  const labels: Record<string, string> = { http_headers: 'HTTP 响应头', websocket_start: 'WS 响应起始', websocket_metadata: 'WS 元数据', response_created: '响应模型声明' }
  return value ? labels[value] ?? value : '未记录'
}
function averageRetries(bucket: TurnStateStatus) {
  const samples = bucket.installations.filter(item => item.source === 'probe' && item.attempts > 0)
  return samples.length ? (samples.reduce((sum, item) => sum + Math.max(0, item.attempts - 1), 0) / samples.length).toFixed(1) : '-'
}
function outcome(value: string) {
  const labels: Record<string, string> = { cookie_ready: 'Cookie 可用', cookie_gateway_filtered: '网关编号未允许', cookie_model_mismatch: 'pod 模型不符', missing_cookie: '未获得路由 Cookie', missing_model: '缺少模型声明', cookie_deleted: 'Cookie 已失效', candidate: '有效候选', length_miss: '长度未命中', missing_header: '无响应头', transport_error: '传输错误', invalid_token: '无效令牌', expired_or_future: '签发时间失效', http_error: 'HTTP 错误', access_token_expired_or_unknown: '凭据过期或时间未知', account_disabled_or_model_denied: '账号停用或模型禁用', oauth_required: '需要 OAuth', missing_account_identity: '缺少账号身份', credential_unavailable: '凭据读取失败', credential_invalid: '凭据无效', cookie_required: '官方上游需要 Cookie', cookie_invalid: 'Cookie 无法发送', model_detached: '实际模型脱离，票已作废', proxy_pool_unavailable: '代理池读取失败', proxy_pool_empty: '无可用出口' }
  if (value === 'reused_state')
    return '相同 state（未续期）'
  if (value === 'not_newer')
    return '签发时间未更新'
  return labels[value] ?? value
}

async function load() {
  const version = ++loadVersion
  loading.value = true
  try {
    const data = await getTurnStateStatus(accountFilter.value || undefined, { silent: true })
    if (version === loadVersion) {
      buckets.value = data
      if (!selection.value)
        selectedKey.value = buckets.value[0] ? bucketKey(buckets.value[0]) : ''
      error.value = ''
    }
  }
  catch {
    if (version === loadVersion)
      error.value = '路由状态加载失败'
  }
  finally {
    if (version === loadVersion)
      loading.value = false
  }
}

function managesInjection(bucket: TurnStateStatus) {
  if (bucket.config.cookieLockEnabled && !bucket.config.enabled)
    return false
  return bucket.config.enabled || bucket.manualOverride || bucket.issuedAt !== null || bucket.config.missingStatePolicy === 'pause'
}
function hasAccountOverride(bucket: TurnStateStatus) {
  return !!accounts.value.find(account => account.id === bucket.accountId)?.turnStateOverride
}
function injectionLabel(bucket: TurnStateStatus) {
  if (businessStatus(bucket) !== 'ready')
    return '业务暂停，不发送'
  if (managesInjection(bucket))
    return bucket.active && !expired(bucket) ? `模型桶：${bucket.manualOverride ? '手动应用' : '自动安装'}` : '模型桶：不携带 state'
  return hasAccountOverride(bucket) ? '账号通用自定义 state' : '不强制覆盖客户端 / 会话 state'
}
function canCopy(bucket: TurnStateStatus, issuedAt: number | null | undefined, observation?: TurnStateObservation) {
  return issuedAt != null && issuedAt > 0 && issuedAt <= now.value && issuedAt + bucket.config.ttlSeconds > now.value
    && (observation ? !!observation.observationId && observation.hasToken === true : (bucket.hasInstalledState && issuedAt === bucket.issuedAt) || issuedAt === bucket.candidateIssuedAt)
}
async function removeState(bucket: TurnStateStatus) {
  if (removing.value || !bucket.hasInstalledState || bucket.issuedAt == null)
    return
  removing.value = true
  try {
    await removeTurnState({ accountId: bucket.accountId, model: bucket.model, issuedAt: bucket.issuedAt })
    toast.success('已移除 state，账号和自动探测开关保持不变')
  }
  catch {
    // 签发时间可能已变化，刷新后再操作。
  }
  finally {
    await load()
    removing.value = false
  }
}
async function copyState(bucket: TurnStateStatus, issuedAt: number | null | undefined, observation?: TurnStateObservation) {
  if (copying.value || !canCopy(bucket, issuedAt, observation) || issuedAt == null)
    return
  copying.value = true
  try {
    const token = await copyTurnState({ accountId: bucket.accountId, model: bucket.model, issuedAt, observationId: observation?.observationId })
    await copyText(token.value, { successText: 'state 已复制' })
  }
  catch {
    // 不把正文缓存到页面状态，失败后重新核对候选或当前票。
    await load()
  }
  finally {
    copying.value = false
  }
}

function configure(bucket: TurnStateStatus | null = null) {
  editing.value = bucket
  showConfig.value = true
}

async function probeOnce(bucket: TurnStateStatus) {
  const key = bucketKey(bucket)
  if (probing.value.has(key) || bucket.manualProbeRequestedAt != null)
    return
  probing.value.add(key)
  try {
    await probeTurnState({ accountId: bucket.accountId, model: bucket.model })
    selectedKey.value = key
    tab.value = 'probe'
    toast.success('单次探测已排队')
    await load()
  }
  catch {
    // 请求层统一展示错误，保留当前状态以便重试。
  }
  finally {
    probing.value.delete(key)
  }
}

function canApply(observation: TurnStateObservation) {
  return !!selection.value && canApplyState(selection.value, observation.issuedAt, observation.tokenLength, observation)
}
// 历史行必须按自身正文和长度判断，不能借用同秒候选的状态。
function canApplyState(bucket: TurnStateStatus, issuedAt: number | null | undefined, tokenLength?: number | null, observation?: TurnStateObservation) {
  if (!bucket.accountEnabled || issuedAt == null || issuedAt <= 0 || issuedAt > now.value || issuedAt + bucket.config.ttlSeconds <= now.value)
    return false
  if (bucket.issuedAt !== null && issuedAt <= bucket.issuedAt)
    return false
  if (observation)
    return !!observation.observationId && observation.hasToken === true && tokenLength === bucket.config.targetLength
  return issuedAt === bucket.candidateIssuedAt && bucket.candidateLength === bucket.config.targetLength
}
async function applyState(bucket: TurnStateStatus, issuedAt: number | null | undefined, tokenLength?: number | null, observation?: TurnStateObservation) {
  if (!canApplyState(bucket, issuedAt, tokenLength, observation) || applying.value || issuedAt == null)
    return
  applying.value = true
  try {
    await applyTurnState({ accountId: bucket.accountId, model: bucket.model, issuedAt, observationId: observation?.observationId })
    toast.success('state 已应用，自动探测设置未变更')
    await load()
  }
  catch {
    // 候选可能被并发探测替换或已经过期，刷新后重新判断可用性。
    await load()
  }
  finally {
    applying.value = false
  }
}

async function copyCookie(bucket: TurnStateStatus, pod: string) {
  if (copying.value)
    return
  copying.value = true
  try {
    const cookie = await copyTurnStateCookie({ accountId: bucket.accountId, model: bucket.model, pod })
    await copyText(`${cookie.name}=${cookie.value}`, { successText: 'Cookie 已复制' })
  }
  catch {
    // 值不落页面状态，过期或被替换后重新核对池状态。
    await load()
  }
  finally {
    copying.value = false
  }
}

async function applyCookie(bucket: TurnStateStatus, pod: string) {
  if (cookieApplying.value || !bucket.accountEnabled)
    return
  cookieApplying.value = true
  try {
    await applyTurnStateCookie({ accountId: bucket.accountId, model: bucket.model, pod })
    toast.success('已固定 Cookie')
    await load()
  }
  catch {
    await load()
  }
  finally {
    cookieApplying.value = false
  }
}

async function removeCookie(bucket: TurnStateStatus) {
  if (cookieRemoving.value || !bucket.cookieOverridePod)
    return
  cookieRemoving.value = true
  try {
    await removeTurnStateCookie({ accountId: bucket.accountId, model: bucket.model })
    toast.success('已取消 Cookie 固定')
    await load()
  }
  catch {
    await load()
  }
  finally {
    cookieRemoving.value = false
  }
}

watch(accountFilter, load)
watch([selectedKey, tab], () => {
  page.value = 1
})
useIntervalFn(() => {
  now.value = Date.now() / 1000
}, 1000)
useIntervalFn(() => {
  if (autoRefresh.value && !loading.value && !showConfig.value && !document.hidden)
    void load()
}, 15000)
onMounted(async () => {
  void load()
  try {
    let currentPage = 1
    let lastPage = 1
    do {
      const result = await fetchAccounts({ page: currentPage, pageSize: 200, provider: 'openai' })
      accounts.value.push(...result.items)
      lastPage = result.page.totalPages
      currentPage++
    } while (currentPage <= lastPage)
  }
  catch { error.value = '账号目录加载失败' }
})
</script>

<template>
  <div class="grid content-start gap-5">
    <BasePageHeader title="路由状态">
      <template #actions>
        <BaseIconButton label="刷新路由状态" :disabled="loading" @click="load">
          <RefreshCw class="size-4" :class="{ 'animate-spin': loading }" />
        </BaseIconButton>
        <BaseButton variant="primary" @click="configure()">
          <template #icon>
            <Plus class="size-4" />
          </template>添加轮换
        </BaseButton>
      </template>
    </BasePageHeader>
    <div class="flex flex-wrap items-center justify-between gap-3">
      <BaseSelect v-model="accountFilter" class="w-full sm:w-72" :options="accountOptions" aria-label="筛选账号" />
      <BaseSwitch v-model="autoRefresh" label="自动刷新" show-label />
    </div>
    <p v-if="error" role="alert" class="m-0 text-cp-error-text">
      {{ error }}
    </p>
    <BaseTable :columns="columns" :rows="tableBuckets" :row-key="bucketKey" :loading="loading && !tableBuckets.length" empty-text="暂无路由状态">
      <template #accountName="{ row }">
        <div class="grid gap-1">
          <span class="break-all" :title="row.accountName">{{ row.accountEmail?.trim() || row.accountName || row.accountId }}</span>
          <span class="break-all font-mono text-cp-xs text-cp-text-secondary">{{ row.accountId }}</span>
          <span class="break-all font-mono text-cp-xs text-cp-text-secondary">{{ row.model }}</span>
        </div>
      </template>
      <template #enabled="{ row }">
        <div class="grid gap-1">
          <span :class="row.accountEnabled ? 'text-cp-text' : 'text-cp-warning-text'">账号：{{ row.accountEnabled ? '手动启用' : '手动停用' }}</span>
          <span class="text-cp-xs text-cp-text-secondary">自动探测：{{ row.config.cookieLockEnabled ? 'Cookie 锁定' : row.config.enabled ? 'state 替换' : '已关闭' }}</span>
        </div>
      </template>
      <template #businessStatus="{ row }">
        <div class="grid gap-1">
          <span :class="businessStatus(row) === 'ready' ? 'text-cp-success-text' : 'text-cp-warning-text'">{{ businessLabel(row) }}</span>
          <span class="text-cp-xs text-cp-text-secondary">无票：{{ row.config.missingStatePolicy === 'pause' ? '暂停业务调度' : '继续调度' }}</span>
          <span v-if="row.config.detectActualModel && !row.config.cookieLockEnabled" class="text-cp-xs text-cp-text-secondary">实际模型检测{{ row.config.missingStatePolicy === 'pause' ? '：脱离即作废' : '：仅记录' }}</span>
          <span v-if="row.config.missingStatePolicy === 'pause' && !row.config.enabled && !row.config.cookieLockEnabled" class="text-cp-xs text-cp-warning-text">缺票需手动探测并应用</span>
        </div>
      </template>
      <template #recentProbe="{ row }">
        <div v-if="recentProbe(row)" class="grid gap-1">
          <span>{{ outcome(recentProbe(row)!.outcome) }}</span>
          <span v-if="recentProbe(row)!.answer" class="break-all text-cp-xs" :title="recentProbe(row)!.answer ?? undefined">{{ clippedAnswer(recentProbe(row)!.answer!) }}</span>
          <span class="text-cp-xs text-cp-text-secondary">{{ date(recentProbe(row)!.observedAt) }}</span>
        </div>
        <span v-else class="text-cp-text-secondary">暂无探测</span>
      </template>
      <template #state="{ row }">
        <div v-if="row.config.cookieLockEnabled" class="grid gap-1">
          <span :class="row.active && !expired(row) ? 'text-cp-success-text' : 'text-cp-text-secondary'">{{ row.routingCookie && !expired(row) ? 'Cookie 使用中' : '等待可用 Cookie' }}</span>
          <span class="break-all text-cp-xs">{{ row.routingCookie?.pod ?? '-' }}</span>
          <span class="text-cp-xs text-cp-text-secondary">到期：{{ date(row.routingCookie?.expiresAt ?? null) }}</span>
        </div>
        <div v-else class="grid gap-1" :title="`签发时间：${date(row.issuedAt)}\n到期时间：${date(expiresAt(row))}`">
          <span :class="row.active && !expired(row) ? 'text-cp-success-text' : 'text-cp-text-secondary'">{{ expired(row) ? `${row.tokenLength} · 已过期` : row.active ? `${row.tokenLength} · 使用中` : row.tokenLength ? `${row.tokenLength} · 未使用` : '尚未获取' }}</span>
          <span class="text-cp-xs text-cp-text-secondary">{{ expired(row) ? '改写已解除' : row.active ? age(row.ageSeconds) : row.config.enabled ? `已尝试 ${row.huntAttempts} 次` : '仅被动采集' }}</span>
          <span v-if="row.candidateIssuedAt != null" class="text-cp-xs" :class="canApplyState(row, row.candidateIssuedAt, row.candidateLength) ? 'text-cp-success-text' : 'text-cp-text-secondary'">{{ row.candidateLength }} · {{ canApplyState(row, row.candidateIssuedAt, row.candidateLength) ? '候选可应用' : '候选（不可应用）' }}</span>
          <span v-if="row.manualOverride && row.active && !expired(row)" class="text-cp-xs text-cp-success-text">手动应用</span>
          <div v-if="canCopy(row, row.issuedAt) || canCopy(row, row.candidateIssuedAt)" class="flex flex-wrap gap-1">
            <BaseIconButton v-if="canCopy(row, row.issuedAt)" label="复制已安装 state" :disabled="copying" @click="copyState(row, row.issuedAt)">
              <Copy class="size-4" />
            </BaseIconButton>
            <BaseIconButton v-if="row.hasInstalledState" label="移除已安装 state" :disabled="removing" @click="removeState(row)">
              <Trash2 :size="14" />
            </BaseIconButton>
            <BaseIconButton v-if="canCopy(row, row.candidateIssuedAt)" label="复制候选 state" :disabled="copying" @click="copyState(row, row.candidateIssuedAt)">
              <Copy class="size-4" />
            </BaseIconButton>
            <BaseIconButton v-if="canApplyState(row, row.candidateIssuedAt, row.candidateLength)" label="应用候选 state" :disabled="applying" @click="applyState(row, row.candidateIssuedAt, row.candidateLength)">
              <Check class="size-4" />
            </BaseIconButton>
          </div>
        </div>
      </template>
      <template #actions="{ row }">
        <BaseIconButton v-if="canCopy(row, row.issuedAt)" label="复制已安装 state" :disabled="copying" @click="copyState(row, row.issuedAt)">
          <Copy class="size-4" />
        </BaseIconButton>
        <BaseIconButton :label="row.manualProbeRequestedAt != null ? '探测已排队' : '探测一次'" :disabled="!row.accountEnabled || probing.has(bucketKey(row)) || row.manualProbeRequestedAt != null" @click="probeOnce(row)">
          <Play class="size-4" />
        </BaseIconButton>
        <BaseIconButton label="查看探测记录" @click="selectedKey = bucketKey(row)">
          <Eye class="size-4" />
        </BaseIconButton>
        <BaseIconButton label="轮换配置" @click="configure(row)">
          <Settings2 class="size-4" />
        </BaseIconButton>
      </template>
    </BaseTable>
    <section v-if="selection" class="grid min-w-0 gap-4 pt-3">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <h2 class="m-0 text-cp-xl font-semibold text-cp-text">
          探测与安装记录
        </h2>
        <BaseSelect v-model="selectedKey" :options="bucketOptions" class="w-full sm:w-80" aria-label="查看账号与模型" />
      </div>
      <dl v-if="!selection.config.cookieLockEnabled" class="m-0 flex flex-wrap gap-x-8 gap-y-3 text-cp-sm">
        <div class="min-w-0 basis-full">
          <dt class="text-cp-text-secondary">
            x-codex-turn-state 生效来源
          </dt>
          <dd class="m-0 mt-1 break-words">
            {{ injectionLabel(selection) }}
            <span v-if="hasAccountOverride(selection) && managesInjection(selection)" class="ml-2 text-cp-warning-text">该模型由桶管理，账号通用自定义 state 不生效</span>
          </dd>
        </div>
        <div>
          <dt class="text-cp-text-secondary">
            最近获取时间
          </dt><dd class="m-0 mt-1 font-mono">
            {{ date(selection.installations[0]?.acquiredAt ?? null) }}
          </dd>
        </div>
        <div>
          <dt class="text-cp-text-secondary">
            本次重试 / 近期平均重试
          </dt><dd class="m-0 mt-1">
            {{ selection.installations[0]?.attempts ? Math.max(0, selection.installations[0].attempts - 1) : '-' }} / {{ averageRetries(selection) }}
          </dd>
        </div>
        <div>
          <dt class="text-cp-text-secondary">
            目标长度
          </dt><dd class="m-0 mt-1 font-mono">
            {{ selection.config.targetLength }}
          </dd>
        </div>
        <div>
          <dt class="text-cp-text-secondary">
            近期成功样本
          </dt>
          <dd class="m-0 mt-1">
            {{ selection.installations.filter(item => item.source === 'probe').length }} 次
          </dd>
        </div>
        <div>
          <dt class="text-cp-text-secondary">
            轮换龄 / 有效期
          </dt><dd class="m-0 mt-1">
            {{ selection.config.refreshAfterSeconds }} / {{ selection.config.ttlSeconds }} 秒
          </dd>
        </div>
        <div>
          <dt class="text-cp-text-secondary">
            签发时间
          </dt>
          <dd class="m-0 mt-1 font-mono">
            {{ date(selection.issuedAt) }}
          </dd>
        </div>
        <div>
          <dt class="text-cp-text-secondary">
            到期时间
          </dt>
          <dd class="m-0 mt-1 font-mono">
            {{ date(expiresAt(selection)) }}
          </dd>
        </div>
        <div v-for="[length, count] in distribution" :key="length">
          <dt class="text-cp-text-secondary">
            {{ length }}
          </dt><dd class="m-0 mt-1 font-mono">
            {{ count }} 次
          </dd>
        </div>
      </dl>
      <div v-if="selection.config.cookieLockEnabled" class="grid gap-2 text-cp-sm">
        <h3 class="m-0 font-semibold">共享 Cookie 池</h3>
        <p class="m-0 text-cp-text-secondary">允许编号：{{ selection.config.cookieGatewayIds || '未设置（仅观察）' }}</p>
        <p v-if="selection.cookieOverridePod" class="m-0 text-cp-success-text">当前固定：{{ selection.cookieOverridePod }}<span v-if="selection.cookieOverrideIssuedAt" class="text-cp-xs">（签发 {{ date(selection.cookieOverrideIssuedAt) }}）</span></p>
        <p v-if="!selection.cookiePool?.length" class="m-0 text-cp-text-secondary">尚未采集到路由 Cookie，将按已配置出口继续探测。</p>
        <div v-for="cookie in selection.cookiePool" :key="cookie.pod" class="flex flex-wrap items-center gap-x-4 gap-y-1">
          <span class="font-mono">unified-{{ cookie.gatewayId }}</span>
          <span class="break-all text-cp-xs text-cp-text-secondary">{{ cookie.pod }}</span>
          <span :class="cookie.reportedModel.toLowerCase() === selection.model.toLowerCase() ? 'text-cp-success-text' : 'text-cp-warning-text'">{{ cookie.reportedModel || '未确认模型' }}</span>
          <span :class="cookie.allowed ? 'text-cp-success-text' : 'text-cp-text-secondary'">{{ cookie.allowed ? '编号已允许' : '未允许' }}</span>
          <span class="text-cp-text-secondary">到期 {{ date(cookie.expiresAt) }}</span>
          <BaseIconButton label="复制此 Cookie" :disabled="copying" @click="copyCookie(selection, cookie.pod)">
            <Cookie class="size-4" />
          </BaseIconButton>
          <BaseIconButton v-if="cookie.reportedModel.toLowerCase() === selection.model.toLowerCase() && selection.cookieOverridePod !== cookie.pod" label="固定此 Cookie" :disabled="cookieApplying" @click="applyCookie(selection, cookie.pod)">
            <LockKeyhole class="size-4" />
          </BaseIconButton>
          <BaseIconButton v-if="selection.cookieOverridePod === cookie.pod" label="取消 Cookie 固定" :disabled="cookieRemoving" @click="removeCookie(selection)">
            <Unlock class="size-4" />
          </BaseIconButton>
        </div>
      </div>
      <BaseSegmented v-model="tab" label="记录类型" class="w-full sm:w-96" :options="[{ label: '主动探测', value: 'probe' }, { label: '被动采集', value: 'passive' }, { label: '安装历史', value: 'history' }]" />
      <BaseTable v-if="tab === 'history'" :columns="historyColumns" :rows="installations.slice((page - 1) * pageSize, page * pageSize)" empty-text="暂无安装记录" density="compact" />
      <BaseTable v-else :columns="logColumns" :rows="observations.slice((page - 1) * pageSize, page * pageSize)" empty-text="暂无观测记录" density="compact">
        <template #answer="{ row }">
          <div v-if="row.answer" class="grid gap-1" :title="row.answer ?? undefined">
            <span class="break-all text-cp-xs">{{ clippedAnswer(row.answer) }}</span>
            <span v-if="row.answerMatch === true" class="text-cp-xs text-cp-success-text">命中期望</span>
            <span v-else-if="row.answerMatch === false" class="text-cp-xs text-cp-warning-text">未命中期望</span>
          </div>
          <span v-else class="text-cp-text-secondary">-</span>
        </template>
        <template #actions="{ row }">
          <BaseIconButton v-if="canApply(row)" label="应用此 state（不改变自动探测开关）" :disabled="applying" @click="applyState(selection, row.issuedAt, row.tokenLength, row)">
            <Check class="size-4" />
          </BaseIconButton>
          <span v-else-if="row.isInstalled && selection.hasInstalledState && !expired(selection)" class="text-cp-xs text-cp-text-secondary">已安装</span>
          <span v-else class="text-cp-text-secondary">-</span>
          <BaseIconButton v-if="canCopy(selection, row.issuedAt, row)" label="复制此 state" :disabled="copying" @click="copyState(selection, row.issuedAt, row)">
            <Copy class="size-4" />
          </BaseIconButton>
          <BaseIconButton v-if="row.oailbHost" label="复制此 Cookie" :disabled="copying" @click="copyCookie(selection, row.oailbHost)">
            <Cookie class="size-4" />
          </BaseIconButton>
          <BaseIconButton v-if="row.oailbHost && selection.cookieOverridePod !== row.oailbHost" label="固定此 Cookie" :disabled="cookieApplying" @click="applyCookie(selection, row.oailbHost)">
            <LockKeyhole class="size-4" />
          </BaseIconButton>
        </template>
      </BaseTable>
      <BaseTablePagination :pagination="{ currentPage: page, pageSize, total }" :loading="false" @page-change="page = $event" @page-size-change="pageSize = $event; page = 1" />
    </section>
    <TurnStateConfigModal v-model="showConfig" :bucket="editing" :accounts="accounts" @saved="load" />
  </div>
</template>
