import { readFileSync } from 'node:fs'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig({
  base: './',
  resolve: { conditions: ['codex-proxy-source', 'module', 'browser', 'import', 'default'] },
  plugins: [
    vue(),
    tailwindcss(),
    {
      name: 'quota-guard-management-page',
      generateBundle() {
        this.emitFile({
          type: 'asset',
          fileName: 'index.html',
          source: readFileSync(new URL('./index.html', import.meta.url), 'utf8'),
        })
      },
    },
  ],
  define: { 'process.env.NODE_ENV': '"production"' },
  build: {
    outDir: '../web/quota',
    emptyOutDir: true,
    // 宿主隔离页只接受 classic script，库模式将 Vue 与组件合并为单个 IIFE
    lib: {
      entry: 'src/main.ts',
      name: 'QuotaGuard',
      formats: ['iife'],
      fileName: () => 'assets/app.js',
    },
    cssCodeSplit: false,
    minify: true,
    rolldownOptions: {
      output: {
        assetFileNames: 'assets/app[extname]',
      },
    },
  },
})
