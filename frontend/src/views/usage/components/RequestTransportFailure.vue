<script setup lang="ts">
import type { OpsErrorMetadata, RequestTraceEvent } from '@/api'
import { CircleAlert } from '@lucide/vue'
import { computed } from 'vue'

const props = defineProps<{ events: RequestTraceEvent[], metadata?: OpsErrorMetadata }>()

const reasonLabels: Record<string, string> = {
  reset_without_closing_handshake: '上游连接中断，未收到 WebSocket 关闭握手',
  tcp_reset: '上游 TCP 连接被重置',
  connection_aborted: '上游连接被中止',
  broken_pipe: '写入失败，上游连接已断开',
  unexpected_eof: '上游连接意外结束',
  transport_timeout: '上游连接传输超时',
  receive_idle_timeout: '等待上游事件超时',
  tls_error: '上游 TLS 连接异常',
  protocol_error: '上游 WebSocket 协议异常',
  capacity_error: '上游 WebSocket 数据超出容量限制',
  io_error: '上游连接读写失败',
  transport_error: '上游连接传输失败',
  outbound_transport_error: '向上游连接发送请求失败',
}

function text(value: unknown) {
  return typeof value === 'string' && value ? value : undefined
}

function duration(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0
    ? `${(value / 1000).toLocaleString('zh-CN', { maximumFractionDigits: 2 })} 秒`
    : undefined
}

const diagnosis = computed(() => {
  const newestFirst = [...props.events].reverse()
  const attempt = newestFirst.find(event => event.stage === 'attempt.failed')
  const rawDiagnostic = attempt?.data.diagnostic
  const diagnostic = rawDiagnostic && typeof rawDiagnostic === 'object'
    ? rawDiagnostic as Record<string, unknown>
    : undefined
  const failure = newestFirst.find(event => [
    'upstream.read.failed',
    'upstream.send.failed',
    'upstream.eof',
    'upstream.close',
  ].includes(event.stage) && (!attempt || event.attemptIndex === attempt.attemptIndex))
  const connection = failure && newestFirst.find(event => event.stage === 'upstream.connection'
    && event.sequence < failure.sequence
    && event.attemptIndex === failure.attemptIndex
    && event.exchangeId === failure.exchangeId)
  const metadata = props.metadata
  const connectionId = text(failure?.data.connectionId) ?? text(connection?.data.connectionId)
    ?? metadata?.upstreamConnectionId
  const storedReason = metadata?.upstreamConnectionExitReason
  const preciseReason = text(diagnostic?.code) ?? text(failure?.data.failureReason)
  const reason = preciseReason ?? storedReason
  const message = text(diagnostic?.message)
  if (!failure && !message && !storedReason)
    return null

  const reused = failure?.data.reused ?? connection?.data.reused
  const closeCode = failure?.data.code
  const fallback = failure?.stage === 'upstream.close'
    ? `上游在响应完成前关闭连接${typeof closeCode === 'number' ? `（关闭码 ${closeCode}）` : ''}`
    : failure?.stage === 'upstream.eof'
      ? '上游连接在响应完成前结束'
      : failure?.stage === 'upstream.send.failed' ? '向上游发送请求失败' : '读取上游响应失败'

  return {
    message: message ?? (!preciseReason && storedReason === 'tcp_reset'
      ? '上游连接异常结束（旧版汇总分类，无法区分 TCP 重置与缺失关闭握手）'
      : reason ? reasonLabels[reason] ?? fallback : fallback),
    reason,
    sequence: (attempt ?? failure)?.sequence,
    elapsed: duration((attempt ?? failure)?.elapsedMs),
    fields: [
      { label: '失败阶段', value: text(diagnostic?.stage) },
      { label: '连接使用', value: reused === true ? '复用已有连接' : reused === false ? '新建连接' : undefined },
      { label: '连接年龄', value: duration(failure?.data.connectionAgeMs ?? metadata?.upstreamConnectionAgeMs) },
      { label: '距最近活动', value: duration(failure?.data.connectionIdleMs ?? metadata?.upstreamConnectionIdleMs) },
      { label: '最后事件', value: failure?.data.lastEventType === null ? '尚未收到事件' : text(failure?.data.lastEventType) },
      { label: '连接 ID', value: connectionId },
    ].filter(field => field.value !== undefined),
  }
})
</script>

<template>
  <aside v-if="diagnosis" class="mb-3 rounded-cp bg-cp-warning-container p-3" aria-label="上游失败诊断">
    <div class="flex items-start gap-2">
      <CircleAlert :size="16" class="mt-0.5 shrink-0 text-cp-warning-text" aria-hidden="true" />
      <div class="min-w-0 flex-1">
        <p class="m-0 text-cp-sm font-bold text-cp-text">
          {{ diagnosis.message }}
        </p>
        <p class="mt-1 mb-0 break-all font-mono text-cp-xs tabular-nums text-cp-text-secondary">
          <span v-if="diagnosis.sequence !== undefined">#{{ diagnosis.sequence }} · +{{ diagnosis.elapsed }} · </span>{{ diagnosis.reason }}
        </p>
      </div>
    </div>
    <dl v-if="diagnosis.fields.length" class="mt-3 mb-0 grid gap-x-5 gap-y-2 text-cp-xs sm:grid-cols-2">
      <div v-for="field in diagnosis.fields" :key="field.label" class="min-w-0">
        <dt class="text-cp-text-secondary">
          {{ field.label }}
        </dt>
        <dd class="mt-0.5 ml-0 break-all font-mono leading-relaxed text-cp-text">
          {{ field.value }}
        </dd>
      </div>
    </dl>
    <p v-if="!diagnosis.reason" class="mt-3 mb-0 text-cp-xs leading-relaxed text-cp-text-secondary">
      这条记录未保存底层错误分类，需用请求 ID 检索留存的服务器日志
    </p>
  </aside>
</template>
