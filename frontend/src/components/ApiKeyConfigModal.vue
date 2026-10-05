<script setup lang="ts">
import { BaseButton, BaseIconButton, BaseModal, BaseScrollbar, BaseSegmented, BaseSwitch } from '@codex-proxy/ui'

import { Apple, Copy, Monitor, Upload } from '@lucide/vue'
import { computed, shallowRef } from 'vue'
import { buildCodexCcSwitchImportDeeplink, buildCodexConfigFiles, CODEX_WEBSOCKET_ENABLED_BY_DEFAULT } from '@/utils/client'

const props = withDefaults(defineProps<{
  title?: string
  apiKey: {
    name?: string
    key?: string
  } | null
  apiBaseUrl: string
}>(), { title: '使用密钥' })

const emit = defineEmits<{
  copy: [text: string]
}>()

const open = defineModel<boolean>({ default: false })

const activePlatform = shallowRef('unix')
const websocketEnabled = shallowRef(CODEX_WEBSOCKET_ENABLED_BY_DEFAULT)

const platformOptions = [
  { label: 'macOS / Linux', value: 'unix', icon: Apple },
  { label: 'Windows', value: 'windows', icon: Monitor },
]

const keyValue = computed(() => props.apiKey?.key ?? '')
const configPath = computed(() =>
  activePlatform.value === 'windows'
    ? '%userprofile%\\.codex\\config.toml'
    : '~/.codex/config.toml',
)
const authPath = computed(() =>
  activePlatform.value === 'windows' ? '%userprofile%\\.codex\\auth.json' : '~/.codex/auth.json',
)
const codexConfigFiles = computed(() => buildCodexConfigFiles({
  apiKey: keyValue.value,
  baseUrl: props.apiBaseUrl,
  websocketEnabled: websocketEnabled.value,
}))

const visibleFiles = computed(() => [
  { path: configPath.value, content: codexConfigFiles.value.configToml, scrollbarHeight: '360px' },
  { path: authPath.value, content: codexConfigFiles.value.authJson, scrollbarHeight: undefined },
])

function importToCcs() {
  if (!keyValue.value)
    return
  window.location.href = buildCodexCcSwitchImportDeeplink({
    apiKey: keyValue.value,
    baseUrl: props.apiBaseUrl,
    providerName: props.apiKey?.name || 'codex-proxy-rs',
    websocketEnabled: websocketEnabled.value,
  })
}
</script>

<template>
  <BaseModal
    v-model="open"
    :title="title"
    description="将下方内容保存或合并到对应文件，保存后重新启动 Codex"
    size="lg"
  >
    <div class="flex flex-col gap-5">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <BaseSegmented v-model="activePlatform" label="配置平台" :options="platformOptions" />
        <BaseSwitch
          v-model="websocketEnabled"
          label="切换 WebSocket 配置"
          active-text="WS"
          inactive-text="WS"
          inline-prompt
          :width="56"
        />
      </div>

      <div class="flex flex-col gap-3">
        <section
          v-for="file in visibleFiles"
          :key="file.path"
          class="overflow-hidden rounded-cp-card bg-cp-fill-quaternary shadow-cp-tertiary"
        >
          <div class="flex items-center justify-between gap-3 px-4 py-2.5">
            <span
              class="min-w-0 truncate font-mono text-cp-sm font-emphasis text-cp-text-secondary"
            >
              {{ file.path }}
            </span>
            <BaseIconButton
              variant="secondary"
              size="sm"
              label="复制"
              @click="emit('copy', file.content)"
            >
              <Copy class="size-3.5" />
            </BaseIconButton>
          </div>
          <BaseScrollbar
            :height="file.scrollbarHeight"
            max-height="360px"
          >
            <div class="mx-3 mb-3 rounded-cp bg-cp-bg-container px-3.5 py-3 shadow-cp-tertiary">
              <pre
                class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-cp-sm leading-[1.65] font-emphasis text-cp-text"
                v-text="file.content"
              />
            </div>
          </BaseScrollbar>
        </section>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="secondary" :disabled="!keyValue" @click="importToCcs">
        <Upload class="size-4" />
        导入 CCSwitch
      </BaseButton>
      <BaseButton variant="primary" @click="open = false">
        关闭
      </BaseButton>
    </template>
  </BaseModal>
</template>
