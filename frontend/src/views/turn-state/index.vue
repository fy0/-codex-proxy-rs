<script setup lang="ts">
import type { getAccounts, TurnStateInstallation, TurnStateObservation, TurnStateStatus } from '@/api'
import { Eye, Plus, RefreshCw, Settings2 } from '@lucide/vue'
import { useIntervalFn } from '@vueuse/core'
import { computed, onMounted, ref, watch } from 'vue'
import { defaultTurnStateConfig, getAccounts as fetchAccounts, getTurnStateStatus } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { useAsyncAction } from '@/composables/useAsyncAction'
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
const { loading, run } = useAsyncAction()
const tableBuckets = computed<TurnStateStatus[]>(() => {
  const configured = new Set(buckets.value.map(bucket => bucket.accountId))
  const unconfigured = accounts.value.filter(account => account.authenticationKind === 'oauth' && !configured.has(account.id) && (!accountFilter.value || account.id === accountFilter.value)).map(account => ({
    accountId: account.id,
    accountName: account.name,
    model: 'gpt-6-astra',
    config: defaultTurnStateConfig(),
    tokenLength: null,
    issuedAt: null,
    ageSeconds: null,
    active: false,
    accountEnabled: account.enabled,
    huntAttempts: 0,
    nextProbeAt: null,
    observations: [],
    installations: [],
    configured: false,
  }))
  return [...buckets.value, ...unconfigured]
})
const selection = computed(() => tableBuckets.value.find(bucket => bucketKey(bucket) === selectedKey.value))
const accountOptions = computed(() => [{ label: '全部账号', value: '' }, ...accounts.value.map(account => ({ label: account.name, value: account.id }))])
const bucketOptions = computed(() => tableBuckets.value.map(bucket => ({ label: `${bucket.accountName} / ${bucket.model}`, value: bucketKey(bucket) })))
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
  { key: 'accountName', label: '账号 / 模型', kind: 'identity', size: 'xl' },
  { key: 'enabled', label: '探测改写', kind: 'status', size: 'md' },
  { key: 'state', label: '当前 state', kind: 'custom', size: 'lg' },
  { key: 'acquiredAt', label: '获取时间', kind: 'datetime', format: (_, row) => date(row.installations[0]?.acquiredAt ?? null) },
  { key: 'retries', label: '本次重试', kind: 'numeric', size: 'sm', format: (_, row) => row.installations[0] ? Math.max(0, row.installations[0].attempts - 1) : '-' },
  { key: 'average', label: '近期平均重试', kind: 'numeric', size: 'md', format: (_, row) => averageRetries(row) },
  { key: 'nextProbeAt', label: '预计开始时间', kind: 'datetime', format: value => value !== null && Number(value) <= Date.now() / 1000 ? '即将开始' : date(value as number | null) },
  { key: 'actions', label: '操作', kind: 'actions' },
])
const logColumns = defineTableColumns<TurnStateObservation>([
  { key: 'observedAt', label: '时间', kind: 'datetime', format: value => date(value as number) },
  { key: 'outcome', label: '结果', kind: 'text', size: 'lg', format: value => outcome(String(value)) },
  { key: 'httpStatus', label: 'HTTP', kind: 'numeric', size: 'sm' },
  { key: 'tokenLength', label: '实际长度', kind: 'numeric', size: 'sm' },
  { key: 'egress', label: '出口', kind: 'text' },
  { key: 'shape', label: 'Shape', kind: 'text', size: 'sm' },
  { key: 'effort', label: 'Effort', kind: 'text', size: 'sm' },
  { key: 'elapsedMs', label: '耗时（ms）', kind: 'numeric' },
  { key: 'stopMode', label: '中断策略', kind: 'text', size: 'lg', format: value => value === 'headers' ? '获得 state' : value === 'first_output' ? '获得回应' : '-' },
  { key: 'stopReason', label: '中断原因', kind: 'text', size: 'lg' },
  { key: 'probeId', label: '请求 ID', kind: 'mono', size: '3xl' },
])
const historyColumns = defineTableColumns<TurnStateInstallation>([
  { key: 'installedAt', label: '安装时间', kind: 'datetime', format: value => date(value as number) },
  { key: 'issuedAt', label: '签发时间', kind: 'datetime', format: value => date(value as number) },
  { key: 'tokenLength', label: '长度', kind: 'numeric' },
  { key: 'acquiredAt', label: '获取时间', kind: 'datetime', format: value => date(value as number) },
  { key: 'attempts', label: '尝试次数', kind: 'numeric' },
  { key: 'huntSeconds', label: '获取耗时（秒）', kind: 'numeric' },
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
function averageRetries(bucket: TurnStateStatus) {
  const samples = bucket.installations.filter(item => item.source === 'probe')
  return samples.length ? (samples.reduce((sum, item) => sum + Math.max(0, item.attempts - 1), 0) / samples.length).toFixed(1) : '-'
}
function outcome(value: string) {
  const labels: Record<string, string> = { candidate: '有效候选', length_miss: '长度未命中', missing_header: '无响应头', transport_error: '传输错误', invalid_token: '无效令牌', expired_or_future: '签发时间失效', http_error: 'HTTP 错误', access_token_expired_or_unknown: '凭据过期或时间未知', account_disabled_or_model_denied: '账号停用或模型禁用', oauth_required: '需要 OAuth', missing_account_identity: '缺少账号身份', credential_unavailable: '凭据读取失败', credential_invalid: '凭据无效', proxy_pool_unavailable: '代理池读取失败', proxy_pool_empty: '无可用出口' }
  return labels[value] ?? value
}

async function load() {
  await run(async () => {
    try {
      buckets.value = await getTurnStateStatus(accountFilter.value || undefined, { silent: true })
      if (!selection.value)
        selectedKey.value = buckets.value[0] ? bucketKey(buckets.value[0]) : ''
      error.value = ''
    }
    catch {
      error.value = '路由状态加载失败'
    }
  })
}

function configure(bucket: TurnStateStatus | null = null) {
  editing.value = bucket
  showConfig.value = true
}

watch(accountFilter, load)
watch([selectedKey, tab], () => {
  page.value = 1
})
useIntervalFn(() => {
  if (autoRefresh.value && !showConfig.value && !document.hidden)
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
    <BaseTable :columns="columns" :rows="tableBuckets" :row-key="bucketKey" :loading="loading" empty-text="暂无路由状态">
      <template #accountName="{ row }">
        <div class="grid gap-1">
          <span>{{ row.accountName }}</span>
          <span class="font-mono text-cp-xs text-cp-text-secondary">{{ row.model }}</span>
        </div>
      </template>
      <template #enabled="{ row }">
        <span :class="row.config.enabled ? 'text-cp-success-text' : 'text-cp-text-secondary'">{{ row.config.enabled ? '已启用' : '未启用' }}</span>
        <span v-if="!row.accountEnabled" class="block text-cp-xs text-cp-warning-text">账号已停用</span>
      </template>
      <template #state="{ row }">
        <div class="grid gap-1" :title="`签发时间：${date(row.issuedAt)}`">
          <span :class="row.active ? 'text-cp-success-text' : 'text-cp-text-secondary'">{{ row.active ? `${row.tokenLength} · 使用中` : row.tokenLength ? `${row.tokenLength} · 未使用` : '尚未获取' }}</span>
          <span class="text-cp-xs text-cp-text-secondary">{{ row.active ? age(row.ageSeconds) : row.config.enabled ? `已尝试 ${row.huntAttempts} 次` : '仅被动采集' }}</span>
        </div>
      </template>
      <template #actions="{ row }">
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
      <dl class="m-0 flex flex-wrap gap-x-8 gap-y-3 text-cp-sm">
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
            轮换龄 / 寿命
          </dt><dd class="m-0 mt-1">
            {{ selection.config.refreshAfterSeconds / 60 }} / {{ selection.config.ttlSeconds / 60 }} 分
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
      <BaseSegmented v-model="tab" label="记录类型" class="w-full sm:w-96" :options="[{ label: '主动探测', value: 'probe' }, { label: '被动采集', value: 'passive' }, { label: '安装历史', value: 'history' }]" />
      <BaseTable v-if="tab === 'history'" :columns="historyColumns" :rows="installations.slice((page - 1) * pageSize, page * pageSize)" empty-text="暂无安装记录" density="compact" />
      <BaseTable v-else :columns="logColumns" :rows="observations.slice((page - 1) * pageSize, page * pageSize)" empty-text="暂无观测记录" density="compact" />
      <BaseTablePagination :pagination="{ currentPage: page, pageSize, total }" :loading="loading" @page-change="page = $event" @page-size-change="pageSize = $event; page = 1" />
    </section>
    <TurnStateConfigModal v-model="showConfig" :bucket="editing" :accounts="accounts" @saved="load" />
  </div>
</template>
