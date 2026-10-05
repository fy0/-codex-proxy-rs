<script setup lang="ts">
import { BaseFormItem, BaseSelect } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useProxyCatalog } from '@/composables/useProxyCatalog'

defineProps<{ disabled?: boolean }>()

const proxyId = defineModel<string>({ required: true })
const { proxies, loading } = useProxyCatalog()
const options = computed(() => [
  { label: '不使用代理（直连）', value: '' },
  ...proxies.value.map(proxy => ({
    label: proxy.name,
    value: proxy.id,
    description: proxy.endpoint,
  })),
  // 目录加载失败或代理已被删除时保留已选 ID，不能静默改为直连。
  ...(proxyId.value && !proxies.value.some(proxy => proxy.id === proxyId.value)
    ? [{ label: '已选代理（目录暂不可用）', value: proxyId.value, description: proxyId.value, disabled: true }]
    : []),
])
</script>

<template>
  <BaseFormItem label="下载代理">
    <BaseSelect
      v-model="proxyId"
      :options="options"
      :disabled="disabled || loading"
      class="w-full"
      aria-label="下载代理"
    />
  </BaseFormItem>
</template>
