<script setup lang="ts">
import type { AccountInfo, AccountRule } from './api/modules/quota'
import { Check, ChevronLeft, ChevronRight, CircleAlert, Clock3, LoaderCircle, Plus, RefreshCw, Save, Search, Settings2, ShieldCheck, ShieldOff, Users, X } from '@lucide/vue'
import { computed, ref, watch } from 'vue'
import AccountRuleCard from './components/AccountRuleCard.vue'
import { useQuotaGuard } from './composables/useQuotaGuard'
import { accountName, formatDate, isProtected } from './presenter'

const {
  overview,
  draft,
  loading,
  saving,
  saved,
  error,
  conflict,
  dirty,
  invalidDailyLimits,
  candidates,
  candidatesLoaded,
  candidatesLoading,
  candidatesError,
  nextCursor,
  refresh,
  loadCandidates,
  addAccount,
  updateRule,
  removeAccount,
  discard,
  save,
} = useQuotaGuard()

const search = ref('')
const candidateSearch = ref('')
const adding = ref(false)
const page = ref(1)
const pageSize = ref(20)
const candidatePage = ref(1)
const statusFilter = ref<'all' | 'available' | 'blocked' | 'off'>('all')
const locked = computed(() => saving.value || saved.value)
const states = computed(() => new Map(overview.value?.accounts.map(account => [account.accountId, account]) ?? []))
const savedRules = computed(() => new Map(overview.value?.configuration.accounts.map(rule => [rule.accountId, rule]) ?? []))
const accountInfo = computed(() => new Map([
  ...candidates.value,
  ...(overview.value?.accounts ?? []),
].map(account => [account.accountId, account])))
const protectedCount = computed(() => overview.value?.configuration.accounts.filter(isProtected).length ?? 0)
const blockedCount = computed(() => overview.value?.accounts.filter(account => account.assessment.excluded).length ?? 0)
const availableCount = computed(() => Math.max(0, protectedCount.value - blockedCount.value))
const filteredRules = computed(() => draft.value.accounts.filter(rule =>
  matches(infoFor(rule.accountId), search.value) && matchesStatus(rule),
))
const pageCount = computed(() => Math.max(1, Math.ceil(filteredRules.value.length / pageSize.value)))
const visibleRules = computed(() => filteredRules.value.slice((page.value - 1) * pageSize.value, page.value * pageSize.value))
const configuredIds = computed(() => new Set(draft.value.accounts.map(rule => rule.accountId)))
const matchingCandidates = computed(() => candidates.value.filter(account => matches(account, candidateSearch.value)))
const candidatePageCount = computed(() => Math.max(1, Math.ceil(matchingCandidates.value.length / 10)))
const visibleCandidates = computed(() => matchingCandidates.value.slice((candidatePage.value - 1) * 10, candidatePage.value * 10))

function matches(account: AccountInfo, query: string) {
  const value = query.trim().toLocaleLowerCase()
  return !value || [account.name, account.email, account.accountId].some(text => text?.toLocaleLowerCase().includes(value))
}

function matchesStatus(rule: AccountRule) {
  const stored = savedRules.value.get(rule.accountId) ?? rule
  const blocked = states.value.get(rule.accountId)?.assessment.excluded
  if (statusFilter.value === 'off')
    return !isProtected(stored)
  if (statusFilter.value === 'blocked')
    return blocked === true
  if (statusFilter.value === 'available')
    return isProtected(stored) && blocked === false
  return true
}

function infoFor(accountId: string): AccountInfo {
  return accountInfo.value.get(accountId) ?? {
    accountId,
    name: null,
    email: null,
    enabled: null,
  }
}

function openAccounts() {
  adding.value = true
  if (!candidatesLoaded.value)
    void loadCandidates()
}

function updateObservationAge(event: Event) {
  const input = event.target as HTMLInputElement
  const value = input.valueAsNumber
  if (Number.isFinite(value))
    draft.value.maxObservationAgeSeconds = Math.min(86400, Math.max(60, Math.round(value)))
  input.value = String(draft.value.maxObservationAgeSeconds)
}

watch([search, pageSize, statusFilter], () => {
  page.value = 1
})
watch(candidateSearch, () => {
  candidatePage.value = 1
})
watch(pageCount, (value) => {
  page.value = Math.min(page.value, value)
})
watch(candidatePageCount, (value) => {
  candidatePage.value = Math.min(candidatePage.value, value)
})
</script>

<template>
  <main class="qg-app" :aria-busy="loading && !overview">
    <div v-if="error && !conflict" class="qg-notice qg-notice-error" role="alert">
      <CircleAlert aria-hidden="true" />
      <span>{{ error }}</span>
      <button v-if="!overview" type="button" class="qg-button qg-button-small" :disabled="loading" @click="refresh">
        重试
      </button>
    </div>
    <div v-if="saved" class="qg-notice qg-notice-success" role="status">
      <Check aria-hidden="true" />
      <span>配置已保存，请点击宿主右上角的“刷新页面”查看最新状态</span>
    </div>
    <div v-if="conflict" class="qg-notice qg-notice-warning" role="alert">
      <CircleAlert aria-hidden="true" />
      <span>配置已被其他操作修改，草稿尚未保存，请点击宿主右上角刷新后重新编辑</span>
    </div>

    <section v-if="loading && !overview" class="qg-empty" role="status">
      <LoaderCircle class="qg-spin" aria-hidden="true" />
      <h2>正在读取额度状态</h2>
      <p>获取账号与已保存的保护规则</p>
    </section>

    <template v-if="overview">
      <section class="qg-overview" aria-label="额度保护概览">
        <div class="qg-stat">
          <div class="qg-stat-icon">
            <Users aria-hidden="true" />
          </div>
          <div><span class="qg-stat-label">已开启保护</span><strong>{{ protectedCount }}<small>个账号</small></strong></div>
        </div>
        <div class="qg-stat">
          <div class="qg-stat-icon qg-icon-success">
            <ShieldCheck aria-hidden="true" />
          </div>
          <div><span class="qg-stat-label">未触发保护</span><strong>{{ availableCount }}<small>可继续分配</small></strong></div>
        </div>
        <div class="qg-stat">
          <div class="qg-stat-icon qg-icon-warning">
            <ShieldOff aria-hidden="true" />
          </div>
          <div><span class="qg-stat-label">已暂停分配</span><strong>{{ blockedCount }}<small>等待额度恢复</small></strong></div>
        </div>
      </section>

      <section class="qg-panel qg-settings-bar" aria-label="保护设置与保存">
        <label class="qg-observation-setting" for="qg-observation-age">
          <Settings2 aria-hidden="true" />
          <span class="qg-setting-copy">
            <span id="qg-observation-age-label" class="qg-setting-label">额度观测有效期</span>
            <span id="qg-observation-age-help" class="qg-setting-description">超过此时间提示更新；仅确认超限才拦截</span>
          </span>
          <span class="qg-number-field qg-age-field">
            <input id="qg-observation-age" type="number" aria-labelledby="qg-observation-age-label" aria-describedby="qg-observation-age-help" :value="draft.maxObservationAgeSeconds" min="60" max="86400" step="30" :disabled="locked" @change="updateObservationAge">
            <span>秒</span>
          </span>
        </label>
        <div class="qg-save-actions">
          <span class="qg-save-state" :class="{ 'qg-save-state-dirty': dirty && !saved }">
            <span class="qg-state-dot" />
            {{ saved ? '已保存，待刷新' : dirty ? '有未保存修改' : '配置已同步' }}
          </span>
          <button type="button" class="qg-button qg-button-quiet" :disabled="!dirty || locked" @click="discard">
            撤销修改
          </button>
          <button type="button" class="qg-button qg-button-primary" :disabled="!dirty || locked || conflict || invalidDailyLimits" @click="save">
            <LoaderCircle v-if="saving" class="qg-spin" aria-hidden="true" />
            <Save v-else aria-hidden="true" />
            保存配置
          </button>
        </div>
      </section>

      <p v-if="invalidDailyLimits" class="qg-validation-note" role="alert">
        请填写有效的每日 USD 限额后保存，或关闭对应账号的日费用保护。
      </p>

      <div class="qg-toolbar">
        <label class="qg-search" for="qg-rule-search">
          <Search aria-hidden="true" />
          <input id="qg-rule-search" v-model="search" type="search" aria-label="搜索保护配置" placeholder="搜索名称、邮箱或账号 ID">
        </label>
        <div class="qg-filter" role="group" aria-label="保护状态筛选">
          <button type="button" :aria-pressed="statusFilter === 'all'" @click="statusFilter = 'all'">
            全部
          </button>
          <button type="button" :aria-pressed="statusFilter === 'available'" @click="statusFilter = 'available'">
            未拦截
          </button>
          <button type="button" :aria-pressed="statusFilter === 'blocked'" @click="statusFilter = 'blocked'">
            已拦截
          </button>
          <button type="button" :aria-pressed="statusFilter === 'off'" @click="statusFilter = 'off'">
            未保护
          </button>
        </div>
        <div class="qg-toolbar-actions">
          <button type="button" class="qg-button" :disabled="locked || loading" @click="refresh">
            <RefreshCw :class="{ 'qg-spin': loading }" aria-hidden="true" />
            刷新状态
          </button>
          <button type="button" class="qg-button qg-button-accent" :disabled="locked || draft.accounts.length >= 1000" @click="openAccounts">
            <Plus aria-hidden="true" />
            添加账号
          </button>
        </div>
      </div>

      <section v-if="adding" class="qg-panel qg-picker" aria-labelledby="qg-picker-title">
        <div class="qg-section-heading">
          <div>
            <h2 id="qg-picker-title">
              选择保护账号
            </h2><p>添加 OpenAI OAuth 账号并设置额度阈值</p>
          </div>
          <button type="button" class="qg-icon-button" aria-label="关闭账号选择" @click="adding = false">
            <X aria-hidden="true" />
          </button>
        </div>
        <label class="qg-search qg-picker-search" for="qg-candidate-search">
          <Search aria-hidden="true" />
          <input id="qg-candidate-search" v-model="candidateSearch" type="search" aria-label="搜索已加载账号" placeholder="搜索已加载的账号">
        </label>
        <p v-if="candidatesError" class="qg-inline-error" role="alert">
          {{ candidatesError }}
        </p>
        <p v-if="!candidatesLoaded && candidatesLoading" class="qg-muted" role="status">
          正在读取账号
        </p>
        <div class="qg-candidates">
          <div v-for="account in visibleCandidates" :key="account.accountId" class="qg-candidate">
            <span class="qg-avatar qg-avatar-small" aria-hidden="true">{{ accountName(account).slice(0, 1).toLocaleUpperCase() }}</span>
            <div class="qg-candidate-name">
              <strong>{{ accountName(account) }}</strong>
              <span>{{ account.email && account.email !== accountName(account) ? account.email : account.accountId }}</span>
            </div>
            <span v-if="account.enabled === false" class="qg-badge qg-badge-neutral">已停用</span>
            <button type="button" class="qg-button qg-button-small" :disabled="locked || configuredIds.has(account.accountId) || draft.accounts.length >= 1000" :aria-label="`添加 ${accountName(account)}`" @click="addAccount(account)">
              <Check v-if="configuredIds.has(account.accountId)" aria-hidden="true" />
              <Plus v-else aria-hidden="true" />
              {{ configuredIds.has(account.accountId) ? '已添加' : '添加' }}
            </button>
          </div>
        </div>
        <p v-if="candidatesLoaded && !matchingCandidates.length" class="qg-picker-empty">
          没有匹配的账号{{ nextCursor ? '，可继续加载更多' : '' }}
        </p>
        <div class="qg-picker-footer">
          <span class="qg-muted">已加载 {{ candidates.length }} 个账号</span>
          <div class="qg-pagination">
            <template v-if="candidatePageCount > 1">
              <button type="button" class="qg-icon-button" aria-label="上一页候选账号" :disabled="candidatePage <= 1" @click="candidatePage--">
                <ChevronLeft aria-hidden="true" />
              </button>
              <span>{{ candidatePage }} / {{ candidatePageCount }}</span>
              <button type="button" class="qg-icon-button" aria-label="下一页候选账号" :disabled="candidatePage >= candidatePageCount" @click="candidatePage++">
                <ChevronRight aria-hidden="true" />
              </button>
            </template>
            <button v-if="nextCursor || candidatesError" type="button" class="qg-button qg-button-small" :disabled="candidatesLoading" @click="loadCandidates">
              {{ candidatesError ? '重试' : '加载更多' }}
            </button>
          </div>
        </div>
      </section>

      <div class="qg-list-heading">
        <h2>账号规则 <span>{{ filteredRules.length }}</span></h2>
        <span class="qg-updated"><Clock3 aria-hidden="true" />{{ formatDate(overview.evaluatedAtMs) }}<span class="qg-auto-refresh"> · 每 30 秒自动刷新</span></span>
      </div>
      <div class="qg-account-list">
        <AccountRuleCard
          v-for="rule in visibleRules"
          :key="rule.accountId"
          :model-value="rule"
          :info="infoFor(rule.accountId)"
          :state="states.get(rule.accountId)"
          :evaluated-at-ms="overview.evaluatedAtMs"
          :saved-rule="savedRules.get(rule.accountId)"
          :awaiting-refresh="saved"
          :disabled="locked"
          @update:model-value="updateRule"
          @remove="removeAccount(rule.accountId)"
        />
      </div>
      <section v-if="!filteredRules.length" class="qg-empty">
        <ShieldCheck aria-hidden="true" />
        <h2>{{ draft.accounts.length ? '没有匹配的保护配置' : '还没有配置保护账号' }}</h2>
        <p>{{ draft.accounts.length ? '尝试其他关键词或切换筛选条件' : '添加账号，设置阈值，保存后开始保护' }}</p>
        <button v-if="!draft.accounts.length" type="button" class="qg-button qg-button-accent" :disabled="locked" @click="openAccounts">
          <Plus aria-hidden="true" />添加第一个账号
        </button>
      </section>

      <details class="qg-daily-help">
        <summary>日费用如何统计与保护？</summary>
        <p>每日费用按账号统计全部 CPR 请求，覆盖所有模型与 Client Key；使用 CPR 记录的实际或估算 USD 费用，不代表官方套餐额度。按请求开始时间归属 UTC 自然日，跨午夜的请求计入开始那天；每天 UTC 00:00 换日，默认关闭。</p>
        <p>只有当天已记录费用达到日限额才暂停新分配。新账号没有使用记录时从 0 开始；未计价记录、读取失败或观测过期只提示，不单独拦截。已知费用始终保留；在途请求与结算延迟可能使最终费用超过阈值，因此日限额不是严格的原子预算。</p>
      </details>

      <footer class="qg-footer">
        <span>按已保存规则评估；确认超限才拦截，未知状态不单独拦截</span>
        <div v-if="filteredRules.length > 20" class="qg-pagination">
          <select v-model.number="pageSize" aria-label="每页账号数">
            <option :value="20">
              20 / 页
            </option><option :value="50">
              50 / 页
            </option><option :value="100">
              100 / 页
            </option>
          </select>
          <button type="button" class="qg-icon-button" aria-label="上一页保护账号" :disabled="page <= 1" @click="page--">
            <ChevronLeft aria-hidden="true" />
          </button>
          <span>{{ page }} / {{ pageCount }}</span>
          <button type="button" class="qg-icon-button" aria-label="下一页保护账号" :disabled="page >= pageCount" @click="page++">
            <ChevronRight aria-hidden="true" />
          </button>
        </div>
      </footer>
    </template>
  </main>
</template>
