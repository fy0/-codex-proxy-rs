import type { RequestOptions } from '../request'
import request from '../request'

export interface TurnStateConfig {
  enabled: boolean
  missingStatePolicy: 'allow' | 'pause'
  targetLength: number
  ttlSeconds: number
  refreshAfterSeconds: number
  retrySeconds: number
  jitterSeconds: number
  budget: number
  idleSeconds: number
  timezone: string
  originator: string
  userAgent: string
  includeAccountProxy: boolean
  includeDirect: boolean
  proxyIds: string[]
  stopStrategy: 'headers' | 'first_output' | 'mixed'
}

export function defaultTurnStateConfig(enabled = false): TurnStateConfig {
  return { enabled, missingStatePolicy: 'allow', targetLength: 292, ttlSeconds: 3600, refreshAfterSeconds: 2100, retrySeconds: 30, jitterSeconds: 15, budget: 40, idleSeconds: 300, timezone: 'UTC', originator: 'codex-tui', userAgent: '', includeAccountProxy: true, includeDirect: false, proxyIds: [], stopStrategy: 'headers' }
}

export interface TurnStateProbePreview {
  userAgent: string
  version: string
  timezone: string
  currentDate: string
}

export function previewTurnState(config: TurnStateConfig) {
  return request<TurnStateProbePreview>({
    url: '/api/admin/accounts/turn-state/preview',
    method: 'POST',
    data: { config },
    silent: true,
  })
}

export function copyTurnState(data: { accountId: string, model: string, issuedAt: number }) {
  return request<{ value: string, issuedAt: number }>({
    url: '/api/admin/accounts/turn-state/copy',
    method: 'POST',
    data,
  })
}

export interface TurnStateObservation {
  accountId: string
  model: string
  observedAt: number
  source: string
  requestStateSource?: string | null
  responseSource?: string | null
  probeTrigger?: string | null
  outcome: string
  httpStatus: number | null
  tokenLength: number | null
  issuedAt: number | null
  egress: string
  shape: string | null
  effort: string | null
  elapsedMs: number
  probeId: string | null
  stopMode: string | null
  stopReason: string | null
}

export interface TurnStateInstallation {
  installedAt: number
  issuedAt: number
  tokenLength: number
  source: string
  acquiredAt: number
  attempts: number
  huntSeconds: number
}

export interface TurnStateStatus {
  accountId: string
  accountName: string
  accountEmail?: string | null
  model: string
  config: TurnStateConfig
  tokenLength: number | null
  issuedAt: number | null
  ageSeconds: number | null
  active: boolean
  hasInstalledState: boolean
  businessStatus: 'ready' | 'manual_disabled' | 'waiting_for_state' | 'model_denied' | 'quota_exhausted' | 'rate_limited' | 'account_error'
  accountEnabled: boolean
  huntAttempts: number
  nextProbeAt: number | null
  manualProbeRequestedAt?: number | null
  manualOverride?: boolean
  candidateIssuedAt?: number | null
  candidateLength?: number | null
  configured?: boolean
  observations: TurnStateObservation[]
  installations: TurnStateInstallation[]
}

export function getTurnStateStatus(accountId?: string, options: RequestOptions = {}) {
  return request<TurnStateStatus[]>({
    url: '/api/admin/accounts/turn-state',
    method: 'GET',
    params: { accountId },
    ...options,
  })
}

export function configureTurnState(data: { accountId: string, model: string, config: TurnStateConfig }) {
  return request<{ accountId: string, configRevision: number }>({
    url: '/api/admin/accounts/turn-state/configure',
    method: 'POST',
    data,
  })
}

export function probeTurnState(data: { accountId: string, model: string }) {
  return request<{ accountId: string, configRevision: number }>({
    url: '/api/admin/accounts/turn-state/probe',
    method: 'POST',
    data,
  })
}

export function applyTurnState(data: { accountId: string, model: string, issuedAt: number }) {
  return request<{ accountId: string, configRevision: number }>({
    url: '/api/admin/accounts/turn-state/apply',
    method: 'POST',
    data,
  })
}

export function removeTurnState(data: { accountId: string, model: string, issuedAt: number }) {
  return request<{ accountId: string, configRevision: number }>({
    url: '/api/admin/accounts/turn-state/remove',
    method: 'POST',
    data,
  })
}
