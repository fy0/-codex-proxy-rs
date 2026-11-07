import type { AccountInfo, AccountRule, Configuration, QuotaWindow } from './api/modules/quota'

export function accountName(account?: AccountInfo) {
  return account?.name || account?.email || account?.accountId || '账号资料不可用'
}

export function cloneConfiguration(configuration: Configuration): Configuration {
  return {
    maxObservationAgeSeconds: configuration.maxObservationAgeSeconds,
    dailyTimezone: configuration.dailyTimezone || 'UTC',
    accounts: configuration.accounts.map(rule => ({ ...rule, dailyLimitUsd: rule.dailyLimitUsd ?? null, dailyCalibration: rule.dailyCalibration ? { ...rule.dailyCalibration } : null })),
  }
}

export function configurationKey(configuration: Configuration) {
  return JSON.stringify({
    ...configuration,
    accounts: [...configuration.accounts].sort((a, b) => a.accountId.localeCompare(b.accountId)),
  })
}

export function isProtected(rule: AccountRule) {
  return rule.fiveHourLimitPercent !== null || rule.weeklyLimitPercent !== null || rule.dailyLimitUsd != null
}

export function isValidUsd(value: string) {
  return /^\d{1,18}(?:\.\d{1,12})?$/.test(value)
}

export function canonicalTimezone(value: string): string | null {
  // Intl also accepts numeric offsets; the backend stores named IANA zones.
  if (!value.trim() || value !== value.trim() || /^[+-]/.test(value))
    return null
  try {
    return new Intl.DateTimeFormat('en', { timeZone: value }).resolvedOptions().timeZone
  }
  catch {
    return null
  }
}

export function isValidTimezone(value: string) {
  return canonicalTimezone(value) !== null
}

export function formatZonedDate(value: number | null | undefined, timezone: string) {
  if (value == null || !Number.isFinite(new Date(value).getTime()))
    return '未返回'
  try {
    return new Intl.DateTimeFormat('zh-CN', {
      month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
      hourCycle: 'h23', timeZone: timezone,
    }).format(value)
  }
  catch {
    return '时区不可用'
  }
}

export function formatUsd(value: string | null | undefined) {
  if (value == null || !/^\d+(?:\.\d+)?$/.test(value))
    return '未知'
  // 金额展示保留十进制字符串，避免大数和小额被浮点转换舍入
  const [integer, fractional = ''] = value.split('.')
  const grouped = integer.replace(/^0+(?=\d)/, '').replace(/\B(?=(\d{3})+(?!\d))/g, ',')
  return `$${grouped}.${fractional.replace(/0+$/, '').padEnd(2, '0')}`
}

export function formatDate(value: number | null | undefined) {
  if (value == null || !Number.isFinite(new Date(value).getTime()))
    return '未知'
  return new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  }).format(value)
}

export function percent(value: number | null | undefined) {
  return value == null
    ? '未知'
    : `${new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 2 }).format(value)}%`
}

export function highestUsageWindow(windows: QuotaWindow[] | undefined, seconds: number, evaluatedAtMs = Date.now()) {
  const matching = windows?.filter(window => window.windowSeconds === seconds) ?? []
  const current = matching.filter(window => window.resetAtMs == null || window.resetAtMs > evaluatedAtMs)
  // 已重置的旧窗口只作历史展示，不能盖住仍在生效的新窗口。
  return (current.length ? current : matching)
    .reduce<QuotaWindow | undefined>((highest, window) => {
      if (!highest || (window.usedPercent != null && (highest.usedPercent == null || window.usedPercent > highest.usedPercent)))
        return window
      return highest
    }, undefined)
}