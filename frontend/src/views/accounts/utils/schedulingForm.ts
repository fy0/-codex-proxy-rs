export interface AccountSchedulingValues {
  concurrencyLimit: number | null
  weight: number
}

const MAX_ACCOUNT_CONCURRENCY = 4_294_967_295

type AccountSchedulingParseResult
  = | { valid: true, values: AccountSchedulingValues }
    | { valid: false, message: string }

export function parseAccountSchedulingForm(
  concurrencyLimit: string,
  weight: string,
): AccountSchedulingParseResult {
  const limitText = concurrencyLimit.trim()
  const parsedLimit = limitText === '' ? null : Number(limitText)
  if (
    parsedLimit !== null
    && (!Number.isSafeInteger(parsedLimit)
      || parsedLimit < 1
      || parsedLimit > MAX_ACCOUNT_CONCURRENCY)
  ) {
    return { valid: false, message: `并发限制必须留空，或为 1 到 ${MAX_ACCOUNT_CONCURRENCY} 的整数` }
  }

  const parsedWeight = Number(weight.trim())
  if (!Number.isSafeInteger(parsedWeight) || parsedWeight < 1 || parsedWeight > 100) {
    return { valid: false, message: '权重必须是 1 到 100 的整数' }
  }

  return {
    valid: true,
    values: {
      concurrencyLimit: parsedLimit,
      weight: parsedWeight,
    },
  }
}

export function concurrencyLimitInput(value: number | null) {
  return value === null ? '' : String(value)
}

type OptionalLimitParseResult
  = | { valid: true, value: number | null }
    | { valid: false, message: string }

// 可选并发上限：留空为 null（不限制），否则须为 1 到 u32 上限的整数。
export function parseOptionalConcurrencyLimit(limit: string): OptionalLimitParseResult {
  const text = limit.trim()
  const value = text === '' ? null : Number(text)
  if (
    value !== null
    && (!Number.isSafeInteger(value) || value < 1 || value > MAX_ACCOUNT_CONCURRENCY)
  ) {
    return { valid: false, message: `并发上限必须留空，或为 1 到 ${MAX_ACCOUNT_CONCURRENCY} 的整数` }
  }
  return { valid: true, value }
}
