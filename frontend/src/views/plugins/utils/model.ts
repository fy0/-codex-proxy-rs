import type {
  PluginArtifact,
  PluginArtifactMetadata,
  PluginInstance,
  PluginObserverEvent,
  PluginSource,
  PluginUpdateSource,
  VerifyRemotePluginRequest,
} from '@/api'

export type PluginInstallSelection = File | VerifyRemotePluginRequest

export function pluginInstallSelectionKey(selection: PluginInstallSelection) {
  return selection instanceof File ? selection : JSON.stringify(selection)
}

export interface JsonSchema {
  type?: string | string[]
  title?: string
  description?: string
  default?: unknown
  enum?: unknown[]
  properties?: Record<string, JsonSchema>
  required?: string[]
  minimum?: number
  maximum?: number
  minLength?: number
  maxLength?: number
  pattern?: string
  items?: JsonSchema
  additionalProperties?: boolean | JsonSchema
}

export const PLUGIN_OBSERVER_EVENT_LABELS: Record<PluginObserverEvent, string> = {
  request_completed: '请求完成与用量',
  websocket_response: '上游 WebSocket 事件',
}

export const PLUGIN_CAPABILITY_LABELS: Record<string, string> = {
  frontend_authentication: '客户端认证',
  scheduler: '请求调度',
  model_router: '模型路由',
  model_catalog: '模型目录',
  retry_policy: '重试策略',
  middleware: '请求中间件',
  upstream_adapter: '上游适配器',
  observer: '事件观察',
  command_line: '命令行',
  management: '管理扩展',
  maintenance: '维护任务',
}

// 编辑入口、范围控件与摘要共用阶段描述；能力和阶段的合法组合由宿主校验。
export const PLUGIN_REQUEST_STAGES: Record<string, {
  label: string
  scope: 'none' | 'model' | 'provider'
  globalLabel?: string
}> = {
  http: { label: 'HTTP 请求', scope: 'none', globalLabel: '所有 HTTP 请求' },
  websocket: { label: 'WebSocket 消息', scope: 'none', globalLabel: '所有 WebSocket 消息' },
  service: { label: '服务调用', scope: 'none', globalLabel: '所有服务调用' },
  request: { label: '请求开始', scope: 'model' },
  attempt: { label: '每次尝试', scope: 'provider' },
  upstream: { label: '上游调用', scope: 'provider' },
  routing: { label: '模型路由', scope: 'model' },
  scheduling: { label: '账号调度', scope: 'provider' },
  retry: { label: '重试决策', scope: 'provider' },
  observation: { label: '事件观察', scope: 'provider' },
}

export function pluginRequestBindingEntries(metadata: PluginArtifactMetadata) {
  return pluginContributionEntries(metadata).flatMap(([capability, contribution]) =>
    contribution.stages.flatMap((stage) => {
      const description = PLUGIN_REQUEST_STAGES[stage]
      if (!description)
        return []
      const events: { event?: PluginObserverEvent, label: string }[] = capability === 'observer'
        ? Object.entries(PLUGIN_OBSERVER_EVENT_LABELS).map(([event, label]) => ({ event: event as PluginObserverEvent, label }))
        : [{ label: description.label }]
      return events.map(({ event, label }) => ({ ...description, capability, contribution: contribution.id, stage, event, label }))
    }),
  )
}

export function pluginCapabilityLabel(capability: string) {
  return PLUGIN_CAPABILITY_LABELS[capability] ?? capability
}

export function pluginContributionEntries(metadata: PluginArtifactMetadata) {
  return Object.entries(metadata.contributes)
}

export function normalizePluginRepository(value: string) {
  // GitHub 下载端使用小写仓库路径，凭据的路径范围必须采用相同规范。
  return value.trim().replace(/^https:\/\/github\.com\//i, '').replace(/\/$/, '').replace(/\.git$/i, '').toLowerCase()
}

export function formatPluginFileSize(bytes: number) {
  if (bytes < 1024)
    return `${bytes} B`
  if (bytes < 1024 * 1024)
    return `${(bytes / 1024).toFixed(1)} KiB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MiB`
}

export function pluginContributionForCapability(
  metadata: PluginArtifactMetadata,
  capability: string,
) {
  return metadata.contributes[capability]
}

export function pluginCapabilityForContribution(
  metadata: PluginArtifactMetadata,
  contributionId: string,
) {
  return pluginContributionEntries(metadata).find(([, contribution]) =>
    contribution.id === contributionId,
  )?.[0]
}

export function sourceLabel(source: PluginSource | PluginUpdateSource) {
  switch (source.kind) {
    case 'builtin':
      return '随版本提供'
    case 'upload':
      return '本地上传'
    case 'url':
      return '固定 URL'
    case 'github':
      return 'GitHub Release'
  }
}

export function sourceDetail(source: PluginSource | PluginUpdateSource) {
  switch (source.kind) {
    case 'builtin':
      return 'release' in source ? source.release : '发行清单'
    case 'upload':
      return '无远程更新来源'
    case 'url':
      return source.url
    case 'github':
      return 'tag' in source ? `${source.repository}@${source.tag}` : source.repository
  }
}

export function shortDigest(digest: string) {
  return digest.length > 15 ? `${digest.slice(0, 8)}…${digest.slice(-6)}` : digest
}

export function artifactForInstance(instance: PluginInstance, artifacts: PluginArtifact[]) {
  return artifacts.find(artifact => artifact.metadata.sha256 === instance.artifactSha256)
}

export function cloneJsonValue<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}
