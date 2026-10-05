<script setup lang="ts">
import type { CreatePluginSourceCredentialRequest, PluginSourceAuthentication, PluginSourceCredential } from '@/api'
import { BaseFormItem, BaseInput, BaseSelect } from '@codex-proxy/ui'
import { computed, reactive, shallowRef, watch } from 'vue'
import { normalizePluginRepository } from '../utils/model'
import PluginHelpPopover from './PluginHelpPopover.vue'
import SourceCredentialPicker from './SourceCredentialPicker.vue'

const props = defineProps<{
  kind: 'url' | 'github'
  location: string
  credentials: PluginSourceCredential[]
  disabled: boolean
}>()
defineEmits<{
  deleteCredential: [credential: PluginSourceCredential]
}>()
const selected = defineModel<string[]>({ required: true })
const pending = defineModel<boolean>('pending', { required: true })
const draft = defineModel<CreatePluginSourceCredentialRequest | null>('draft', { required: true })
const mode = shallowRef(selected.value.length ? 'saved' : 'public')
const form = reactive({ kind: 'bearer', token: '', username: '', password: '', headerName: '', headerValue: '' })
const authOptions = [
  { label: '访问令牌', value: 'bearer' },
  { label: '用户名与密码', value: 'basic' },
  { label: '自定义请求头', value: 'header' },
]
const modeOptions = computed(() => [
  { label: '无需认证', value: 'public' },
  { label: props.kind === 'github' ? '填写 GitHub 令牌' : '填写认证信息', value: 'new' },
  ...(props.credentials.length ? [{ label: '使用已保存凭据', value: 'saved' }] : []),
])
const scope = computed(() => {
  if (props.kind === 'github') {
    const repository = normalizePluginRepository(props.location)
    return /^[-\w.]+\/[-\w.]+$/.test(repository)
      ? { origin: 'https://api.github.com', pathPrefix: `/repos/${repository}`, name: `GitHub · ${repository}` }
      : null
  }
  try {
    const url = new URL(props.location.trim())
    const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)
    if (!(url.protocol === 'https:' || (url.protocol === 'http:' && loopback)) || url.username || url.password || url.hash || /[%\\]/.test(url.pathname))
      return null
    return { origin: url.origin, pathPrefix: url.pathname, name: `下载 · ${url.hostname}` }
  }
  catch {
    return null
  }
})
const authentication = computed<PluginSourceAuthentication | null>(() => {
  if (props.kind === 'github')
    return form.token.trim() ? { kind: 'github', token: form.token.trim() } : null
  if (form.kind === 'basic')
    return form.username.trim() && form.password ? { kind: 'basic', username: form.username.trim(), password: form.password } : null
  if (form.kind === 'header')
    return form.headerName.trim() && form.headerValue ? { kind: 'header', name: form.headerName.trim(), value: form.headerValue } : null
  return form.token.trim() ? { kind: 'bearer', token: form.token.trim() } : null
})

function clearSecrets() {
  Object.assign(form, { token: '', username: '', password: '', headerName: '', headerValue: '' })
}

function changeMode(value: string) {
  mode.value = value
  selected.value = []
  clearSecrets()
}

watch(selected, (ids) => {
  // 弹窗统一提交保存成功后，切换为安全的凭据引用并清掉明文草稿。
  if (mode.value === 'new' && ids.length) {
    mode.value = 'saved'
    clearSecrets()
  }
})
watch(() => [props.kind, props.location], () => {
  // 切换目标必须重新选择凭据，不把当前目标的认证默认带到另一个来源。
  mode.value = 'public'
  selected.value = []
  clearSecrets()
})
watch(() => props.credentials, (credentials) => {
  selected.value = selected.value.filter(id => credentials.some(item => item.id === id))
  if (mode.value === 'saved' && !credentials.length)
    mode.value = 'public'
})
watch([mode, selected], () => {
  pending.value = mode.value === 'new' || (mode.value === 'saved' && !selected.value.length)
}, { immediate: true })
watch([mode, scope, authentication], () => {
  draft.value = mode.value === 'new' && scope.value && authentication.value
    ? {
        ...scope.value,
        name: scope.value.name.slice(0, 128),
        purposes: props.kind === 'github' ? ['metadata', 'artifact'] : ['artifact'],
        authentication: authentication.value,
      }
    : null
}, { immediate: true })
</script>

<template>
  <div class="grid gap-4">
    <BaseFormItem label="下载认证">
      <template #label-extra>
        <PluginHelpPopover label="下载认证说明">
          <p class="m-0">
            {{ kind === 'github' ? '公开仓库无需认证，私有仓库需要有读取权限的 GitHub 令牌' : '仅在下载地址要求认证时填写' }}
          </p>
          <p class="m-0">
            认证信息在查询或解析时保存，仅用于当前来源，不会提供给插件
          </p>
        </PluginHelpPopover>
      </template>
      <BaseSelect :model-value="mode" :options="modeOptions" :disabled="disabled" class="w-full" aria-label="下载认证" @update:model-value="changeMode" />
    </BaseFormItem>
    <template v-if="mode === 'new'">
      <BaseFormItem v-if="kind === 'url'" label="认证方式">
        <BaseSelect v-model="form.kind" :options="authOptions" :disabled="disabled" class="w-full" aria-label="认证方式" @update:model-value="clearSecrets" />
      </BaseFormItem>
      <BaseFormItem v-if="kind === 'github' || form.kind === 'bearer'" :label="kind === 'github' ? 'GitHub 令牌' : '访问令牌'" required>
        <BaseInput v-model="form.token" type="password" autocomplete="new-password" :disabled="disabled" aria-label="下载令牌" placeholder="输入令牌" />
      </BaseFormItem>
      <div v-else-if="form.kind === 'basic'" class="grid gap-4 sm:grid-cols-2">
        <BaseFormItem label="用户名" required>
          <BaseInput v-model="form.username" autocomplete="off" :disabled="disabled" aria-label="下载用户名" />
        </BaseFormItem>
        <BaseFormItem label="密码" required>
          <BaseInput v-model="form.password" type="password" autocomplete="new-password" :disabled="disabled" aria-label="下载密码" />
        </BaseFormItem>
      </div>
      <div v-else class="grid gap-4 sm:grid-cols-2">
        <BaseFormItem label="请求头名称" required>
          <BaseInput v-model="form.headerName" :disabled="disabled" aria-label="请求头名称" placeholder="X-Download-Token" />
        </BaseFormItem>
        <BaseFormItem label="请求头值" required>
          <BaseInput v-model="form.headerValue" type="password" autocomplete="new-password" :disabled="disabled" aria-label="请求头值" />
        </BaseFormItem>
      </div>
    </template>
    <SourceCredentialPicker
      v-else-if="mode === 'saved'"
      v-model="selected"
      :credentials="credentials"
      :disabled="disabled"
      @delete="$emit('deleteCredential', $event)"
    />
  </div>
</template>
