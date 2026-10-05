<script setup lang="ts">
import type { ClientProfileSelection, ProviderRequestProfile, ProviderRequestProfiles, XaiClientProfileSelection } from '@/api/modules/client-profiles'
import { BaseSegmented } from '@codex-proxy/ui'
import { computed, shallowRef } from 'vue'
import { formatProviderLabel, PROVIDER_IDS, providerIcon } from '@/utils/providers'
import ClientProfileEditor from './ClientProfileEditor.vue'
import XaiClientProfileEditor from './XaiClientProfileEditor.vue'

withDefaults(defineProps<{
  active?: boolean
  disabled?: boolean
  allowInherit?: boolean
}>(), {
  active: true,
  disabled: false,
  allowInherit: false,
})

const model = defineModel<ProviderRequestProfiles>({ required: true })
const provider = shallowRef<(typeof PROVIDER_IDS)[number]>('openai')
const providerOptions = PROVIDER_IDS.map(value => ({
  value,
  label: formatProviderLabel(value),
  icon: providerIcon(value),
}))
const providerSelectorStyle = { width: `${providerOptions.length * 40}px` }
const openai = profileModel<ClientProfileSelection>('openai')
const xai = profileModel<XaiClientProfileSelection>('xai')

function profileModel<T extends object>(providerId: string) {
  return computed<T | null>({
    get: () => (model.value[providerId] as T | undefined) ?? null,
    set: value => updateProfile(providerId, value as ProviderRequestProfile | null),
  })
}

function updateProfile(providerId: string, value: ProviderRequestProfile | null) {
  const profiles = { ...model.value }
  if (value === null)
    delete profiles[providerId]
  else
    profiles[providerId] = value
  model.value = profiles
}
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <div v-if="!allowInherit" class="flex flex-wrap items-start justify-between gap-3">
      <slot name="heading" />
      <BaseSegmented
        v-model="provider"
        class="ml-auto max-w-full shrink-0"
        label="客户端身份 Provider"
        :options="providerOptions"
        :disabled="disabled"
        :style="providerSelectorStyle"
        display="icon"
      />
    </div>
    <ClientProfileEditor
      v-if="provider === 'openai'"
      v-model="openai"
      :allow-inherit="allowInherit"
      :active="active"
      :disabled="disabled"
    >
      <template #source-extra>
        <BaseSegmented
          v-model="provider"
          class="max-w-full shrink-0"
          label="上游身份平台"
          :options="providerOptions"
          :disabled="disabled"
          :style="providerSelectorStyle"
          display="icon"
        />
      </template>
    </ClientProfileEditor>
    <XaiClientProfileEditor
      v-else
      v-model="xai"
      :allow-inherit="allowInherit"
      :active="active"
      :disabled="disabled"
    >
      <template #source-extra>
        <BaseSegmented
          v-model="provider"
          class="max-w-full shrink-0"
          label="上游身份平台"
          :options="providerOptions"
          :disabled="disabled"
          :style="providerSelectorStyle"
          display="icon"
        />
      </template>
    </XaiClientProfileEditor>
  </div>
</template>
