import { request } from '../request'

export interface AccountRule {
  accountId: string
  fiveHourLimitPercent: number | null
  weeklyLimitPercent: number | null
}

export interface Configuration {
  accounts: AccountRule[]
  maxObservationAgeSeconds: number
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

export interface AccountOverview extends AccountInfo {
  assessment: { accountId: string, excluded: boolean, reasons: Reason[] }
  quota: { observedAtMs: number | null, windows: QuotaWindow[] } | null
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
