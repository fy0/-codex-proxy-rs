import type { Ref } from 'vue'
import type { PluginActionContext } from './usePluginActions'
import type { CreatePluginSourceCredentialRequest, PluginSourceCredential } from '@/api'
import { toast } from '@codex-proxy/ui'
import { shallowRef } from 'vue'
import { createPluginSourceCredential, deletePluginSourceCredential } from '@/api'

interface CredentialContext extends PluginActionContext {
  credentials: Ref<PluginSourceCredential[]>
}

export function usePluginCredentials({ credentials, refresh, notifyError, runAction }: CredentialContext) {
  const savingCredential = shallowRef(false)
  const showCredentialDelete = shallowRef(false)
  const pendingCredential = shallowRef<PluginSourceCredential | null>(null)
  const busyDistributionId = shallowRef('')

  async function saveCredential(request: CreatePluginSourceCredentialRequest) {
    const result = await runAction(savingCredential, '下载认证保存失败', () => createPluginSourceCredential(request, { silent: true }))
    if (!result)
      return null
    credentials.value = [...credentials.value, result]
    return result
  }

  function requestCredentialDelete(credential: PluginSourceCredential) {
    pendingCredential.value = credential
    showCredentialDelete.value = true
  }

  async function confirmCredentialDelete() {
    const credential = pendingCredential.value
    if (!credential || busyDistributionId.value)
      return
    busyDistributionId.value = credential.id
    try {
      await deletePluginSourceCredential({ id: credential.id }, { silent: true })
      showCredentialDelete.value = false
      toast.success('来源凭据已删除')
      await refresh(true)
    }
    catch (error) {
      notifyError('来源凭据删除失败', error)
    }
    finally {
      busyDistributionId.value = ''
    }
  }

  return { savingCredential, showCredentialDelete, pendingCredential, busyDistributionId, saveCredential, requestCredentialDelete, confirmCredentialDelete }
}
