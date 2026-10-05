import type { Ref, ShallowRef } from 'vue'
import type { getApiKeys } from '@/api'
import { computed, shallowRef } from 'vue'

import { buildCodexCcSwitchImportDeeplink, resolveServiceRootUrl } from '@/utils/client'

// “使用密钥”弹窗展示明文时，在列表行上补挂 reveal 得到的完整 key。
type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number] & { key?: string }

// 密钥使用与 CCSwitch 导入编排：服务根地址推导、deeplink 跳转、
// “使用密钥”弹窗的明文补全与打开。
export function useApiKeyUse(options: {
  createdKey: Readonly<Ref<string>>
  createdKeyName: Readonly<Ref<string>>
  revealPlaintextKey: (apiKey: ApiKeyRow) => Promise<string | undefined>
}) {
  const showUseKeyModal = shallowRef(false)
  const selectedUseKey: ShallowRef<ApiKeyRow | null> = shallowRef(null)

  const serviceRootUrl = computed(() => resolveServiceRootUrl())
  const openAiBaseUrl = computed(() => `${serviceRootUrl.value}/v1`)

  function importCreatedKeyToCcs() {
    if (!options.createdKey.value)
      return

    window.location.href = buildCodexCcSwitchImportDeeplink({
      apiKey: options.createdKey.value,
      baseUrl: openAiBaseUrl.value,
      providerName: options.createdKeyName.value || 'codex-proxy-rs',
    })
  }

  async function openUseKeyModal(apiKey: ApiKeyRow) {
    const key = await options.revealPlaintextKey(apiKey)
    if (!key)
      return
    selectedUseKey.value = { ...apiKey, key }
    showUseKeyModal.value = true
  }

  async function importToCcs(apiKey: ApiKeyRow) {
    const key = await options.revealPlaintextKey(apiKey)
    if (!key)
      return
    window.location.href = buildCodexCcSwitchImportDeeplink({
      apiKey: key,
      baseUrl: openAiBaseUrl.value,
      providerName: apiKey.name || apiKey.prefix || 'codex-proxy-rs',
    })
  }

  return {
    showUseKeyModal,
    selectedUseKey,
    serviceRootUrl,
    openAiBaseUrl,
    importCreatedKeyToCcs,
    openUseKeyModal,
    importToCcs,
  }
}
