import type { RequestOptions } from '../request'
import request from '../request'

export interface TurnStateConfig {
  enabled: boolean
  targetLength: number
  ttlSeconds: number
  refreshAfterSeconds: number
  retrySeconds: number
  jitterSeconds: number
  budget: number
  idleSeconds: number
  timezone: string
  includeAccountProxy: boolean
  includeDirect: boolean
  proxyIds: string[]
  stopStrategy: 'headers' | 'first_output' | 'mixed'
}

export function defaultTurnStateConfig(enabled = false): TurnStateConfig {
  return { enabled, targetLength: 292, ttlSeconds: 2700, refreshAfterSeconds: 2100, retrySeconds: 30, jitterSeconds: 15, budget: 40, idleSeconds: 300, timezone: 'UTC', includeAccountProxy: true, includeDirect: false, proxyIds: [], stopStrategy: 'headers' }
}

export interface TurnStateObservation {
  accountId: string
  model: string
  observedAt: number
  source: string
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
  model: string
  config: TurnStateConfig
  tokenLength: number | null
  issuedAt: number | null
  ageSeconds: number | null
  active: boolean
  accountEnabled: boolean
  huntAttempts: number
  nextProbeAt: number | null
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
