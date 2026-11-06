<script setup lang="ts">
import type { QuotaWindow } from '../api/modules/quota'
import { computed, ref, watch } from 'vue'
import { formatDate, percent } from '../presenter'

const props = defineProps<{
  label: string
  accountName: string
  window?: QuotaWindow
  disabled?: boolean
}>()
const limit = defineModel<number | null>({ required: true })
const previousLimit = ref(limit.value ?? 95)
const observed = computed(() => props.window?.usedPercent != null)
const width = computed(() => Math.min(100, Math.max(0, props.window?.usedPercent ?? 0)))
const tone = computed(() => {
  if (!observed.value)
    return 'unknown'
  if (limit.value !== null && width.value >= limit.value)
    return 'warning'
  return 'normal'
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
  const input = event.target as HTMLInputElement
  const value = input.valueAsNumber
  if (Number.isFinite(value))
    limit.value = Math.min(100, Math.max(0, value))
  input.value = String(limit.value ?? previousLimit.value)
}
</script>

<template>
  <section class="qg-window" :class="`qg-window-${tone}`" :aria-label="`${accountName} ${label}额度`">
    <div class="qg-window-heading">
      <span>{{ label }}额度</span>
      <div class="qg-window-usage">
        <strong>{{ observed ? percent(window?.usedPercent) : '未知' }}</strong><span>{{ observed ? '已使用' : '暂无观测' }}</span>
      </div>
    </div>
    <div
      class="qg-progress"
      role="progressbar"
      :aria-label="`${accountName} ${label}使用率`"
      aria-valuemin="0"
      aria-valuemax="100"
      :aria-valuenow="observed ? width : undefined"
      :aria-valuetext="percent(window?.usedPercent)"
    >
      <div class="qg-progress-fill" :style="{ width: `${width}%` }" />
      <span v-if="limit !== null" class="qg-progress-limit" :style="{ left: `${limit}%` }" aria-hidden="true" />
    </div>
    <div class="qg-window-controls">
      <div class="qg-switch-label">
        <button type="button" class="qg-switch" role="switch" :aria-checked="limit !== null" :aria-label="`${accountName} ${label}保护`" :disabled="disabled" @click="toggle">
          <span />
        </button>
        <span>{{ limit !== null ? '保护阈值' : '未开启保护' }}</span>
      </div>
      <div v-if="limit !== null" class="qg-number-field">
        <input type="number" :value="limit" :aria-label="`${accountName} ${label}使用率上限`" min="0" max="100" step="1" :disabled="disabled" @change="updateLimit">
        <span>%</span>
      </div>
    </div>
    <p class="qg-window-reset">
      重置 {{ formatDate(window?.resetAtMs) }}
    </p>
  </section>
</template>
