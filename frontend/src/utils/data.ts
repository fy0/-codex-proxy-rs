import type { RequestLocation } from '@/api'

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function parseJsonObject(text: string, maximumBytes?: number): Record<string, unknown> {
  let value: unknown
  try {
    value = JSON.parse(text)
  }
  catch {
    throw new Error('请输入有效的 JSON 对象')
  }
  if (!isRecord(value))
    throw new Error('输入必须是 JSON 对象')
  if (maximumBytes !== undefined && new TextEncoder().encode(JSON.stringify(value)).byteLength > maximumBytes)
    throw new Error(`输入不能超过 ${maximumBytes / 1024} KiB`)
  return value
}

export function jsonObjectError(text: string, maximumBytes?: number) {
  try {
    parseJsonObject(text, maximumBytes)
    return undefined
  }
  catch (error) {
    return (error as Error).message
  }
}

export function normalizeRequestLocation(location: RequestLocation): RequestLocation {
  return {
    country: location.country.trim().toUpperCase(),
    region: location.region.trim(),
    city: location.city.trim(),
    timezone: location.timezone.trim(),
  }
}

export function requestLocationError(location: RequestLocation): string {
  if (!/^[A-Z]{2}$/.test(location.country) || !location.region || !location.city || !location.timezone)
    return '请填写两位国家代码、地区、城市和 IANA 时区'
  return ''
}
