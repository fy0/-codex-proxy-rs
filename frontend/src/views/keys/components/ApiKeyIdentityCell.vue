<script setup lang="ts">
import type { getApiKeys } from '@/api'

type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number]

defineProps<{
  apiKey: ApiKeyRow
}>()
</script>

<template>
  <div class="grid min-w-0 gap-1">
    <span class="truncate text-cp text-cp-text" :title="apiKey.name">
      {{ apiKey.name }}
    </span>
    <span v-if="apiKey.label" class="truncate text-cp-xs font-emphasis text-cp-text-quaternary" :title="apiKey.label">
      {{ apiKey.label }}
    </span>
  </div>
</template>
