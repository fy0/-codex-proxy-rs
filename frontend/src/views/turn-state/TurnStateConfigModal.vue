<script setup lang="ts">
import type { getAccounts, TurnStateConfig, TurnStateProbePreview, TurnStateStatus } from '@/api'
import { Copy, Eye, EyeOff, Save } from '@lucide/vue'
import { watchDebounced } from '@vueuse/core'
import { computed, ref, useId, watch } from 'vue'
import { configureTurnState, defaultTurnStateConfig, previewTurnState } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseNumberInput from '@/components/base/BaseNumberInput.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useCopyText } from '@/composables/useCopyText'
import { useProxyCatalog } from '@/composables/useProxyCatalog'

const props = defineProps<{
  bucket: TurnStateStatus | null
  accounts: Awaited<ReturnType<typeof getAccounts>>['items']
}>()
const emit = defineEmits<{ saved: [] }>()
const open = defineModel<boolean>({ required: true })
const accountId = ref('')
const model = ref('gpt-6-astra')
const config = ref<TurnStateConfig>(defaultTurnStateConfig(true))
const proxySearch = ref('')
const { proxies, loading: loadingProxies } = useProxyCatalog()
const { loading: saving, run } = useAsyncAction()
const preview = ref<TurnStateProbePreview | null>(null)
const previewError = ref('')
const previewLoading = ref(false)
const userAgentMode = ref('auto')
const showWebhook = ref(false)
const copyText = useCopyText()
const timezoneListId = useId()
const timezones = ['UTC', 'Asia/Shanghai', 'Asia/Taipei', 'Asia/Hong_Kong', 'Asia/Tokyo', 'Asia/Singapore', 'America/Los_Angeles', 'America/New_York', 'Europe/London', 'Europe/Berlin']
let previewVersion = 0
const accountOptions = computed(() => props.accounts.filter(account => account.provider === 'openai' && account.authenticationKind === 'oauth').map(account => ({ value: account.id, label: `${account.email?.trim() || account.name} · ${account.id}` })))
const selectedAccount = computed(() => props.accounts.find(account => account.id === accountId.value))
const existing = computed(() => !!props.bucket && props.bucket.configured !== false)
const proxyOptions = computed(() => proxies.value.filter(proxy => `${proxy.name} ${proxy.endpoint}`.toLowerCase().includes(proxySearch.value.toLowerCase())))

watch(open, (value) => {
  if (!value)
    return
  accountId.value = props.bucket?.accountId ?? ''
  model.value = props.bucket?.model ?? 'gpt-6-astra'
  config.value = props.bucket ? { ...defaultTurnStateConfig(), ...props.bucket.config, proxyIds: [...props.bucket.config.proxyIds] } : defaultTurnStateConfig(true)
  userAgentMode.value = config.value.userAgent ? 'custom' : 'auto'
  showWebhook.value = false
  proxySearch.value = ''
})

watch(userAgentMode, (mode) => {
  if (mode === 'auto')
    config.value.userAgent = ''
})

watchDebounced(() => [open.value, config.value.originator, config.value.clientVersion, config.value.userAgent, config.value.timezone], async () => {
  const version = ++previewVersion
  preview.value = null
  previewError.value = ''
  if (!open.value)
    return
  previewLoading.value = true
  try {
    const value = await previewTurnState({ ...defaultTurnStateConfig(), originator: config.value.originator, clientVersion: config.value.clientVersion, userAgent: config.value.userAgent, timezone: config.value.timezone })
    if (version === previewVersion)
      preview.value = value
  }
  catch {
    if (version === previewVersion)
      previewError.value = '预览失败，请检查正式版本号、IANA 时区、originator 和 User-Agent'
  }
  finally {
    if (version === previewVersion)
      previewLoading.value = false
  }
}, { debounce: 250 })

function selectProxy(id: string, selected: boolean) {
  config.value.proxyIds = selected ? [...new Set([...config.value.proxyIds, id])] : config.value.proxyIds.filter(value => value !== id)
}

async function save() {
  if (!accountId.value || !model.value.trim()) {
    toast.warning('请选择账号并填写模型')
    return
  }
  if (config.value.refreshAfterSeconds >= config.value.ttlSeconds) {
    toast.warning('轮换龄必须小于 state 有效期')
    return
  }
  if (!config.value.originator.trim() || !config.value.timezone.trim()) {
    toast.warning('请填写探测 originator 和时区')
    return
  }
  if (userAgentMode.value === 'custom' && !config.value.userAgent.trim()) {
    toast.warning('请填写自定义 User-Agent，或切换为自动')
    return
  }
  if (previewLoading.value || !preview.value || previewError.value) {
    toast.warning('请等待探测画像校验通过')
    return
  }
  if (!config.value.includeAccountProxy && !config.value.includeDirect && !config.value.proxyIds.length) {
    toast.warning('请至少选择一个探测出口')
    return
  }
  await run(async () => {
    await configureTurnState({ accountId: accountId.value, model: model.value.trim(), config: config.value })
    toast.success('轮换配置已保存')
    open.value = false
    emit('saved')
  })
}
</script>

<template>
  <BaseModal v-model="open" title="轮换配置" size="lg" :dismissible="!saving">
    <BaseForm class="grid gap-5" autocomplete="off">
      <div class="grid gap-4 sm:grid-cols-2">
        <BaseFormItem label="账号" required>
          <BaseSelect v-model="accountId" :options="accountOptions" :disabled="saving || existing" aria-label="账号" />
        </BaseFormItem>
        <BaseFormItem label="模型" required>
          <BaseInput v-model="model" :disabled="saving || existing" maxlength="256" aria-label="模型" />
        </BaseFormItem>
      </div>
      <BaseSwitch v-model="config.enabled" label="启用自动探测" show-label :disabled="saving" />
      <BaseFormItem label="无票调度策略">
        <BaseSelect v-model="config.missingStatePolicy" :options="[{ label: '继续调度', value: 'allow' }, { label: '暂停业务调度', value: 'pause' }]" aria-label="无票调度策略" :disabled="saving" />
      </BaseFormItem>
      <p v-if="config.missingStatePolicy === 'pause' && !config.enabled" role="status" class="m-0 text-cp-sm text-cp-warning-text">
        自动探测已关闭，缺票时需要手动探测并应用 state 才能恢复业务调度。
      </p>
      <BaseFormItem label="探测中断策略">
        <BaseSelect v-model="config.stopStrategy" :options="[{ label: '获得 state 即中断', value: 'headers' }, { label: '获得回应中断', value: 'first_output' }, { label: '混合', value: 'mixed' }]" aria-label="探测中断策略" :disabled="saving" />
      </BaseFormItem>
      <p class="m-0 text-cp-sm text-cp-text-secondary">
        长度为 292 的 state 目前大约只在签发后 240 秒内可用，并且后续请求必须同时带上该账号可回放的 Cookie。这里的有效期只决定网关何时停止注入。
      </p>
      <div class="grid grid-cols-2 gap-4 sm:grid-cols-3">
        <BaseFormItem label="目标长度">
          <BaseNumberInput v-model="config.targetLength" label="目标长度" :min="76" :max="4096" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="state 有效期（秒）">
          <BaseNumberInput v-model="config.ttlSeconds" label="state 有效期" :min="60" :max="3600" :step="60" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="轮换龄（秒）">
          <BaseNumberInput v-model="config.refreshAfterSeconds" label="轮换龄" :min="30" :max="3599" :step="60" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="重试间隔（秒）">
          <BaseNumberInput v-model="config.retrySeconds" label="重试间隔" :min="1" :max="3600" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="随机抖动（秒）">
          <BaseNumberInput v-model="config.jitterSeconds" label="随机抖动" :min="0" :max="3600" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="每轮探测预算">
          <BaseNumberInput v-model="config.budget" label="每轮探测预算" :min="1" :max="100" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="空闲间隔（秒）">
          <BaseNumberInput v-model="config.idleSeconds" label="空闲间隔" :min="10" :max="86400" :step="10" :disabled="saving" />
        </BaseFormItem>
      </div>
      <div class="grid gap-4 sm:grid-cols-2">
        <BaseFormItem label="探测 originator" required>
          <BaseInput v-model="config.originator" aria-label="探测 originator" maxlength="128" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="探测时区" required>
          <BaseInput v-model="config.timezone" aria-label="探测时区" :list="timezoneListId" placeholder="Asia/Shanghai" :disabled="saving" />
          <datalist :id="timezoneListId">
            <option v-for="timezone in timezones" :key="timezone" :value="timezone" />
          </datalist>
          <span class="text-cp-xs text-cp-text-secondary">IANA 时区，如 Asia/Shanghai、Asia/Taipei、UTC；用于探测环境中的日期。</span>
        </BaseFormItem>
        <BaseFormItem label="探测 User-Agent" class="sm:col-span-2">
          <BaseSegmented v-model="userAgentMode" label="User-Agent 模式" :options="[{ label: '自动', value: 'auto' }, { label: '自定义', value: 'custom' }]" :disabled="saving" />
          <BaseInput v-if="userAgentMode === 'custom'" v-model="config.userAgent" name="turn-state-http-agent" autocomplete="off" data-1p-ignore data-lpignore="true" data-bwignore aria-label="探测 User-Agent" placeholder="自定义完整 User-Agent" maxlength="1024" :disabled="saving" />
        </BaseFormItem>
        <BaseFormItem label="探测 CLI 版本" class="sm:col-span-2" required>
          <BaseInput v-model="config.clientVersion" name="turn-state-client-release" autocomplete="off" aria-label="探测 CLI 版本" placeholder="0.154.0" maxlength="64" :disabled="saving" />
          <span class="text-cp-xs text-cp-text-secondary">默认固定为正式版 0.154.0，可填写其他正式版本；不接受 alpha、beta 或 rc。自动 UA 和 version 请求头共同使用此版本，不跟随 Desktop 更新。</span>
        </BaseFormItem>
        <div class="grid min-w-0 gap-2 sm:col-span-2" aria-live="polite">
          <div class="flex items-center justify-between gap-2 text-cp-sm">
            <span>实际探测 User-Agent</span>
            <BaseIconButton label="复制探测 User-Agent" :disabled="!preview || previewLoading" @click="copyText(preview?.userAgent ?? '', { successText: 'User-Agent 已复制' })">
              <Copy class="size-4" />
            </BaseIconButton>
          </div>
          <code v-if="preview" class="whitespace-pre-wrap break-all text-cp-sm">{{ preview.userAgent }}</code>
          <span v-else :class="previewError ? 'text-cp-error-text' : 'text-cp-text-secondary'">{{ previewError || '正在生成探测画像' }}</span>
          <span v-if="preview" class="text-cp-xs text-cp-text-secondary">version 请求头：{{ preview.version }} · 探测日期：{{ preview.currentDate }}（{{ preview.timezone }}）</span>
        </div>
      </div>
      <BaseFormItem label="飞书机器人 Webhook">
        <!-- URL 保持遮罩，但不用 password 类型，避免浏览器把前面的 UA 识别为登录账号。 -->
        <BaseInput v-model="config.feishuWebhookUrl" type="url" name="turn-state-notification-endpoint" autocomplete="off" data-1p-ignore data-lpignore="true" data-bwignore :spellcheck="false" :class="{ '[&_input]:[-webkit-text-security:disc]': !showWebhook }" aria-label="飞书机器人 Webhook" placeholder="留空关闭通知" maxlength="512" :disabled="saving">
          <template #suffix>
            <BaseIconButton :label="showWebhook ? '隐藏 Webhook' : '显示 Webhook'" :disabled="saving" @click="showWebhook = !showWebhook">
              <EyeOff v-if="showWebhook" class="size-4" />
              <Eye v-else class="size-4" />
            </BaseIconButton>
          </template>
        </BaseInput>
        <span class="text-cp-xs text-cp-text-secondary">支持飞书 / Lark 自定义机器人。安装新票后推送完整 state、距上次安装时间、获取耗时和尝试次数。关键词可设为 state；不使用签名校验。</span>
      </BaseFormItem>
      <fieldset class="m-0 min-w-0 border-0 p-0">
        <legend class="mb-3 text-cp font-semibold text-cp-text">
          探测出口
        </legend>
        <div class="grid gap-3">
          <BaseCheckbox v-model="config.includeAccountProxy" :label="`账号当前出口：${selectedAccount?.outboundProxyEndpoint || '直连'}`" show-label :disabled="saving" />
          <BaseCheckbox v-model="config.includeDirect" label="额外加入直连" show-label :disabled="saving" />
          <BaseInput v-model="proxySearch" placeholder="搜索其他代理" aria-label="搜索其他代理" :disabled="saving || loadingProxies" />
          <div class="grid max-h-52 gap-3 overflow-y-auto py-1">
            <BaseCheckbox v-for="proxy in proxyOptions" :key="proxy.id" :model-value="config.proxyIds.includes(proxy.id)" :label="`${proxy.name} · ${proxy.endpoint}`" show-label :disabled="saving || (!config.proxyIds.includes(proxy.id) && config.proxyIds.length >= 32)" @update:model-value="selectProxy(proxy.id, $event)" />
            <span v-if="!proxyOptions.length" class="text-cp-sm text-cp-text-secondary">{{ loadingProxies ? '加载中' : '暂无匹配代理' }}</span>
          </div>
        </div>
      </fieldset>
    </BaseForm>
    <template #footer>
      <BaseButton :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="primary" :loading="saving" @click="save">
        <template #icon>
          <Save class="size-4" />
        </template>
        保存配置
      </BaseButton>
    </template>
  </BaseModal>
</template>
