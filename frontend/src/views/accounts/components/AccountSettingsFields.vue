<script setup lang="ts">
import type { AccountGroup, AccountModelAccess } from '@/api'
import { Copy } from '@lucide/vue'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { useCopyText } from '@/composables/useCopyText'
import AccountModelAccessField from './AccountModelAccessField.vue'
import AccountProxyField from './AccountProxyField.vue'

withDefaults(defineProps<{
  groups: AccountGroup[]
  groupsLoading: boolean
  disabled: boolean
  endpoint?: string | null
  accountId?: string
  preserveProxy?: boolean
  preserveModelAccess?: boolean
  proxyError?: string
  showTurnState?: boolean
}>(), { preserveProxy: true })

const modelAccess = defineModel<AccountModelAccess | undefined>('modelAccess', { required: true })
const enabled = defineModel<boolean>('enabled', { required: true })
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
const turnStateOverride = defineModel<string>('turnStateOverride', { default: '' })
const copyText = useCopyText()
</script>

<template>
  <div class="grid gap-5">
    <AccountModelAccessField v-model="modelAccess" :account-id="accountId" :disabled="disabled" :allow-preserve="preserveModelAccess" />
    <div class="flex min-h-6 items-center justify-between gap-3">
      <span class="text-cp leading-none font-medium text-cp-text-secondary">调度</span>
      <BaseSwitch
        v-model="enabled"
        label="切换账号调度"
        :disabled="disabled"
      />
    </div>

    <div class="grid gap-4 sm:grid-cols-2">
      <BaseFormItem label="并发限制">
        <BaseInput
          v-model="concurrencyLimit"
          aria-label="账号并发限制"
          type="number"
          min="1"
          max="4294967295"
          placeholder="留空使用默认值"
          :disabled="disabled"
        />
      </BaseFormItem>
      <BaseFormItem label="权重">
        <BaseInput
          v-model="weight"
          aria-label="账号调度权重"
          type="number"
          min="1"
          max="100"
          placeholder="越高越优先，最大 100"
          :disabled="disabled"
        />
      </BaseFormItem>
    </div>

    <BaseFormItem label="所属分组">
      <AccountGroupCheckboxGrid
        v-model="selectedGroupIds"
        :groups="groups"
        :loading="groupsLoading"
        :disabled="disabled"
      />
    </BaseFormItem>
    <AccountProxyField v-model:mode="proxyMode" v-model:proxy-id="proxyId" :preserve="preserveProxy" :error="proxyError" :endpoint="endpoint" :account-id="accountId" :disabled="disabled" />
    <BaseFormItem v-if="showTurnState" label="账号通用 x-codex-turn-state">
      <div class="flex min-w-0 items-center gap-2">
        <BaseInput
          v-model="turnStateOverride"
          class="min-w-0 flex-1"
          aria-label="自定义 x-codex-turn-state"
          placeholder="非空时覆盖自动 state，留空则使用模型桶"
          :disabled="disabled"
        />
        <BaseIconButton label="复制账号通用 state" :disabled="!turnStateOverride" @click="copyText(turnStateOverride, { successText: 'state 已复制' })">
          <Copy :size="15" />
        </BaseIconButton>
      </div>
      <p class="mb-0 mt-2 text-cp-xs text-cp-text-secondary">
        非空时业务请求使用这里的值，覆盖桶内自动安装的 state，直到改为空。清空后恢复桶内票；不能解除暂停业务调度的缺票限制。
      </p>
    </BaseFormItem>
  </div>
</template>
