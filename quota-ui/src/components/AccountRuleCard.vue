<script setup lang="ts">
import type { AccountInfo, AccountOverview, AccountRule } from '../api/modules/quota'
import { ZButton, ZCard, ZTag } from '@codex-proxy/ui'
import { Trash2 } from '@lucide/vue'
import { computed } from 'vue'
import { accountName, formatDate, highestUsageWindow, isProtected } from '../presenter'
import QuotaWindow from './QuotaWindow.vue'

const props = defineProps<{
  info: AccountInfo
  state?: AccountOverview
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
    return { label: '已保存，待刷新', type: 'info' as const }
  if (!props.savedRule)
    return { label: '待保存', type: 'info' as const }
  if (!isProtected(props.savedRule))
    return { label: '保护已关闭', type: 'default' as const }
  if (!props.state)
    return { label: '状态未知', type: 'warning' as const }
  if (props.state.assessment.excluded)
    return { label: '已拦截', type: 'warning' as const }
  return { label: '未拦截', type: 'success' as const }
})
const reasons = computed(() => [
  ...new Set(props.state?.assessment.reasons.map(reason => reason.message) ?? []),
])

function updateLimit(field: 'fiveHourLimitPercent' | 'weeklyLimitPercent', value: number | null) {
  rule.value = { ...rule.value, [field]: value }
}
</script>

<template>
  <ZCard as="article" padding="compact">
    <div class="grid min-w-0 gap-4 lg:grid-cols-[minmax(180px,1.3fr)_minmax(170px,1fr)_minmax(170px,1fr)_auto] lg:items-start">
      <div class="min-w-0">
        <div class="flex flex-wrap items-center gap-2">
          <h3 class="m-0 wrap-anywhere text-cp font-heavy">
            {{ name }}
          </h3>
          <ZTag :type="status.type" size="small">
            {{ status.label }}
          </ZTag>
          <ZTag v-if="changed && savedRule && !awaitingRefresh" type="info" size="small">
            未保存
          </ZTag>
          <ZTag v-if="info.enabled === false" size="small">
            宿主已停用
          </ZTag>
        </div>
        <p v-if="info.email && info.email !== name" class="mt-1 mb-0 wrap-anywhere text-cp-sm text-cp-text-secondary">
          {{ info.email }}
        </p>
        <p class="mt-1 mb-0 wrap-anywhere font-mono text-cp-xs text-cp-text-quaternary">
          {{ rule.accountId }}
        </p>
        <ul v-if="reasons.length && !awaitingRefresh" class="mt-3 mb-0 list-none space-y-1 p-0 text-cp-xs leading-relaxed text-cp-warning-text">
          <li v-for="reason in reasons" :key="reason">
            {{ reason }}
          </li>
        </ul>
        <p class="mt-3 mb-0 text-cp-xs text-cp-text-secondary">
          观测 {{ formatDate(state?.quota?.observedAtMs) }}
        </p>
      </div>
      <QuotaWindow
        :model-value="rule.fiveHourLimitPercent"
        label="5 小时"
        :account-name="name"
        :window="highestUsageWindow(state?.quota?.windows, 18000)"
        :disabled="disabled"
        @update:model-value="updateLimit('fiveHourLimitPercent', $event)"
      />
      <QuotaWindow
        :model-value="rule.weeklyLimitPercent"
        label="每周"
        :account-name="name"
        :window="highestUsageWindow(state?.quota?.windows, 604800)"
        :disabled="disabled"
        @update:model-value="updateLimit('weeklyLimitPercent', $event)"
      />
      <ZButton
        variant="text"
        size="small"
        :disabled="disabled"
        :aria-label="`移除 ${name} 的保护配置`"
        class="justify-self-end"
        @click="emit('remove')"
      >
        <template #icon>
          <Trash2 class="size-3.5" />
        </template>
        移除
      </ZButton>
    </div>
  </ZCard>
</template>
