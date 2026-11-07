<script setup lang="ts">
import type { DailyCalibration, DailyUsage } from '../api/modules/quota'
import { computed, ref, useId, watch } from 'vue'
import { formatUsd, formatZonedDate, isValidUsd } from '../presenter'

const props = defineProps<{
  accountName: string
  usage?: DailyUsage
  savedLimit?: string | null
  savedCalibration?: DailyCalibration | null
  timezone?: string
  disabled?: boolean
  calibrationDisabled?: boolean
}>()
const emit = defineEmits<{ calibrate: [amountUsd: string | null] }>()
const limit = defineModel<string | null>({ required: true })
const inputId = useId()
const previousLimit = ref(limit.value ?? '')
const calibrationAmount = ref('')
const invalid = computed(() => limit.value !== null && !isValidUsd(limit.value))
const calibrationInvalid = computed(() => calibrationAmount.value !== '' && !isValidUsd(calibrationAmount.value))
const pending = computed(() => limit.value !== (props.savedLimit ?? null))
const zone = computed(() => props.usage?.timezone || props.timezone || 'UTC')
const known = computed(() => props.usage?.usedUsd != null && formatUsd(props.usage.usedUsd) !== '未知')
const complete = computed(() => known.value && (props.usage?.complete ?? ['ready', 'reached'].includes(props.usage?.status ?? '')))
const canCalibrate = computed(() => !props.disabled && !props.calibrationDisabled && !pending.value && props.savedLimit != null)
const reasons = computed(() => props.usage?.incompleteReasons ?? [])
const reasonText = computed(() => {
  const labels: Record<string, string> = {
    not_observed: '尚未读取本日明细',
    window_changed: '日窗口已变化，等待本日明细',
    calibration_changed: '校准已更新，等待新增费用明细',
    unpriced_records: '存在尚未计价的成功请求',
    unsupported_cost: '部分费用的币种或格式无法累计',
    read_failed: '部分费用明细读取失败',
    read_timeout: '本次读取达到时间预算，已保留已知金额',
    page_limit: '已读取部分明细，尚未覆盖全部记录',
    invalid_records: '部分记录无法确认归属，未计入金额',
    invalid_cursor: '费用分页未完整结束',
    observation_stale: '费用观测较旧，等待更新',
  }
  return [...new Set(reasons.value.map(reason => labels[reason] || `其他未齐原因：${reason}`))]
})
const state = computed(() => {
  if (!props.usage || props.usage.status === 'disabled') {
    if (pending.value && limit.value !== null)
      return { label: '保存后生效', tone: 'neutral' }
    if (props.savedLimit != null)
      return { label: '等待观测', tone: 'info' }
    return { label: '未开启', tone: 'neutral' }
  }
  return {
    ready: { label: props.usage.calibration ? '已校准' : '记录正常', tone: 'success' },
    reached: { label: '达到限额', tone: 'warning' },
    unknown: { label: reasons.value.includes('unpriced_records') ? '有未计价请求' : '记录未齐', tone: 'info' },
    stale: { label: '待刷新', tone: 'info' },
  }[props.usage.status]
})
const width = computed(() => {
  if (!known.value || props.usage?.limitUsd == null)
    return 0
  const savedLimit = Number(props.usage.limitUsd)
  return savedLimit === 0
    ? 100
    : Math.min(100, Math.max(0, Number(props.usage.usedUsd) / savedLimit * 100))
})
const description = computed(() => {
  if (props.usage?.status === 'unknown')
    return '按已知费用判断，未知记录不单独拦截'
  if (props.usage?.status === 'stale')
    return '等待费用更新，未确认超限不拦截'
  if (pending.value)
    return '保存后按新日限额保护'
  if (props.usage?.status === 'reached')
    return `已达日限额，等待 ${zone.value} 换日`
  if (props.usage?.remainingUsd != null && props.savedLimit != null)
    return `剩余 ${formatUsd(props.usage.remainingUsd)}`
  return '覆盖该账号的全部 CPR 请求'
})
const zeroLimit = computed(() => limit.value !== null && isValidUsd(limit.value) && /^0+(?:\.0+)?$/.test(limit.value))
const resetTime = computed(() => props.usage?.resetAtMs == null
  ? `${zone.value} 当地 00:00 换日`
  : `重置 ${formatZonedDate(props.usage.resetAtMs, zone.value)} · ${zone.value}`)
const ledgerAmount = computed(() => props.usage?.ledgerUsedUsd ?? (props.usage?.calibration ? null : props.usage?.usedUsd))

watch(limit, (value) => {
  if (value !== null)
    previousLimit.value = value
})

function toggle() {
  limit.value = limit.value === null ? previousLimit.value : null
}
function updateLimit(event: Event) {
  limit.value = (event.target as HTMLInputElement).value
}
function applyCalibration() {
  if (canCalibrate.value && isValidUsd(calibrationAmount.value))
    emit('calibrate', calibrationAmount.value)
}
</script>

<template>
  <section class="qg-window qg-daily-window" :class="{ 'qg-window-warning': state.tone === 'warning', 'qg-window-unknown': !known }" :aria-label="`${accountName} 每日费用`">
    <div class="qg-window-heading">
      <span>每日费用 <span class="qg-currency">USD</span></span>
      <span class="qg-badge" :class="`qg-badge-${state.tone}`">{{ state.label }}</span>
    </div>
    <div class="qg-daily-usage">
      <strong :title="known ? formatUsd(usage?.usedUsd) : undefined">{{ known ? formatUsd(usage?.usedUsd) : '—' }}</strong>
      <span>{{ known ? (usage?.calibration ? '校准后的本日累计' : complete ? '本日已记录' : '本日已知费用') : (savedLimit != null ? '尚无可计价记录' : '开启后统计') }}</span>
    </div>
    <div v-if="usage?.windowStartMs != null && usage?.windowEndMs != null" class="qg-daily-range">
      <span>{{ formatZonedDate(usage.windowStartMs, zone) }} → {{ formatZonedDate(usage.windowEndMs, zone) }}</span>
      <span>{{ zone }}</span>
    </div>
    <div class="qg-progress" role="progressbar" :aria-label="`${accountName} 每日费用使用率`" aria-valuemin="0" aria-valuemax="100" :aria-valuenow="known ? width : undefined" :aria-valuetext="known ? `${complete ? '本日已记录' : '本日已知'} ${formatUsd(usage?.usedUsd)} / ${formatUsd(usage?.limitUsd)}，按已保存日限额` : state.label">
      <div class="qg-progress-fill" :style="{ width: `${width}%` }" />
    </div>
    <div class="qg-window-controls qg-daily-controls">
      <div class="qg-switch-label">
        <button type="button" class="qg-switch" role="switch" :aria-checked="limit !== null" :aria-label="`${accountName} 每日费用保护`" :disabled="disabled" @click="toggle"><span /></button>
        <span>{{ limit !== null ? '日限额' : '未开启保护' }}</span>
      </div>
      <div v-if="limit !== null" class="qg-number-field qg-money-field" :class="{ 'qg-field-invalid': invalid }">
        <input :id="inputId" type="text" inputmode="decimal" :value="limit" :aria-label="`${accountName} 每日费用上限 USD`" :aria-invalid="invalid" :aria-describedby="`${inputId}-help`" placeholder="填写金额" autocomplete="off" spellcheck="false" :disabled="disabled" @input="updateLimit">
        <span>USD</span>
      </div>
    </div>
    <p :id="`${inputId}-help`" class="qg-daily-description" :class="{ 'qg-daily-error': invalid }">
      {{ invalid ? '填写非负金额：最多 18 位整数、12 位小数' : zeroLimit ? '0 将立即暂停该账号分配' : description }}
    </p>
    <p class="qg-window-reset qg-daily-reset">
      <span>{{ resetTime }}</span>
      <span v-if="usage?.inFlightCount">{{ usage.inFlightCount }} 笔待结算</span>
      <span v-if="usage?.unknownCostCount">{{ usage.unknownCostCount }} 笔费用未知</span>
    </p>
    <details class="qg-calibration">
      <summary>记账明细与手工校准</summary>
      <dl class="qg-ledger-breakdown">
        <div><dt>本日账本已知费用</dt><dd>{{ formatUsd(ledgerAmount) }}</dd></div>
        <div v-if="usage?.ledgerKnownCostCount != null || usage?.knownCostCount != null"><dt>原账本已计价记录</dt><dd>{{ usage?.ledgerKnownCostCount ?? usage?.knownCostCount }} 笔</dd></div>
        <div v-if="usage?.ledgerUnknownCostCount"><dt>原账本未计价记录</dt><dd>{{ usage.ledgerUnknownCostCount }} 笔</dd></div>
        <template v-if="usage?.calibration">
          <div><dt>手工校准基准</dt><dd>{{ formatUsd(usage.calibration.amountUsd) }}</dd></div>
          <div><dt>校准后新增费用</dt><dd>{{ formatUsd(usage.postCalibrationUsedUsd) }}</dd></div>
        </template>
      </dl>
      <p v-for="reason in reasonText" :key="reason" class="qg-ledger-reason">{{ reason }}</p>
      <p v-if="usage?.calibration" class="qg-calibration-note">校准于 {{ formatZonedDate(usage.calibration.atMs, usage.calibration.timezone) }} · {{ usage.calibration.timezone }}，本日窗口结束后失效。</p>
      <p v-else-if="savedCalibration" class="qg-calibration-note">上次校准已失效；当前按本日账本累计。</p>
      <p class="qg-calibration-note">设置当前本日已用金额，此后新发起的请求继续累计。校准前已发起的请求（含未结算）视为已包含在基准中；下次当地换日后失效。</p>
      <div class="qg-calibration-entry">
        <label :for="`${inputId}-calibration`">当前本日已用 USD</label>
        <input :id="`${inputId}-calibration`" v-model="calibrationAmount" type="text" inputmode="decimal" :aria-label="`${accountName} 校准本日已用 USD`" :aria-invalid="calibrationInvalid" :disabled="!canCalibrate" placeholder="输入核对后的金额" autocomplete="off" spellcheck="false" @keydown.enter.prevent="applyCalibration">
        <p v-if="calibrationInvalid" class="qg-daily-error">填写非负金额，最多 18 位整数、12 位小数。</p>
      </div>
      <div class="qg-calibration-actions">
        <button type="button" class="qg-button qg-button-small qg-button-accent" :disabled="!canCalibrate || !isValidUsd(calibrationAmount)" @click="applyCalibration">设为当前已用</button>
        <button v-if="savedCalibration" type="button" class="qg-button qg-button-small qg-button-quiet" :disabled="!canCalibrate" @click="emit('calibrate', null)">恢复自动记账</button>
      </div>
      <p v-if="!canCalibrate" class="qg-calibration-note">{{ savedLimit == null ? '先开启并保存日限额，再进行校准。' : '请先保存或撤销其他修改，并刷新页面后再校准。' }}</p>
    </details>
  </section>
</template>
