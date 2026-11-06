<script setup lang="ts">
import type { QuotaWindow } from '../api/modules/quota'
import { ZInputNumber, ZSwitch } from '@codex-proxy/ui'
import { computed } from 'vue'
import { formatDate, percent } from '../presenter'

const props = defineProps<{
  label: string
  accountName: string
  window?: QuotaWindow
  disabled?: boolean
}>()
const limit = defineModel<number | null>({ required: true })
const width = computed(() => `${Math.min(100, Math.max(0, props.window?.usedPercent ?? 0))}%`)
</script>

<template>
  <div class="min-w-0 rounded-cp bg-cp-fill-quaternary p-3">
    <div class="mb-2 flex items-center justify-between gap-3">
      <span class="text-cp-sm font-emphasis text-cp-text-secondary">{{ label }}</span>
      <strong class="font-mono text-cp tabular-nums">{{ percent(window?.usedPercent) }}</strong>
    </div>
    <div
      role="progressbar"
      :aria-label="`${accountName} ${label}使用率`"
      aria-valuemin="0"
      aria-valuemax="100"
      :aria-valuenow="window?.usedPercent == null ? undefined : Math.min(100, Math.max(0, window.usedPercent))"
      :aria-valuetext="percent(window?.usedPercent)"
      class="h-1.5 overflow-hidden rounded-full bg-cp-fill-secondary"
    >
      <div class="h-full rounded-full bg-cp-primary" :style="{ width }" />
    </div>
    <div class="mt-3 flex flex-wrap items-center justify-between gap-2">
      <ZSwitch
        :model-value="limit !== null"
        :aria-label="`${accountName} ${label}保护`"
        :disabled="disabled"
        @update:model-value="limit = $event ? 95 : null"
      />
      <ZInputNumber
        v-if="limit !== null"
        :model-value="limit"
        :aria-label="`${accountName} ${label}使用率上限`"
        :min="0"
        :max="100"
        :step="1"
        :disabled="disabled"
        unit="%"
        size="small"
        class="w-28"
        @update:model-value="limit = $event"
      />
      <span v-else class="text-cp-xs text-cp-text-secondary">此窗口未保护</span>
    </div>
    <p class="mt-2 mb-0 text-cp-xs leading-relaxed text-cp-text-quaternary">
      重置 {{ formatDate(window?.resetAtMs) }}
    </p>
  </div>
</template>
