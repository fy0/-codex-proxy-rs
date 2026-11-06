import type { AccountInfo, AccountRule, Configuration, QuotaWindow } from './api/modules/quota'

export function accountName(account?: AccountInfo) {
  return account?.name || account?.email || account?.accountId || '账号资料不可用'
}

export function cloneConfiguration(configuration: Configuration): Configuration {
  return {
    maxObservationAgeSeconds: configuration.maxObservationAgeSeconds,
    accounts: configuration.accounts.map(rule => ({ ...rule, dailyLimitUsd: rule.dailyLimitUsd ?? null })),
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

export function highestUsageWindow(windows: QuotaWindow[] | undefined, seconds: number) {
  // 同周期可能有多个通用窗口，不能只显示较低的第一个值
  return windows?.filter(window => window.windowSeconds === seconds)
    .reduce<QuotaWindow | undefined>((highest, window) => {
      if (!highest || (window.usedPercent != null && (highest.usedPercent == null || window.usedPercent > highest.usedPercent)))
        return window
      return highest
    }, undefined)
}
