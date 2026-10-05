import type { PluginManagementView } from '@/api'

import { acceptHMRUpdate, defineStore } from 'pinia'
import { computed, shallowRef, watch } from 'vue'
import { getPluginManagementViews } from '@/api'
import { ApiError } from '@/api/request'
import { useAuthStore } from '@/stores/modules/auth'
import { errorMessage } from '@/utils/operation'

export const usePluginViewsStore = defineStore('plugin-views', () => {
  const authStore = useAuthStore()
  const views = shallowRef<PluginManagementView[]>([])
  const loading = shallowRef(false)
  const loaded = shallowRef(false)
  const loadError = shallowRef('')
  const pageViews = computed(() => views.value.filter(view => view.pages.length > 0))

  let controller: AbortController | undefined
  let pending: Promise<PluginManagementView[]> | undefined

  function reset() {
    controller?.abort()
    controller = undefined
    pending = undefined
    views.value = []
    loading.value = false
    loaded.value = false
    loadError.value = ''
  }

  function refresh(silent = true) {
    controller?.abort()
    const current = new AbortController()
    controller = current
    loading.value = true
    loadError.value = ''

    const request = getPluginManagementViews({ signal: current.signal, silent })
      .then((items) => {
        if (controller === current) {
          views.value = items
          loaded.value = true
        }
        return items
      })
      .catch((error: unknown) => {
        if (controller === current && error instanceof ApiError && (error.status === 401 || error.status === 403)) {
          views.value = []
          loaded.value = false
        }
        if (controller === current && !(error instanceof ApiError && error.kind === 'cancelled'))
          loadError.value = errorMessage(error)
        throw error
      })
      .finally(() => {
        if (controller === current) {
          controller = undefined
          pending = undefined
          loading.value = false
        }
      })

    pending = request
    return request
  }

  function ensureLoaded() {
    if (loaded.value)
      return Promise.resolve(views.value)
    return pending ?? refresh()
  }

  watch(() => authStore.session, (session, previous) => {
    if (session !== previous)
      reset()
  })

  return {
    views,
    pageViews,
    loading,
    loaded,
    loadError,
    refresh,
    ensureLoaded,
    reset,
  }
})

if (import.meta.hot)
  import.meta.hot.accept(acceptHMRUpdate(usePluginViewsStore, import.meta.hot))
