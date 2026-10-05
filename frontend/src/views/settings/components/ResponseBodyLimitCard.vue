<script setup lang="ts">
import { BaseCard, BaseForm, BaseFormItem, BaseInput } from '@codex-proxy/ui'

const model = defineModel<string>({ required: true })
</script>

<template>
  <BaseCard title="请求体限制" description="限制 Responses 请求解压后的请求体大小">
    <BaseForm class="max-w-6xl sm:grid-cols-2 sm:items-end">
      <BaseFormItem label="解压后大小上限" description="调高可接收更大的图片和历史上下文，也会增加内存占用">
        <BaseInput v-model="model" aria-label="解压后请求体大小上限" type="number" min="1" step="1">
          <template #suffix>
            MiB
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>
