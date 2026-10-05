<script setup lang="ts">
import type { ProviderRequestProfiles } from '@/api/modules/client-profiles'
import { BaseCard } from '@codex-proxy/ui'
import ProviderRequestProfilesEditor from '@/components/client-profile/ProviderRequestProfilesEditor.vue'

withDefaults(defineProps<{ active?: boolean, disabled?: boolean }>(), {
  active: true,
  disabled: false,
})

const model = defineModel<ProviderRequestProfiles>({ required: true })
</script>

<template>
  <BaseCard>
    <ProviderRequestProfilesEditor
      v-model="model"
      :active="active"
      :disabled="disabled"
      class="max-w-6xl"
    >
      <template #heading>
        <div class="min-w-0 pt-0.5">
          <h2 class="m-0 text-xl leading-[1.15] font-heavy text-cp-text text-balance">
            客户端身份
          </h2>
          <p class="mt-1.75 mb-0 text-cp leading-[1.3] font-emphasis text-cp-text-secondary text-pretty">
            配置网关向上游声明的客户端类型、版本与请求头
          </p>
        </div>
      </template>
    </ProviderRequestProfilesEditor>
  </BaseCard>
</template>
