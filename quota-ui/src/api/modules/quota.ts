import { request } from '../request'

export interface DailyCalibration {
  amountUsd: string
  atMs: number
  active?: boolean
  windowStartMs: number
  windowEndMs: number
  timezone: string
}

export interface AccountRule {
  accountId: string
  fiveHourLimitPercent: number | null
  weeklyLimitPercent: number | null
  dailyLimitUsd: string | null
  dailyCalibration?: DailyCalibration | null
}

export interface Configuration {
  accounts: AccountRule[]
  maxObservationAgeSeconds: number
  dailyTimezone: string
}

export interface AccountInfo {
  accountId: string
  name: string | null
  email: string | null
  enabled: boolean | null
}

export interface Reason {
  code: string
  message: string
  windowSeconds?: number
  usedPercent?: number
  limitPercent?: number
  resetAtMs?: number
}

export interface QuotaWindow {
  windowSeconds: number
  usedPercent: number | null
  resetAtMs: number | null
}

export interface DailyUsage {
  limitUsd: string | null
  usedUsd: string | null
  remainingUsd: string | null
  status: 'disabled' | 'ready' | 'reached' | 'unknown' | 'stale'
  complete?: boolean
  unknownCostCount: number
  inFlightCount: number
  observedAtMs: number | null
  resetAtMs: number | null
  timezone: string
  windowStartMs?: number | null
  windowEndMs?: number | null
  ledgerUsedUsd?: string | null
  postCalibrationUsedUsd?: string | null
  calibration?: DailyCalibration | null
  incompleteReasons?: string[]
  knownCostCount?: number
  ledgerUnknownCostCount?: number
  ledgerKnownCostCount?: number
  ledgerInFlightCount?: number
  currency: 'USD'
}

export interface AccountOverview extends AccountInfo {
  assessment: { accountId: string, excluded: boolean, reasons: Reason[] }
  quota: { observedAtMs: number | null, windows: QuotaWindow[] } | null
  dailyUsage?: DailyUsage
}

export interface Overview {
  revision: number
  configuration: Configuration
  evaluatedAtMs: number
  accounts: AccountOverview[]
}

export function getOverview() {
  return request<Overview>({
    method: 'GET',
    path: 'quota/overview',
  })
}

export function getAccounts(cursor?: string) {
  return request<{ items: AccountInfo[], nextCursor: string | null }>({
    method: 'GET',
    path: 'quota/accounts',
    query: new URLSearchParams({
      limit: '100',
      ...(cursor
        ? { cursor }
        : {}),
    }).toString(),
  })
}

export function saveConfiguration(expectedRevision: number, configuration: Configuration) {
  return request<{ saved: true, reloadRequired: true }>({
    method: 'POST',
    path: 'quota/configuration',
    contentType: 'application/json',
    body: JSON.stringify({ expectedRevision, configuration }),
  })
}

export function saveCalibration(expectedRevision: number, accountId: string, amountUsd: string | null) {
  return request<{ saved: true, reloadRequired: true }>({
    method: 'POST',
    path: 'quota/calibration',
    contentType: 'application/json',
    body: JSON.stringify({ expectedRevision, accountId, amountUsd }),
  })
}
