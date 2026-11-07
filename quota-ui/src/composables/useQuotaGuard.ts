import type { AccountInfo, AccountRule, Configuration, Overview } from '../api/modules/quota'
import { computed, onBeforeUnmount, onMounted, ref, shallowRef } from 'vue'
import { getAccounts, getOverview, saveCalibration, saveConfiguration } from '../api/modules/quota'
import { RequestError } from '../api/request'
import { canonicalTimezone, cloneConfiguration, configurationKey, isValidTimezone, isValidUsd } from '../presenter'

export function useQuotaGuard() {
  const overview = shallowRef<Overview>()
  const baseline = shallowRef<Configuration>()
  const baselineRevision = shallowRef<number>()
  const draft = ref<Configuration>({ accounts: [], maxObservationAgeSeconds: 300, dailyTimezone: 'UTC' })
  const loading = ref(false)
  const saving = ref(false)
  const saved = ref(false)
  const error = ref('')
  const conflict = ref(false)
  const candidates = ref<AccountInfo[]>([])
  const candidatesLoaded = ref(false)
  const candidatesLoading = ref(false)
  const candidatesError = ref('')
  const nextCursor = ref<string | null>(null)
  const dirty = computed(() => baseline.value !== undefined && configurationKey(draft.value) !== configurationKey(baseline.value))
  const invalidDailyLimits = computed(() => draft.value.accounts.some(rule => rule.dailyLimitUsd !== null && !isValidUsd(rule.dailyLimitUsd)))
  const invalidTimezone = computed(() => !isValidTimezone(draft.value.dailyTimezone))
  let disposed = false
  let timer: ReturnType<typeof setInterval> | undefined

  async function refresh() {
    if (loading.value || saving.value || saved.value)
      return
    const wasDirty = dirty.value
    const startedDraft = configurationKey(draft.value)
    loading.value = true
    error.value = ''
    try {
      const result = await getOverview()
      if (disposed)
        return
      // 旧版响应省略日限额时按关闭处理，避免刷新后出现虚假的未保存修改
      result.configuration = cloneConfiguration(result.configuration)
      overview.value = result
      // 刷新仅更新观测，不覆盖请求期间产生的草稿，也不静默接受新的保存版本
      if (!wasDirty && configurationKey(draft.value) === startedDraft) {
        baseline.value = cloneConfiguration(result.configuration)
        baselineRevision.value = result.revision
        draft.value = cloneConfiguration(result.configuration)
        conflict.value = false
      }
      else if (baselineRevision.value !== result.revision) {
        conflict.value = true
      }
    }
    catch (failure) {
      if (!disposed) {
        error.value = failure instanceof Error
          ? failure.message
          : '读取额度保护状态失败'
      }
    }
    finally {
      if (!disposed)
        loading.value = false
    }
  }

  async function loadCandidates() {
    if (candidatesLoading.value || saved.value || (candidatesLoaded.value && !nextCursor.value))
      return
    candidatesLoading.value = true
    candidatesError.value = ''
    try {
      const result = await getAccounts(nextCursor.value ?? undefined)
      if (disposed)
        return
      candidates.value = [
        ...new Map([
          ...candidates.value,
          ...result.items,
        ].map(account => [account.accountId, account])).values(),
      ]
      nextCursor.value = result.nextCursor
      candidatesLoaded.value = true
    }
    catch (failure) {
      if (!disposed) {
        candidatesError.value = failure instanceof Error
          ? failure.message
          : '读取账号列表失败'
      }
    }
    finally {
      if (!disposed)
        candidatesLoading.value = false
    }
  }

  function addAccount(account: AccountInfo) {
    if (draft.value.accounts.some(rule => rule.accountId === account.accountId) || draft.value.accounts.length >= 1000)
      return
    draft.value.accounts.push({
      accountId: account.accountId,
      fiveHourLimitPercent: 95,
      weeklyLimitPercent: 95,
      dailyLimitUsd: null,
      dailyCalibration: null,
    })
  }

  function updateRule(rule: AccountRule) {
    const index = draft.value.accounts.findIndex(item => item.accountId === rule.accountId)
    if (index !== -1)
      draft.value.accounts[index] = rule
  }

  function removeAccount(accountId: string) {
    draft.value.accounts = draft.value.accounts.filter(rule => rule.accountId !== accountId)
  }

  function discard() {
    if (!overview.value || saving.value || saved.value)
      return
    baseline.value = cloneConfiguration(overview.value.configuration)
    baselineRevision.value = overview.value.revision
    draft.value = cloneConfiguration(overview.value.configuration)
    conflict.value = false
    error.value = ''
  }

  function normalizeDailyTimezone() {
    const value = draft.value.dailyTimezone
    // Keep saved aliases unchanged when editing other settings: a timezone
    // string change intentionally clears calibration on the server.
    if (value === baseline.value?.dailyTimezone)
      return isValidTimezone(value)
    const normalized = canonicalTimezone(value)
    if (normalized === null)
      return false
    draft.value.dailyTimezone = normalized
    return true
  }

  async function save() {
    if (!dirty.value || baselineRevision.value === undefined || saving.value || conflict.value || saved.value || invalidDailyLimits.value || invalidTimezone.value)
      return
    if (!normalizeDailyTimezone() || !dirty.value)
      return
    saving.value = true
    error.value = ''
    try {
      await saveConfiguration(baselineRevision.value, cloneConfiguration(draft.value))
      if (!disposed) {
        saved.value = true
        // 新配置会更换实例 revision，停止使用旧目标，交给宿主已有刷新入口加载新页面
        clearInterval(timer)
      }
    }
    catch (failure) {
      if (!disposed) {
        error.value = failure instanceof Error
          ? failure.message
          : '保存配置失败'
        conflict.value = failure instanceof RequestError && failure.status === 409
      }
    }
    finally {
      if (!disposed)
        saving.value = false
    }
  }

  async function calibrate(accountId: string, amountUsd: string | null) {
    if (dirty.value || baselineRevision.value === undefined || loading.value || saving.value || conflict.value || saved.value)
      return
    if (amountUsd !== null && !isValidUsd(amountUsd))
      return
    saving.value = true
    error.value = ''
    try {
      await saveCalibration(baselineRevision.value, accountId, amountUsd)
      if (!disposed) {
        saved.value = true
        clearInterval(timer)
      }
    }
    catch (failure) {
      if (!disposed) {
        error.value = failure instanceof Error ? failure.message : '保存费用校准失败'
        conflict.value = failure instanceof RequestError && failure.status === 409
      }
    }
    finally {
      if (!disposed)
        saving.value = false
    }
  }

  onMounted(() => {
    void refresh()
    timer = setInterval(() => {
      if (!dirty.value)
        void refresh()
    }, 30000)
  })
  onBeforeUnmount(() => {
    disposed = true
    clearInterval(timer)
  })

  return {
    overview,
    baseline,
    draft,
    loading,
    saving,
    saved,
    error,
    conflict,
    dirty,
    invalidDailyLimits,
    invalidTimezone,
    normalizeDailyTimezone,
    candidates,
    candidatesLoaded,
    candidatesLoading,
    candidatesError,
    nextCursor,
    refresh,
    loadCandidates,
    addAccount,
    updateRule,
    removeAccount,
    discard,
    save,
    calibrate,
  }
}
