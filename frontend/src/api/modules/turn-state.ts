import type { RequestOptions } from '../request'
import request from '../request'

export interface TurnStateConfig {
  enabled: boolean
  cookieLockEnabled: boolean
  cookieRefreshBeforeSeconds: number
  cookieGatewayIds: string
  missingStatePolicy: 'allow' | 'pause'
  detectActualModel: boolean
  targetLength: number
  ttlSeconds: number
  refreshAfterSeconds: number
  retrySeconds: number
  jitterSeconds: number
  budget: number
  idleSeconds: number
  timezone: string
  originator: string
  clientVersion: string
  userAgent: string
  includeAccountProxy: boolean
  includeDirect: boolean
  proxyIds: string[]
  stopStrategy: 'headers' | 'first_output' | 'mixed'
  feishuWebhookUrl: string
}

export function defaultTurnStateConfig(enabled = false): TurnStateConfig {
  return { enabled, cookieLockEnabled: false, cookieRefreshBeforeSeconds: 300, cookieGatewayIds: '', missingStatePolicy: 'allow', detectActualModel: false, targetLength: 292, ttlSeconds: 240, refreshAfterSeconds: 120, retrySeconds: 30, jitterSeconds: 15, budget: 40, idleSeconds: 300, timezone: 'UTC', originator: 'codex-tui', clientVersion: '0.154.0', userAgent: '', includeAccountProxy: true, includeDirect: false, proxyIds: [], stopStrategy: 'headers', feishuWebhookUrl: '' }
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

export function copyTurnState(data: { accountId: string, model: string, issuedAt: number, observationId?: string }) {
  return request<{ value: string, issuedAt: number }>({
    url: '/api/admin/accounts/turn-state/copy',
    method: 'POST',
    data,
  })
}

export interface TurnStateObservation {
  observationId?: string
  isInstalled?: boolean
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
  oailbHost?: string | null
  cookieExpiresAt?: number | null
  reportedModel?: string | null
  hasToken?: boolean
  egress: string
  shape: string | null
  effort: string | null
  elapsedMs: number
  probeId: string | null
  stopMode: string | null
  stopReason: string | null
  answer?: string | null
  answerMatch?: boolean | null
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

export interface RoutingCookieStatus {
  gatewayId: string
  pod: string
  issuedAt: number
  expiresAt: number
  observedAt: number
  reportedModel: string
  allowed: boolean
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
  routingCookie?: RoutingCookieStatus | null
  cookiePool?: RoutingCookieStatus[]
  cookieOverridePod?: string | null
  cookieOverrideIssuedAt?: number | null
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

export function applyTurnState(data: { accountId: string, model: string, issuedAt: number, observationId?: string }) {
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

export function copyTurnStateCookie(data: { accountId: string, model: string, pod: string }) {
  return request<{ pod: string, name: string, value: string, expiresAt: number }>({
    url: '/api/admin/accounts/turn-state/cookie-copy',
    method: 'POST',
    data,
  })
}

export function applyTurnStateCookie(data: { accountId: string, model: string, pod: string }) {
  return request<{ accountId: string, configRevision: number }>({
    url: '/api/admin/accounts/turn-state/cookie-apply',
    method: 'POST',
    data,
  })
}

export function removeTurnStateCookie(data: { accountId: string, model: string }) {
  return request<{ accountId: string, configRevision: number }>({
    url: '/api/admin/accounts/turn-state/cookie-remove',
    method: 'POST',
    data,
  })
}
