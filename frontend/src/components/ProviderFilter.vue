<script setup lang="ts">
import { BaseSegmented } from '@codex-proxy/ui'
import { LayoutGrid } from '@lucide/vue'
import { formatProviderLabel, PROVIDER_IDS, providerIcon } from '@/utils/providers'

defineProps<{ disabled?: boolean }>()
const provider = defineModel<string>({ required: true })
const options = [
  { label: '全部平台', value: '', icon: LayoutGrid },
  ...PROVIDER_IDS.map(value => ({ label: formatProviderLabel(value), value, icon: providerIcon(value) })),
]
</script>

<template>
  <div class="flex min-w-0 max-w-full items-center gap-1">
    <div class="min-w-0 overflow-x-auto">
      <BaseSegmented
        v-model="provider"
        label="按平台筛选"
        display="icon"
        :style="{ width: `${options.length * 40 + 4}px` }"
        :options="options"
        :disabled="disabled"
      />
    </div>
  </div>
</template>
