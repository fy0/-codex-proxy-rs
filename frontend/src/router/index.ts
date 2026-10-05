import type { AuthSession } from '@/api'
import { createRouter, createWebHistory } from 'vue-router'

import { useAuthStore } from '@/stores/modules/auth'
import { routes } from './routes'

export const router = createRouter({
  history: createWebHistory('/'),
  routes,
})

function resolveRoleEntry(role: AuthSession['role']) {
  const entry = router.getRoutes().find(route => route.meta.role === role && route.meta.defaultEntry)
  if (!entry)
    throw new Error(`未配置 ${role} 身份的默认入口`)
  return { path: entry.path }
}

router.beforeEach(async (to) => {
  const authStore = useAuthStore()

  // 登录页不依赖会话恢复；只使用当前已知身份决定是否跳转。
  if (to.meta.guestOnly) {
    if (authStore.session)
      return resolveRoleEntry(authStore.session.role)
    return
  }

  const requiredRole = to.meta.role
  if (!requiredRole)
    return

  const login = {
    name: 'login',
    query: { redirect: to.fullPath },
    state: { loginMode: requiredRole },
  }

  if (!authStore.sessionChecked) {
    try {
      await authStore.checkAuth()
    }
    catch {
      // 暂时无法确认会话时不进入受保护页面，也不缓存成“已退出”。
      return login
    }
  }

  if (!authStore.session)
    return login

  // 两种身份各自进入独立页面，Key 不挂载会请求管理接口的布局。
  if (authStore.session.role !== requiredRole)
    return resolveRoleEntry(authStore.session.role)
})
