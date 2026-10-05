import type {
  AxiosInstance,
  AxiosRequestConfig,
} from 'axios'

import { toast } from '@codex-proxy/ui'
import axios from 'axios'
import { API_BASE_URL, API_TIMEOUT_MS } from './constants'
import { ApiError, normalizeApiError, normalizeApiResponseError } from './error'

export { ApiError } from './error'

export interface RequestOptions {
  // 静默只关闭全局提示，不吞掉异常，也不跳过会话失效处理。
  silent?: boolean
  signal?: AbortSignal
  timeout?: number
}

export type RequestConfig = AxiosRequestConfig & RequestOptions

export interface RawResponseMeta {
  status: number
  header: (name: string) => string | undefined
}

export interface RawResponse {
  status: number
  contentType: string
  body: ArrayBuffer
}

export interface RawResponseOptions {
  maximumBytes: number
  accept: (response: RawResponseMeta) => boolean
}

const http: AxiosInstance = axios.create({
  baseURL: API_BASE_URL,
  timeout: API_TIMEOUT_MS,
  withCredentials: true,
})

// 会话失效是后端业务事实；登录凭据错误和 403 不清除已有会话。
const SESSION_REQUIRED = 40101
let unauthorizedHandled = false
let sessionGeneration = 0
let unauthorizedHandler: (() => void | Promise<void>) | undefined

export function setUnauthorizedHandler(handler: () => void | Promise<void>) {
  unauthorizedHandler = handler
}

export function resetUnauthorizedHandling() {
  sessionGeneration += 1
  unauthorizedHandled = false
}

function handleUnauthorizedOnce() {
  if (unauthorizedHandled || !unauthorizedHandler)
    return
  unauthorizedHandled = true
  void Promise.resolve(unauthorizedHandler()).catch(() => {
    unauthorizedHandled = false
  })
}

function rejectRequest(error: ApiError, config: RequestConfig, generation: number) {
  if (error.kind === 'cancelled' || config.signal?.aborted || generation !== sessionGeneration)
    return Promise.reject(error)

  const sessionExpired = error.status === 401 && error.code === SESSION_REQUIRED
  const alreadyHandled = sessionExpired && unauthorizedHandled
  if (sessionExpired)
    handleUnauthorizedOnce()
  if (!config.silent && !alreadyHandled)
    toast.error(error.message)
  return Promise.reject(error)
}

interface ApiEnvelope {
  code: number
  message: string
  data: unknown
}

function isApiEnvelope(value: unknown): value is ApiEnvelope {
  return (
    typeof value === 'object'
    && value !== null
    && 'data' in value
    && 'code' in value && typeof value.code === 'number'
    && 'message' in value && typeof value.message === 'string'
  )
}

export default async function request<T = unknown>(config: RequestConfig): Promise<T> {
  const generation = sessionGeneration
  try {
    const response = await http.request<unknown>(config)
    const error = normalizeApiResponseError(response)
    if (error)
      throw error
    return (isApiEnvelope(response.data) ? response.data.data : response.data) as T
  }
  catch (error) {
    if (error instanceof ApiError)
      return rejectRequest(error, config, generation)
    if (axios.isAxiosError(error))
      return rejectRequest(normalizeApiError(error), config, generation)
    throw error
  }
}

/**
 * 使用同一管理会话请求原始字节。只有调用方识别的隔离响应才会作为业务正文返回；
 * 其余响应继续按 Admin 错误信封处理，不绕过会话失效与统一错误提示。
 */
export async function requestRaw(
  config: RequestConfig,
  options: RawResponseOptions,
): Promise<RawResponse> {
  const generation = sessionGeneration
  try {
    const response = await http.request<ArrayBuffer>({
      ...config,
      responseType: 'arraybuffer',
      validateStatus: () => true,
    })
    const body = response.data
    const meta: RawResponseMeta = {
      status: response.status,
      header: name => rawHeader(response.headers, name),
    }
    if (options.accept(meta)) {
      if (body.byteLength > options.maximumBytes)
        throw new ApiError('响应内容超过大小限制', 502, undefined, rawHeader(response.headers, 'x-request-id'), 'http')
      return {
        status: response.status,
        contentType: rawHeader(response.headers, 'content-type') ?? 'application/octet-stream',
        body,
      }
    }

    const decoded = decodeRawEnvelope(body)
    const normalized = normalizeApiResponseError({ ...response, data: decoded })
    if (normalized)
      throw normalized
    throw new ApiError(
      response.status >= 400 ? `请求失败（HTTP ${response.status}）` : '服务返回了不受支持的响应',
      response.status,
      undefined,
      rawHeader(response.headers, 'x-request-id'),
      'http',
    )
  }
  catch (error) {
    if (error instanceof ApiError)
      return rejectRequest(error, config, generation)
    if (axios.isAxiosError(error))
      return rejectRequest(normalizeApiError(error), config, generation)
    throw error
  }
}

function rawHeader(headers: unknown, name: string) {
  if (!headers || typeof headers !== 'object')
    return undefined
  const source = headers as { get?: (key: string) => unknown, [key: string]: unknown }
  const value = typeof source.get === 'function' ? source.get(name) : source[name]
  return typeof value === 'string' && value ? value : undefined
}

function decodeRawEnvelope(body: ArrayBuffer): unknown {
  if (body.byteLength === 0)
    return undefined
  try {
    return JSON.parse(new TextDecoder().decode(new Uint8Array(body))) as unknown
  }
  catch {
    return undefined
  }
}
