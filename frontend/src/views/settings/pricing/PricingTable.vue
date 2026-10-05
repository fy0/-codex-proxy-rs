<script setup lang="ts">
import type { BaseTableColumn } from '@codex-proxy/ui'
import type { PricingRow } from './model'
import { BaseCheckbox, BaseIconButton, BaseTable } from '@codex-proxy/ui'
import { Pencil, Trash2 } from '@lucide/vue'
import { effectivePrice, multiplierText, sourceLabels } from './model'
import PricingUnit from './PricingUnit.vue'

defineProps<{ rows: PricingRow[], selected: string[], loading: boolean, disabled: boolean }>()
defineEmits<{ toggle: [model: string, checked: boolean], togglePage: [checked: boolean], edit: [row: PricingRow], delete: [row: PricingRow] }>()
const columns: BaseTableColumn<PricingRow>[] = [
  { key: 'select', kind: 'selection' },
  { key: 'model', label: '模型', kind: 'identity' },
  { key: 'input', label: '输入', kind: 'numeric' },
  { key: 'output', label: '输出', kind: 'numeric' },
  { key: 'cacheRead', label: '缓存读取', kind: 'numeric' },
  { key: 'multiplier', label: '倍率', kind: 'numeric', align: 'center' },
  { key: 'actions', label: '操作', kind: 'actions' },
]

function summaryBand(row: PricingRow) {
  return row.effective.bands.image ? 'image' : 'standard'
}
</script>

<template>
  <BaseTable :columns="columns" :rows="rows" row-key="model" :loading="loading" :selected-row-keys="selected" empty-text="没有匹配的模型" show-header-when-empty>
    <template v-if="$slots.empty" #empty>
      <slot name="empty" />
    </template>
    <template #header-select>
      <BaseCheckbox label="选择本页模型" :disabled="disabled || !rows.length" :model-value="!!rows.length && rows.every(row => selected.includes(row.model))" :indeterminate="rows.some(row => selected.includes(row.model)) && !rows.every(row => selected.includes(row.model))" @update:model-value="$emit('togglePage', $event)" />
    </template>
    <template #header-model>
      <div class="flex items-center gap-3">
        <span>模型</span>
        <PricingUnit />
      </div>
    </template>
    <template #select="{ row }">
      <BaseCheckbox :label="`选择 ${row.model}`" :disabled="disabled" :model-value="selected.includes(row.model)" @update:model-value="$emit('toggle', row.model, $event)" />
    </template>
    <template #model="{ row }">
      <div class="min-w-0 py-1.5">
        <div class="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1">
          <span class="truncate font-mono font-semibold" :title="row.model">{{ row.model }}</span>
          <span class="shrink-0 text-cp-xs" :class="row.source === 'custom' ? 'text-cp-primary-text' : 'text-cp-text-tertiary'">
            {{ sourceLabels[row.source] }}<span v-if="row.effective.bands.image"> · 图像</span>
          </span>
        </div>
        <div class="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-cp-xs text-cp-text-secondary sm:hidden">
          <span>输入 <span class="font-mono">{{ effectivePrice(row.effective.bands[summaryBand(row)]?.input, row.effective.multiplierBps) }}</span></span>
          <span>输出 <span class="font-mono">{{ effectivePrice(row.effective.bands[summaryBand(row)]?.output, row.effective.multiplierBps) }}</span></span>
        </div>
      </div>
    </template>
    <template v-for="field in (['input', 'output', 'cacheRead'] as const)" :key="field" #[field]="{ row }">
      <div class="py-1 font-mono tabular-nums">
        <div v-if="effectivePrice(row.base.bands[summaryBand(row)]?.[field]) !== effectivePrice(row.effective.bands[summaryBand(row)]?.[field], row.effective.multiplierBps)" class="text-cp-xs text-cp-text-tertiary line-through">
          {{ effectivePrice(row.base.bands[summaryBand(row)]?.[field]) }}
        </div>
        <div :class="row.source === 'custom' ? 'text-cp-primary-text' : 'text-cp-text'">
          {{ effectivePrice(row.effective.bands[summaryBand(row)]?.[field], row.effective.multiplierBps) }}
        </div>
      </div>
    </template>
    <template #multiplier="{ row }">
      <span class="font-mono tabular-nums">{{ multiplierText(row.effective.multiplierBps) }}</span>
    </template>
    <template #actions="{ row }">
      <div class="flex items-center gap-1">
        <BaseIconButton size="sm" label="编辑价格" :disabled="disabled" @click="$emit('edit', row)">
          <Pencil class="size-3.5 text-cp-link" aria-hidden="true" />
        </BaseIconButton>
        <BaseIconButton v-if="row.canDelete" size="sm" label="删除价目" :disabled="disabled" @click="$emit('delete', row)">
          <Trash2 class="size-3.5 text-cp-error" aria-hidden="true" />
        </BaseIconButton>
      </div>
    </template>
  </BaseTable>
</template>
