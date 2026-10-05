<script setup lang="ts">
import { BaseFormItem, BaseTextarea } from '@codex-proxy/ui'

import { computed } from 'vue'

withDefaults(defineProps<{
  label: string
  description?: string
  placeholder?: string
  disabled?: boolean
}>(), {
  description: undefined,
  placeholder: '每行一项',
  disabled: false,
})

const values = defineModel<string[]>({ required: true })
const text = computed({
  get: () => values.value.join('\n'),
  set: (value) => {
    values.value = [...new Set(value.split(/[\n,]/).map(item => item.trim()).filter(Boolean))]
  },
})
</script>

<template>
  <BaseFormItem :label="label" :description="description">
    <BaseTextarea
      v-model="text"
      :rows="2"
      :disabled="disabled"
      :placeholder="placeholder"
      :aria-label="label"
      class="font-mono"
    />
  </BaseFormItem>
</template>
