<script setup lang="ts">
import type { AccountInfo, AccountOverview, AccountRule } from '../api/modules/quota'
import { CircleAlert, Info, Trash2 } from '@lucide/vue'
import { computed } from 'vue'
import { accountName, formatDate, highestUsageWindow, isProtected } from '../presenter'
import DailyQuotaWindow from './DailyQuotaWindow.vue'
import QuotaWindow from './QuotaWindow.vue'

const props = defineProps<{
  info: AccountInfo
  state?: AccountOverview
  evaluatedAtMs?: number
  savedRule?: AccountRule
  awaitingRefresh?: boolean
  disabled?: boolean
}>()
const emit = defineEmits<{ remove: [] }>()
const rule = defineModel<AccountRule>({ required: true })
const name = computed(() => accountName(props.info))
const changed = computed(() => JSON.stringify(rule.value) !== JSON.stringify(props.savedRule))
const status = computed(() => {
  if (props.awaitingRefresh)
    return { label: '已保存，待刷新', tone: 'info' }
  if (!props.savedRule)
    return { label: '待保存', tone: 'info' }
  if (!isProtected(props.savedRule))
    return { label: '保护已关闭', tone: 'neutral' }
  if (!props.state)
    return { label: '等待观测', tone: 'info' }
  if (props.state.assessment.excluded)
    return { label: '已拦截', tone: 'warning' }
  return { label: '未拦截', tone: 'success' }
})
const reasons = computed(() => [
  ...new Set(props.state?.assessment.reasons.map(reason => reason.message) ?? []),
])

function updateLimit(field: 'fiveHourLimitPercent' | 'weeklyLimitPercent', value: number | null) {
  rule.value = { ...rule.value, [field]: value }
}

function updateDailyLimit(value: string | null) {
  rule.value = { ...rule.value, dailyLimitUsd: value }
}
</script>

<template>
  <article class="qg-account-card" :class="{ 'qg-account-blocked': state?.assessment.excluded }" data-testid="account-card">
    <header class="qg-account-heading">
      <span class="qg-avatar" aria-hidden="true">{{ name.slice(0, 1).toLocaleUpperCase() }}</span>
      <div class="qg-account-identity">
        <div class="qg-account-title">
          <h3>{{ name }}</h3>
          <span class="qg-badge" :class="`qg-badge-${status.tone}`"><span class="qg-state-dot" />{{ status.label }}</span>
          <span v-if="changed && savedRule && !awaitingRefresh" class="qg-badge qg-badge-info">未保存</span>
          <span v-if="info.enabled === false" class="qg-badge qg-badge-neutral">宿主已停用</span>
        </div>
        <p>{{ info.email && info.email !== name ? info.email : rule.accountId }}<span v-if="info.email && info.email !== name" class="qg-account-id">{{ rule.accountId }}</span></p>
      </div>
      <span class="qg-account-observed">观测 {{ formatDate(state?.quota?.observedAtMs) }}</span>
      <button type="button" class="qg-icon-button qg-remove" :disabled="disabled" :aria-label="`移除 ${name} 的保护配置`" title="移除保护配置" @click="emit('remove')">
        <Trash2 aria-hidden="true" />
      </button>
    </header>
    <div class="qg-account-windows">
      <QuotaWindow
        :model-value="rule.fiveHourLimitPercent"
        label="5 小时"
        :account-name="name"
        :window="highestUsageWindow(state?.quota?.windows, 18000, evaluatedAtMs)"
        :evaluated-at-ms="evaluatedAtMs"
        :disabled="disabled"
        @update:model-value="updateLimit('fiveHourLimitPercent', $event)"
      />
      <QuotaWindow
        :model-value="rule.weeklyLimitPercent"
        label="每周"
        :account-name="name"
        :window="highestUsageWindow(state?.quota?.windows, 604800, evaluatedAtMs)"
        :evaluated-at-ms="evaluatedAtMs"
        :disabled="disabled"
        @update:model-value="updateLimit('weeklyLimitPercent', $event)"
      />
      <DailyQuotaWindow
        :model-value="rule.dailyLimitUsd"
        :account-name="name"
        :usage="state?.dailyUsage"
        :saved-limit="savedRule?.dailyLimitUsd"
        :disabled="disabled"
        @update:model-value="updateDailyLimit"
      />
    </div>
    <div v-if="reasons.length && !awaitingRefresh" class="qg-account-reasons" :class="{ 'qg-account-reasons-info': !state?.assessment.excluded }">
      <CircleAlert v-if="state?.assessment.excluded" aria-hidden="true" />
      <Info v-else aria-hidden="true" />
      <span>{{ reasons.join(' · ') }}</span>
    </div>
  </article>
</template>
