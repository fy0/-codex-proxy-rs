import type { Ref } from 'vue'
import { toast } from '@codex-proxy/ui'
import { ApiError } from '@/api/request'
import { errorMessage } from '@/utils/operation'

export type PluginActionContext = ReturnType<typeof usePluginActions>

export function usePluginActions(refresh: (silent?: boolean, suppressErrors?: boolean) => Promise<void>) {
  function notifyError(title: string, error: unknown) {
    if (error instanceof ApiError && error.kind === 'cancelled')
      return
    toast.error(errorMessage(error, title))
  }

  async function runAction<T>(
    flag: Ref<boolean>,
    title: string,
    task: () => Promise<T>,
  ): Promise<T | undefined> {
    if (flag.value)
      return undefined
    flag.value = true
    try {
      return await task()
    }
    catch (error) {
      notifyError(title, error)
      return undefined
    }
    finally {
      flag.value = false
    }
  }

  return { refresh, notifyError, runAction }
}
