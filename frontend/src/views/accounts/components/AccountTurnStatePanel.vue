<script setup lang="ts">
import type { TurnStateStatus } from '@/api'
import { Check, Copy, Play, RefreshCw, Trash2 } from '@lucide/vue'
import { useIntervalFn } from '@vueuse/core'
import { onMounted, ref } from 'vue'
import { applyTurnState, copyTurnState, getTurnStateStatus, probeTurnState, removeTurnState } from '@/api'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import { toast } from '@/components/base/BaseToast'
import { useCopyText } from '@/composables/useCopyText'

const props = defineProps<{ accountId: string, hasAccountOverride: boolean }>()
const buckets = ref<TurnStateStatus[]>([])
const loading = ref(false)
const error = ref('')
const busy = ref(false)
const now = ref(Date.now() / 1000)
const copyText = useCopyText()
let version = 0

async function load() {
  const current = ++version
  loading.value = true
  try {
    const data = await getTurnStateStatus(props.accountId, { silent: true })
    if (current === version) {
      buckets.value = data
      error.value = ''
    }
  }
  catch {
    if (current === version)
      error.value = 'state 加载失败，请刷新重试'
  }
  finally {
    if (current === version)
      loading.value = false
  }
}
function fresh(bucket: TurnStateStatus, issuedAt: number | null | undefined) {
  return issuedAt != null && issuedAt <= now.value && issuedAt + bucket.config.ttlSeconds > now.value
}
function installed(bucket: TurnStateStatus) {
  return bucket.hasInstalledState && fresh(bucket, bucket.issuedAt)
}
function source(bucket: TurnStateStatus) {
  if (installed(bucket))
    return bucket.manualOverride ? '模型桶手动应用' : '模型桶自动安装'
  if (bucket.config.enabled || bucket.manualOverride || bucket.issuedAt != null || bucket.config.missingStatePolicy === 'pause')
    return '模型桶无有效 state，不回退通用覆盖'
  return props.hasAccountOverride ? '账号通用覆盖' : '客户端 / 会话'
}
function business(bucket: TurnStateStatus) {
  if (!bucket.accountEnabled)
    return '手动停用'
  if (bucket.businessStatus === 'ready' && bucket.config.missingStatePolicy === 'pause' && !installed(bucket))
    return '等待 state'
  const labels: Record<TurnStateStatus['businessStatus'], string> = { ready: '正常调度', manual_disabled: '手动停用', waiting_for_state: '等待 state', model_denied: '模型权限禁止', quota_exhausted: '额度耗尽', rate_limited: '限流 / 冷却中', account_error: '账号不可用' }
  return labels[bucket.businessStatus]
}
function date(value: number) {
  return new Date(value * 1000).toLocaleString()
}
async function act(bucket: TurnStateStatus, action: 'copy' | 'candidate' | 'apply' | 'remove' | 'probe') {
  if (busy.value)
    return
  const issuedAt = action === 'candidate' || action === 'apply' ? bucket.candidateIssuedAt : bucket.issuedAt
  if (action !== 'probe' && issuedAt == null)
    return
  busy.value = true
  try {
    const identity = { accountId: bucket.accountId, model: bucket.model }
    if (action === 'probe') {
      await probeTurnState(identity)
      toast.success('单次探测已排队')
    }
    else if (issuedAt != null) {
      const data = { ...identity, issuedAt }
      if (action === 'copy' || action === 'candidate') {
        const token = await copyTurnState(data)
        await copyText(token.value, { successText: 'state 已复制' })
      }
      else if (action === 'apply') {
        await applyTurnState(data)
        toast.success('state 已应用，后续请求强制使用此票')
      }
      else {
        await removeTurnState(data)
        toast.success('已移除 state，账号和自动探测开关保持不变')
      }
    }
  }
  catch {
    // 请求层统一提示；重新获取状态，避免操作已替换的票。
  }
  finally {
    await load()
    busy.value = false
  }
}
onMounted(load)
useIntervalFn(() => {
  now.value = Date.now() / 1000
  if (!loading.value && !busy.value)
    void load()
}, 5000)
</script>

<template>
  <section class="grid min-w-0 gap-3" aria-label="模型 state">
    <div class="flex items-center justify-between gap-3">
      <h3 class="m-0 text-cp font-heavy text-cp-text">
        模型 state
      </h3>
      <BaseIconButton label="刷新账号 state" :disabled="loading" @click="load">
        <RefreshCw :size="15" />
      </BaseIconButton>
    </div>
    <p class="m-0 text-cp-sm text-cp-text-secondary">
      已安装的模型 state 强制覆盖请求头和会话元数据，会话切换后仍生效。操作立即生效；移除后，自动探测开启的桶会继续寻找新票。
    </p>
    <p v-if="error" role="alert" class="m-0 text-cp-sm text-cp-error">
      {{ error }}
    </p>
    <p v-else-if="!buckets.length" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
      {{ loading ? '正在读取 state…' : '暂无模型桶，使用账号通用覆盖或客户端 state' }}
    </p>
    <div v-for="bucket in buckets" :key="bucket.model" class="grid min-w-0 gap-2 border-t border-cp-split pt-3">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <span class="min-w-0 break-all font-mono text-cp-sm font-semibold">{{ bucket.model }}</span>
        <span class="text-cp-sm" :class="business(bucket) === '正常调度' ? 'text-cp-success-text' : 'text-cp-warning-text'">{{ business(bucket) }}</span>
      </div>
      <div class="flex flex-wrap items-center justify-between gap-2">
        <span class="text-cp-sm text-cp-text-secondary">{{ source(bucket) }}</span>
        <div class="flex shrink-0 items-center gap-1">
          <BaseIconButton v-if="installed(bucket)" label="复制已安装 state" :disabled="busy" @click="act(bucket, 'copy')">
            <Copy :size="15" />
          </BaseIconButton>
          <BaseIconButton v-if="bucket.hasInstalledState" label="移除已安装 state" :disabled="busy" @click="act(bucket, 'remove')">
            <Trash2 :size="15" />
          </BaseIconButton>
          <BaseIconButton label="探测一次" :disabled="busy || !bucket.accountEnabled || bucket.manualProbeRequestedAt != null" @click="act(bucket, 'probe')">
            <Play :size="15" />
          </BaseIconButton>
        </div>
      </div>
      <p v-if="installed(bucket) && bucket.issuedAt != null" class="m-0 text-cp-xs text-cp-text-secondary">
        {{ bucket.tokenLength }} 字符 · 签发 {{ date(bucket.issuedAt) }} · 到期 {{ date(bucket.issuedAt + bucket.config.ttlSeconds) }}
      </p>
      <div v-if="fresh(bucket, bucket.candidateIssuedAt)" class="flex flex-wrap items-center justify-between gap-2">
        <span class="text-cp-sm text-cp-text-secondary">候选 {{ bucket.candidateLength }} 字符 · 尚未应用</span>
        <div class="flex items-center gap-1">
          <BaseIconButton label="复制候选 state" :disabled="busy" @click="act(bucket, 'candidate')">
            <Copy :size="15" />
          </BaseIconButton>
          <BaseIconButton label="应用候选 state" :disabled="busy || !bucket.accountEnabled || (bucket.issuedAt != null && bucket.candidateIssuedAt! <= bucket.issuedAt)" @click="act(bucket, 'apply')">
            <Check :size="15" />
          </BaseIconButton>
        </div>
      </div>
      <p class="m-0 text-cp-xs text-cp-text-secondary">
        账号{{ bucket.accountEnabled ? '启用' : '手动停用' }} · 自动探测{{ bucket.config.enabled ? '开启' : '关闭' }}
        <span v-if="!bucket.config.enabled && bucket.config.missingStatePolicy === 'pause'" class="text-cp-warning-text"> · 缺票时需手动探测并应用恢复</span>
      </p>
    </div>
  </section>
</template>
