<script setup lang="ts">
import type { CloudMintConfig, TurnStateConfig } from '@/api'
import { Plus, Trash2 } from '@lucide/vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseNumberInput from '@/components/base/BaseNumberInput.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'

defineProps<{ saving: boolean }>()
const config = defineModel<TurnStateConfig>({ required: true })

const MAX_ENDPOINTS = 16

function nextEndpointName() {
  const used = new Set(config.value.cloudMints.map(endpoint => endpoint.name))
  let index = config.value.cloudMints.length + 1
  while (used.has(`fc-${index}`))
    index += 1
  return `fc-${index}`
}

function addEndpoint() {
  if (config.value.cloudMints.length >= MAX_ENDPOINTS)
    return
  const endpoint: CloudMintConfig = {
    name: nextEndpointName(),
    url: '',
    keyEnv: 'CPA_RELAY_KEY',
    transport: 'sse',
    gateway: 'any',
    ticketLength: config.value.targetLength,
    ttlSeconds: 240,
    timeoutMs: 90000,
    proxyUrl: '',
  }
  config.value.cloudMints.push(endpoint)
}

function removeEndpoint(index: number) {
  config.value.cloudMints.splice(index, 1)
}
</script>

<template>
  <fieldset class="m-0 min-w-0 border-0 p-0">
    <legend class="mb-3 text-cp font-semibold text-cp-text">
      云端打票
    </legend>
    <div class="grid gap-4">
      <p class="m-0 text-cp-sm text-cp-text-secondary">
        优先向云端端点请求已签发票与路由 Cookie；全部端点冷却时回落本地探测，下方探测出口选择仍然生效。
        <template v-if="!config.cloudMints.length">
          未配置端点时仅使用本地探测打票。
        </template>
      </p>
      <div class="grid gap-4 sm:grid-cols-2">
        <BaseFormItem label="端点选择策略">
          <BaseSegmented v-model="config.strategy" label="端点选择策略" :options="[{ label: '随机', value: 'random' }, { label: '轮询', value: 'round_robin' }]" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="账号任务时限（秒）">
          <BaseNumberInput v-model="config.taskTimeoutSeconds" label="账号任务时限" :min="10" :max="600" class="[&_input]:w-16" :disabled="saving" />
          <span class="text-cp-xs text-cp-text-secondary">跨模型共享预算，实际时限最多 75 秒。</span>
        </BaseFormItem>
        <BaseFormItem label="连续失败进入冷却">
          <BaseNumberInput v-model="config.cloudFailureThreshold" label="连续失败进入冷却" :min="1" :max="64" class="[&_input]:w-16" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="端点冷却（秒）">
          <BaseNumberInput v-model="config.cloudCooldownSeconds" label="端点冷却" :min="10" :max="86400" :step="10" class="[&_input]:w-16" :disabled="saving" />
        </BaseFormItem>
      </div>
      <div v-for="(endpoint, index) in config.cloudMints" :key="index" class="grid min-w-0 gap-4 rounded-cp bg-cp-fill-quaternary p-4">
        <div class="flex items-center justify-between gap-2">
          <span class="text-cp-sm font-semibold text-cp-text">端点 {{ index + 1 }}</span>
          <BaseIconButton label="删除端点" :disabled="saving" @click="removeEndpoint(index)">
            <Trash2 class="size-4" />
          </BaseIconButton>
        </div>
        <div class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="名称" required>
            <BaseInput v-model="endpoint.name" aria-label="端点名称" placeholder="fc-1" maxlength="64" :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="密钥环境变量名" required>
            <BaseInput v-model="endpoint.keyEnv" aria-label="密钥环境变量名" placeholder="CPA_RELAY_KEY" maxlength="128" :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="云函数地址" class="sm:col-span-2" required>
            <BaseInput v-model="endpoint.url" type="url" aria-label="云函数地址" placeholder="https://relay.example.com/codex/mint" maxlength="2048" :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="传输">
            <BaseSegmented v-model="endpoint.transport" label="传输方式" :options="[{ label: 'SSE', value: 'sse' }, { label: 'WebSocket', value: 'websocket' }]" :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="网关">
            <BaseInput v-model="endpoint.gateway" aria-label="网关" placeholder="any" maxlength="64" :disabled="saving" />
            <span class="text-cp-xs text-cp-text-secondary">any 或 unified-N 节点编号。</span>
          </BaseFormItem>
          <BaseFormItem label="票长度">
            <BaseNumberInput v-model="endpoint.ticketLength" label="票长度" :min="76" :max="4096" class="[&_input]:w-16" :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="票有效期（秒）">
            <BaseNumberInput v-model="endpoint.ttlSeconds" label="票有效期" :min="1" :max="3600" :step="60" class="[&_input]:w-16" :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="超时（毫秒）">
            <BaseNumberInput v-model="endpoint.timeoutMs" label="超时" :min="1000" :max="300000" :step="1000" class="[&_input]:w-20" :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="代理 URL（可选）" class="sm:col-span-2">
            <BaseInput v-model="endpoint.proxyUrl" aria-label="代理 URL" placeholder="留空不使用前置代理" maxlength="512" :disabled="saving" />
            <span class="text-cp-xs text-cp-text-secondary">仅用于访问该云函数地址，不影响业务请求出口。</span>
          </BaseFormItem>
        </div>
      </div>
      <div class="flex items-center gap-3">
        <BaseButton :disabled="saving || config.cloudMints.length >= MAX_ENDPOINTS" @click="addEndpoint">
          <template #icon>
            <Plus class="size-4" />
          </template>
          添加端点
        </BaseButton>
        <span v-if="config.cloudMints.length" class="text-cp-xs text-cp-text-secondary">{{ config.cloudMints.length }}/{{ MAX_ENDPOINTS }}</span>
      </div>
      <p class="m-0 text-cp-xs text-cp-text-secondary">
        密钥环境变量名只填变量名；实际密钥配置在网关进程或容器的环境变量中，不在这里保存。
      </p>
    </div>
  </fieldset>
</template>
