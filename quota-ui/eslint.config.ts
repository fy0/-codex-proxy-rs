import antfu from '@antfu/eslint-config'

export default antfu({
  type: 'app',
  ignores: ['dist/**', '**/pnpm-lock.yaml', '**/pnpm-workspace.yaml'],
  formatters: { css: true, html: true },
  stylistic: { indent: 2, quotes: 'single' },
  typescript: true,
  vue: { a11y: true },
})
