<script setup lang="ts">
import type { AccountInfo } from './api/modules/quota'
import { ZButton, ZCard, ZEmpty, ZInput, ZInputNumber, ZPagination, ZTag } from '@codex-proxy/ui'
import { Check, CircleAlert, Clock3, LoaderCircle, Plus, RefreshCw, Search, ShieldCheck, ShieldOff, Users, X } from '@lucide/vue'
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
const locked = computed(() => saving.value || saved.value)
const states = computed(() => new Map(overview.value?.accounts.map(account => [account.accountId, account]) ?? []))
const savedRules = computed(() => new Map(overview.value?.configuration.accounts.map(rule => [rule.accountId, rule]) ?? []))
const accountInfo = computed(() => new Map([
  ...candidates.value,
  ...(overview.value?.accounts ?? []),
].map(account => [account.accountId, account])))
const protectedCount = computed(() => overview.value?.configuration.accounts.filter(isProtected).length ?? 0)
const blockedCount = computed(() => overview.value?.accounts.filter(account => account.assessment.excluded).length ?? 0)
const filteredRules = computed(() => draft.value.accounts.filter(rule => matches(accountInfo.value.get(rule.accountId) ?? {
  accountId: rule.accountId,
  name: null,
  email: null,
  enabled: null,
}, search.value)))
const visibleRules = computed(() => filteredRules.value.slice((page.value - 1) * pageSize.value, page.value * pageSize.value))
const configuredIds = computed(() => new Set(draft.value.accounts.map(rule => rule.accountId)))
const matchingCandidates = computed(() => candidates.value.filter(account => matches(account, candidateSearch.value)))
const visibleCandidates = computed(() => matchingCandidates.value.slice((candidatePage.value - 1) * 10, candidatePage.value * 10))

function matches(account: AccountInfo, query: string) {
  const value = query.trim().toLocaleLowerCase()
  return !value || [account.name, account.email, account.accountId].some(text => text?.toLocaleLowerCase().includes(value))
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

watch([search, pageSize], () => {
  page.value = 1
})
watch(candidateSearch, () => {
  candidatePage.value = 1
})
watch(() => filteredRules.value.length, (length) => {
  page.value = Math.max(1, Math.min(page.value, Math.ceil(length / pageSize.value)))
})
</script>

<template>
  <main class="flex min-w-0 flex-col gap-4 text-cp-text">
    <div v-if="error && !conflict" role="alert" class="flex items-start gap-2 rounded-cp bg-cp-error-container px-4 py-3 text-cp-sm text-cp-error-on-container">
      <CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <span>{{ error }}</span>
      <ZButton v-if="!overview" size="small" :loading="loading" class="ml-auto" @click="refresh">
        重试
      </ZButton>
    </div>
    <div v-if="saved" role="status" class="flex items-start gap-2 rounded-cp bg-cp-success-container px-4 py-3 text-cp-sm text-cp-success-on-container">
      <Check class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      配置已保存，请点击页面右上角的“刷新页面”查看最新状态
    </div>
    <div v-if="conflict" role="alert" class="rounded-cp bg-cp-warning-container px-4 py-3 text-cp-sm text-cp-warning-on-container">
      配置已被其他操作修改，草稿尚未保存，请点击宿主页面右上角刷新后重新编辑
    </div>

    <ZCard v-if="loading && !overview" padding="compact">
      <div role="status" class="flex items-center gap-2 py-8 text-cp-sm text-cp-text-secondary">
        <LoaderCircle class="size-4 animate-spin motion-reduce:animate-none" aria-hidden="true" />
        正在读取账号和额度状态
      </div>
    </ZCard>

    <template v-if="overview">
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <ZCard padding="compact">
          <div class="flex items-start justify-between gap-3">
            <div>
              <p class="m-0 text-cp-sm font-emphasis text-cp-text-secondary">
                已开启保护
              </p>
              <strong class="mt-2 block font-mono text-2xl font-heavy tabular-nums">{{ protectedCount }}</strong>
            </div>
            <Users class="size-5 text-cp-primary-text" aria-hidden="true" />
          </div>
        </ZCard>
        <ZCard padding="compact">
          <div class="flex items-start justify-between gap-3">
            <div>
              <p class="m-0 text-cp-sm font-emphasis text-cp-text-secondary">
                已拦截
              </p>
              <strong class="mt-2 block font-mono text-2xl font-heavy text-cp-warning-text tabular-nums">{{ blockedCount }}</strong>
            </div>
            <ShieldOff class="size-5 text-cp-warning-text" aria-hidden="true" />
          </div>
        </ZCard>
        <ZCard padding="compact">
          <div class="flex items-start justify-between gap-3">
            <div>
              <p class="m-0 text-cp-sm font-emphasis text-cp-text-secondary">
                未拦截
              </p>
              <strong class="mt-2 block font-mono text-2xl font-heavy text-cp-success-text tabular-nums">{{ protectedCount - blockedCount }}</strong>
            </div>
            <ShieldCheck class="size-5 text-cp-success-text" aria-hidden="true" />
          </div>
        </ZCard>
      </div>

      <ZCard padding="compact">
        <div class="flex flex-wrap items-center justify-between gap-4">
          <div class="flex min-w-0 flex-wrap items-center gap-x-5 gap-y-3">
            <div>
              <span id="observation-age-label" class="flex items-center gap-2 text-cp-sm font-heavy">
                <Clock3 class="size-4 text-cp-text-secondary" aria-hidden="true" />
                额度观测有效期
              </span>
              <p class="mt-1 mb-0 text-cp-xs text-cp-text-secondary">
                超过有效期，等待新观测后再恢复分配
              </p>
            </div>
            <ZInputNumber
              id="observation-age"
              v-model="draft.maxObservationAgeSeconds"
              aria-label="额度观测有效期"
              aria-labelledby="observation-age-label"
              :min="60"
              :max="86400"
              :step="30"
              :precision="0"
              :disabled="locked"
              unit="秒"
              class="w-40"
            />
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <span v-if="dirty && !saved" class="mr-1 text-cp-xs text-cp-warning-text">有未保存修改</span>
            <ZButton :disabled="!dirty || locked" variant="text" @click="discard">
              撤销修改
            </ZButton>
            <ZButton type="primary" :disabled="!dirty || locked || conflict" :loading="saving" @click="save">
              保存配置
            </ZButton>
          </div>
        </div>
      </ZCard>

      <div class="flex flex-wrap items-center justify-between gap-3">
        <ZInput v-model="search" aria-label="搜索保护配置" placeholder="搜索名称、邮箱或账号 ID" class="min-w-0 flex-1 sm:max-w-80">
          <template #prefix>
            <Search class="size-4 text-cp-text-quaternary" />
          </template>
        </ZInput>
        <div class="flex flex-wrap items-center gap-2">
          <ZButton :disabled="locked" :loading="loading" variant="text" @click="refresh">
            <template #icon>
              <RefreshCw class="size-4" />
            </template>
            刷新状态
          </ZButton>
          <ZButton :disabled="locked || draft.accounts.length >= 1000" @click="openAccounts">
            <template #icon>
              <Plus class="size-4" />
            </template>
            添加账号
          </ZButton>
        </div>
      </div>

      <ZCard v-if="adding" padding="compact">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <h2 class="m-0 text-cp font-heavy">
            选择 OpenAI OAuth 账号
          </h2>
          <ZButton variant="text" size="small" aria-label="关闭账号选择" @click="adding = false">
            <template #icon>
              <X class="size-4" />
            </template>
            关闭
          </ZButton>
        </div>
        <p class="mt-2 mb-0 text-cp-xs text-cp-text-secondary">
          仅支持具有 5 小时和周额度观测的 OAuth 账号
        </p>
        <ZInput v-model="candidateSearch" aria-label="搜索已加载账号" placeholder="搜索已加载的账号" class="mt-3 sm:max-w-80" />
        <p v-if="candidatesError" role="alert" class="my-3 text-cp-sm text-cp-error-text">
          {{ candidatesError }}
        </p>
        <div v-if="!candidatesLoaded && candidatesLoading" role="status" class="py-4 text-cp-sm text-cp-text-secondary">
          正在读取账号
        </div>
        <div class="mt-3 grid gap-2 sm:grid-cols-2">
          <div v-for="account in visibleCandidates" :key="account.accountId" class="flex min-w-0 items-center gap-3 rounded-cp bg-cp-fill-quaternary px-3 py-2.5">
            <div class="min-w-0 flex-1">
              <p class="m-0 truncate text-cp-sm font-emphasis">
                {{ accountName(account) }}
              </p>
              <p class="mt-1 mb-0 truncate font-mono text-cp-xs text-cp-text-quaternary">
                {{ account.email && account.email !== accountName(account) ? account.email : account.accountId }}
              </p>
            </div>
            <ZTag v-if="account.enabled === false" size="small">
              已停用
            </ZTag>
            <ZButton
              size="small"
              :disabled="locked || configuredIds.has(account.accountId) || draft.accounts.length >= 1000"
              :aria-label="`添加 ${accountName(account)} 的保护配置`"
              @click="addAccount(account)"
            >
              {{ configuredIds.has(account.accountId) ? '已添加' : '添加' }}
            </ZButton>
          </div>
        </div>
        <ZEmpty v-if="candidatesLoaded && !matchingCandidates.length" size="small" title="没有匹配的账号" :description="nextCursor ? '可继续加载更多账号' : '请先在宿主添加 OpenAI OAuth 账号'" class="mt-3" />
        <ZPagination v-if="matchingCandidates.length > 10" v-model:current-page="candidatePage" :page-size="10" :total="matchingCandidates.length" :page-sizes="[10]" class="mt-3" />
        <div class="mt-3 flex flex-wrap items-center justify-between gap-3">
          <span class="text-cp-xs text-cp-text-secondary">已加载 {{ candidates.length }} 个账号</span>
          <ZButton v-if="nextCursor || candidatesError" :loading="candidatesLoading" size="small" @click="loadCandidates">
            {{ candidatesError ? '重试' : '加载更多' }}
          </ZButton>
        </div>
      </ZCard>

      <p class="m-0 text-cp-xs leading-relaxed text-cp-text-secondary">
        状态按已保存配置评估 · {{ formatDate(overview.evaluatedAtMs) }} · 达到上限即拦截
      </p>
      <AccountRuleCard
        v-for="rule in visibleRules"
        :key="rule.accountId"
        :model-value="rule"
        :info="infoFor(rule.accountId)"
        :state="states.get(rule.accountId)"
        :saved-rule="savedRules.get(rule.accountId)"
        :awaiting-refresh="saved"
        :disabled="locked"
        @update:model-value="updateRule"
        @remove="removeAccount(rule.accountId)"
      />
      <ZEmpty
        v-if="!filteredRules.length"
        :icon="ShieldCheck"
        :title="draft.accounts.length ? '没有匹配的保护配置' : '还没有配置保护账号'"
        :description="draft.accounts.length ? '尝试其他名称、邮箱或账号 ID' : '添加账号并设置使用率上限，保存后开始保护'"
      >
        <template v-if="!draft.accounts.length" #action>
          <ZButton :disabled="locked" @click="openAccounts">
            添加账号
          </ZButton>
        </template>
      </ZEmpty>
      <ZPagination v-if="filteredRules.length > 20" v-model:current-page="page" v-model:page-size="pageSize" :total="filteredRules.length" :page-sizes="[20, 50, 100]" />
    </template>
  </main>
</template>
