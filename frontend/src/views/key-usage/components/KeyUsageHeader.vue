<script setup lang="ts">
import { BaseButton, BaseIconButton, BasePageHeader, BaseSelect } from '@codex-proxy/ui'
import { Info, KeyRound, LogOut, Moon, RefreshCw, Sun, Terminal } from '@lucide/vue'
import { shallowRef } from 'vue'
import { useRouter } from 'vue-router'
import { useAuthStore } from '@/stores/modules/auth'
import { useThemeStore } from '@/stores/modules/theme'

defineProps<{ name?: string, prefix?: string, refreshing: boolean, configuring: boolean }>()
defineEmits<{ refresh: [], configure: [], openAbout: [] }>()
const period = defineModel<string>('period', { required: true })
const refreshInterval = defineModel<string>('refreshInterval', { required: true })
const auth = useAuthStore()
const theme = useThemeStore()
const router = useRouter()
const loggingOut = shallowRef(false)

async function logout() {
  loggingOut.value = true
  try {
    if (await auth.logout())
      await router.replace({ name: 'login', state: { loginMode: 'key' } })
  }
  finally {
    loggingOut.value = false
  }
}
</script>

<template>
  <BasePageHeader title="使用统计">
    <template #description>
      <span class="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1.5">
        <span class="min-w-0 truncate text-cp font-semibold text-cp-text" :title="name">
          {{ name || '当前密钥' }}
        </span>
        <span v-if="prefix" class="inline-flex shrink-0 items-center gap-1.5 rounded-cp-sm bg-cp-fill-tertiary px-2 py-0.5 text-cp-text-secondary" aria-label="密钥前缀">
          <KeyRound class="size-3" aria-hidden="true" />
          <code class="font-mono text-cp-xs leading-4 font-normal">{{ prefix }}…</code>
        </span>
      </span>
    </template>
    <template #actions>
      <div class="key-usage-actions flex max-w-[calc(100vw-32px)] flex-wrap items-center justify-end gap-2">
        <BaseIconButton label="关于" variant="secondary" @click="$emit('openAbout')">
          <Info class="size-4" />
        </BaseIconButton>
        <BaseButton variant="secondary" :loading="configuring" @click="$emit('configure')">
          <Terminal class="size-4" />
          密钥配置
        </BaseButton>
        <BaseSelect v-model="period" aria-label="统计时间范围" class="w-29" :options="[{ label: '今天', value: 'today' }, { label: '近 7 天', value: '7d' }, { label: '近 30 天', value: '30d' }]" />
        <BaseSelect v-model="refreshInterval" aria-label="自动刷新频率" class="w-30" :options="[{ label: '30 秒刷新', value: '30' }, { label: '60 秒刷新', value: '60' }, { label: '暂停刷新', value: '0' }]" />
        <BaseIconButton label="刷新用量" variant="secondary" :loading="refreshing" @click="$emit('refresh')">
          <template #loading>
            <RefreshCw class="size-4 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-4" />
        </BaseIconButton>
        <BaseIconButton label="切换主题" variant="secondary" @click="theme.toggleTheme($event)">
          <Sun v-if="theme.effectiveTheme === 'dark'" class="size-4" />
          <Moon v-else class="size-4" />
        </BaseIconButton>
        <BaseIconButton label="退出登录" variant="secondary" :disabled="loggingOut" @click="logout">
          <LogOut class="size-4" />
        </BaseIconButton>
      </div>
    </template>
  </BasePageHeader>
</template>

<style scoped>
.key-usage-actions {
  --cp-icon-button-secondary-bg: var(--cp-button-secondary-bg);
  --cp-icon-button-secondary-hover-bg: var(--cp-button-secondary-hover-bg);
  --cp-icon-button-secondary-active-bg: var(--cp-button-secondary-active-bg);
  --cp-input-bg: var(--cp-button-secondary-bg);
  --cp-input-hover-bg: var(--cp-button-secondary-hover-bg);
  --cp-input-active-bg: var(--cp-button-secondary-active-bg);
}
</style>
