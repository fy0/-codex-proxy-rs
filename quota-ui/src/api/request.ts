interface ManagementRequest {
  method: 'GET' | 'POST'
  path: string
  query?: string
  contentType?: string
  body?: string
}

interface ManagementReply {
  status: number
  contentType: string
  body: ArrayBuffer
}

declare global {
  interface Window {
    readonly codexProxyPlugin?: {
      readonly version: number
      readonly theme: 'light' | 'dark'
      request: (input: ManagementRequest) => Promise<ManagementReply>
    }
  }
}

export class RequestError extends Error {
  constructor(message: string, readonly status?: number) {
    super(message)
    this.name = 'RequestError'
  }
}

export async function request<T>(input: ManagementRequest): Promise<T> {
  const bridge = window.codexProxyPlugin
  if (!bridge || bridge.version !== 2)
    throw new RequestError('请从 Codex Proxy RS 的插件页面打开')

  let reply: ManagementReply
  try {
    reply = await bridge.request(input)
  }
  catch {
    throw new RequestError('页面连接已失效或请求超时，请使用宿主页面右上角的刷新按钮')
  }

  let body: unknown
  try {
    body = JSON.parse(new TextDecoder().decode(reply.body))
  }
  catch {
    throw new RequestError('插件返回的数据无法解析', reply.status)
  }
  if (reply.status < 200 || reply.status >= 300) {
    const message = body && typeof body === 'object' && 'message' in body && typeof body.message === 'string'
      ? body.message
      : '插件请求未完成'
    throw new RequestError(message, reply.status)
  }
  return body as T
}
