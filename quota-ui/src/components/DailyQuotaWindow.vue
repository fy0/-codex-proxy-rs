<script setup lang="ts">
import type { DailyUsage } from '../api/modules/quota'
import { computed, ref, useId, watch } from 'vue'
import { formatUsd, isValidUsd } from '../presenter'

const props = defineProps<{
  accountName: string
  usage?: DailyUsage
  savedLimit?: string | null
  disabled?: boolean
}>()
const limit = defineModel<string | null>({ required: true })
const inputId = useId()
const previousLimit = ref(limit.value ?? '')
const invalid = computed(() => limit.value !== null && !isValidUsd(limit.value))
const pending = computed(() => limit.value !== (props.savedLimit ?? null))
const known = computed(() => props.usage?.usedUsd != null && formatUsd(props.usage.usedUsd) !== '未知')
const complete = computed(() => known.value && ['ready', 'reached'].includes(props.usage?.status ?? ''))
const state = computed(() => {
  if (!props.usage || props.usage.status === 'disabled') {
    if (pending.value && limit.value !== null)
      return { label: '保存后生效', tone: 'neutral' }
    if (props.savedLimit != null)
      return { label: '暂无观测', tone: 'warning' }
    return { label: '未开启', tone: 'neutral' }
  }
  return {
    ready: { label: '记录正常', tone: 'success' },
    reached: { label: '达到限额', tone: 'warning' },
    unknown: { label: '费用待确认', tone: 'warning' },
    stale: { label: '记录已过期', tone: 'warning' },
  }[props.usage.status]
})
const width = computed(() => {
  if (!complete.value || props.usage?.limitUsd == null)
    return 0
  const savedLimit = Number(props.usage.limitUsd)
  return savedLimit === 0
    ? 100
    : Math.min(100, Math.max(0, Number(props.usage.usedUsd) / savedLimit * 100))
})
const description = computed(() => {
  if (props.usage?.status === 'unknown')
    return '费用数据不完整，暂停分配'
  if (props.usage?.status === 'stale')
    return '费用记录已过期，暂停分配'
  if (pending.value)
    return '保存后按新日限额保护'
  if (props.usage?.status === 'reached')
    return '已达日限额，等待 UTC 换日'
  if (props.usage?.remainingUsd != null && props.savedLimit != null)
    return `剩余 ${formatUsd(props.usage.remainingUsd)}`
  return '覆盖该账号的全部 CPR 请求'
})
const zeroLimit = computed(() => limit.value !== null && isValidUsd(limit.value) && /^0+(?:\.0+)?$/.test(limit.value))
const resetTime = computed(() => {
  const value = props.usage?.resetAtMs
  if (value == null || !Number.isFinite(new Date(value).getTime()))
    return 'UTC 00:00 换日'
  return `重置 ${new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
    timeZone: 'UTC',
  }).format(value)} UTC`
})

watch(limit, (value) => {
  if (value !== null)
    previousLimit.value = value
})

function toggle() {
  limit.value = limit.value === null
    ? previousLimit.value
    : null
}

function updateLimit(event: Event) {
  limit.value = (event.target as HTMLInputElement).value
}
</script>

<template>
  <section class="qg-window qg-daily-window" :class="{ 'qg-window-warning': state.tone === 'warning', 'qg-window-unknown': !complete }" :aria-label="`${accountName} 每日费用`">
    <div class="qg-window-heading">
      <span>每日费用 <span class="qg-currency">USD</span></span>
      <span class="qg-badge" :class="`qg-badge-${state.tone}`">{{ state.label }}</span>
    </div>
    <div class="qg-daily-usage">
      <strong :title="known ? formatUsd(usage?.usedUsd) : undefined">{{ known ? formatUsd(usage?.usedUsd) : '—' }}</strong>
      <span>{{ known ? (complete ? '今日已记录' : '已知费用，待核对') : (savedLimit != null ? '暂无完整记录' : '开启后统计') }}</span>
    </div>
    <div
      class="qg-progress"
      role="progressbar"
      :aria-label="`${accountName} 每日费用使用率`"
      aria-valuemin="0"
      aria-valuemax="100"
      :aria-valuenow="complete ? width : undefined"
      :aria-valuetext="complete ? `${formatUsd(usage?.usedUsd)} / ${formatUsd(usage?.limitUsd)}，按已保存日限额` : state.label"
    >
      <div class="qg-progress-fill" :style="{ width: `${width}%` }" />
    </div>
    <div class="qg-window-controls qg-daily-controls">
      <div class="qg-switch-label">
        <button type="button" class="qg-switch" role="switch" :aria-checked="limit !== null" :aria-label="`${accountName} 每日费用保护`" :disabled="disabled" @click="toggle">
          <span />
        </button>
        <span>{{ limit !== null ? '日限额' : '未开启保护' }}</span>
      </div>
      <div v-if="limit !== null" class="qg-number-field qg-money-field" :class="{ 'qg-field-invalid': invalid }">
        <input
          :id="inputId"
          type="text"
          inputmode="decimal"
          :value="limit"
          :aria-label="`${accountName} 每日费用上限 USD`"
          :aria-invalid="invalid"
          :aria-describedby="`${inputId}-help`"
          placeholder="填写金额"
          autocomplete="off"
          spellcheck="false"
          :disabled="disabled"
          @input="updateLimit"
        >
        <span>USD</span>
      </div>
    </div>
    <p :id="`${inputId}-help`" class="qg-daily-description" :class="{ 'qg-daily-error': invalid }">
      {{ invalid ? '填写非负金额：最多 18 位整数、12 位小数' : zeroLimit ? '0 将立即暂停该账号分配' : description }}
    </p>
    <p class="qg-window-reset qg-daily-reset">
      <span>{{ resetTime }}</span>
      <span v-if="usage?.inFlightCount">{{ usage.inFlightCount }} 笔待结算</span>
      <span v-if="usage?.unknownCostCount">{{ usage.unknownCostCount }} 笔费用未知</span>
    </p>
  </section>
</template>
