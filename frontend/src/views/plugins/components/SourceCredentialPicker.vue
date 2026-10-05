<script setup lang="ts">
import type { PluginSourceCredential } from '@/api'

import { BaseCheckbox, BaseIconButton } from '@codex-proxy/ui'
import { Trash2 } from '@lucide/vue'

withDefaults(defineProps<{
  credentials: PluginSourceCredential[]
  disabled?: boolean
}>(), {
  disabled: false,
})

defineEmits<{ delete: [credential: PluginSourceCredential] }>()
const selected = defineModel<string[]>({ required: true })
function isSelected(id: string) {
  return selected.value.includes(id)
}

function toggle(id: string, checked: boolean) {
  selected.value = checked
    ? [...selected.value, id]
    : selected.value.filter(value => value !== id)
}
</script>

<template>
  <div class="grid gap-2">
    <div
      v-for="credential in credentials"
      :key="credential.id"
      class="flex min-w-0 items-center gap-3 rounded-cp bg-cp-fill-alter px-3 py-1.5"
      :class="disabled ? 'opacity-60' : undefined"
    >
      <BaseCheckbox
        :model-value="isSelected(credential.id)"
        :disabled="disabled"
        :label="`选择下载认证 ${credential.name}`"
        @update:model-value="toggle(credential.id, $event)"
      />
      <span class="flex min-w-0 flex-1 items-center gap-3">
        <strong class="max-w-[45%] shrink-0 truncate text-cp-sm text-cp-text" :title="credential.name">{{ credential.name }}</strong>
        <span class="min-w-0 flex-1 truncate font-mono text-cp-xs text-cp-text-secondary" :title="`${credential.origin}${credential.pathPrefix}`">
          {{ credential.origin }}{{ credential.pathPrefix }}
        </span>
      </span>
      <BaseIconButton size="sm" :label="`删除下载认证 ${credential.name}`" :disabled="disabled" @click="$emit('delete', credential)">
        <Trash2 class="size-3.5 text-cp-error" />
      </BaseIconButton>
    </div>
  </div>
</template>
