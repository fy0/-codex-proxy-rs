<script setup lang="ts">
import { BaseCard, BaseForm, BaseFormItem, BaseInput } from '@codex-proxy/ui'
import { Timer, Zap } from '@lucide/vue'

const refreshMarginSeconds = defineModel<string>('refreshMarginSeconds', { required: true })
const refreshConcurrency = defineModel<string>('refreshConcurrency', { required: true })
</script>

<template>
  <BaseCard title="凭据刷新">
    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseFormItem label="提前刷新时间（秒）" description="访问令牌到期前提前触发自动刷新的时间">
        <BaseInput v-model="refreshMarginSeconds" aria-label="提前刷新时间（秒）" type="number">
          <template #prefix>
            <Timer class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>
      <BaseFormItem label="刷新并发上限" description="同时执行凭据刷新的最大请求数，降低上限可减少限流风险">
        <BaseInput v-model="refreshConcurrency" aria-label="刷新并发上限" type="number">
          <template #prefix>
            <Zap class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>
