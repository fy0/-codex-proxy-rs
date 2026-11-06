import { createApp } from 'vue'
import App from './App.vue'
import './styles.css'

// 沿用宿主主题与注入的 Token，缺少皮肤变量时由页面局部默认值保证控件可见
function syncTheme() {
  document.documentElement.dataset.theme = window.codexProxyPlugin?.theme ?? 'light'
}
syncTheme()
window.addEventListener('codex-proxy-themechange', syncTheme)
createApp(App).mount('#app')
