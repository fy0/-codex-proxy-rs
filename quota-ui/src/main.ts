import { createApp } from 'vue'
import App from './App.vue'
import './styles.css'

// 宿主桥会在加载页面前注入并同步主题，保留其自定义 Token
createApp(App).mount('#app')
