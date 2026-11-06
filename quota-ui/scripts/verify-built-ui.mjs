import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath, pathToFileURL } from 'node:url'

const directory = path.dirname(fileURLToPath(import.meta.url))
const root = path.resolve(directory, '../..')
const baseline = process.argv.includes('--baseline')
const moduleFlag = process.argv.indexOf('--playwright-module')
const playwrightModule = moduleFlag >= 0
  ? process.argv[moduleFlag + 1]
  : 'playwright'
const { chromium } = await import(playwrightModule.startsWith('.') || path.isAbsolute(playwrightModule)
  ? pathToFileURL(path.resolve(playwrightModule)).href
  : playwrightModule)
const outDirectory = path.resolve(root, 'quota-ui/dist/qa')
await mkdir(outDirectory, { recursive: true })

const now = Date.UTC(2026, 10, 6, 14, 20)
const configuration = {
  maxObservationAgeSeconds: 300,
  accounts: [
    {
      accountId: 'qa-studio',
      fiveHourLimitPercent: 90,
      weeklyLimitPercent: 95,
      dailyLimitUsd: '20',
    },
    {
      accountId: 'qa-creative',
      fiveHourLimitPercent: 90,
      weeklyLimitPercent: 90,
      dailyLimitUsd: '10',
    },
    {
      accountId: 'qa-reserve',
      fiveHourLimitPercent: null,
      weeklyLimitPercent: 95,
      dailyLimitUsd: null,
    },
  ],
}
const accounts = [
  {
    accountId: 'qa-studio',
    name: 'Studio · 主力账号',
    email: 'studio@example.invalid',
    enabled: true,
    dailyUsage: {
      limitUsd: '20',
      usedUsd: '6.25',
      remainingUsd: '13.75',
      status: 'ready',
      unknownCostCount: 0,
      inFlightCount: 1,
      observedAtMs: now,
      resetAtMs: Date.UTC(2026, 10, 7),
      timezone: 'UTC',
      currency: 'USD',
    },
    assessment: { accountId: 'qa-studio', excluded: false, reasons: [] },
    quota: {
      observedAtMs: now,
      windows: [
        { windowSeconds: 18000, usedPercent: 42, resetAtMs: now + 7200000 },
        { windowSeconds: 604800, usedPercent: 67, resetAtMs: now + 172800000 },
      ],
    },
  },
  {
    accountId: 'qa-creative',
    name: '创作团队',
    email: 'creative@example.invalid',
    enabled: true,
    dailyUsage: {
      limitUsd: '10',
      usedUsd: '10.125',
      remainingUsd: '0',
      status: 'reached',
      unknownCostCount: 0,
      inFlightCount: 0,
      observedAtMs: now,
      resetAtMs: Date.UTC(2026, 10, 7),
      timezone: 'UTC',
      currency: 'USD',
    },
    assessment: {
      accountId: 'qa-creative',
      excluded: true,
      reasons: [
        {
          code: 'limit_reached',
          message: '5 小时额度已达到保护阈值',
          windowSeconds: 18000,
          usedPercent: 94,
          limitPercent: 90,
        },
      ],
    },
    quota: {
      observedAtMs: now,
      windows: [
        { windowSeconds: 18000, usedPercent: 94, resetAtMs: now + 3600000 },
        { windowSeconds: 604800, usedPercent: 36, resetAtMs: now + 259200000 },
      ],
    },
  },
  {
    accountId: 'qa-reserve',
    name: '备用账号',
    email: 'reserve@example.invalid',
    enabled: true,
    dailyUsage: {
      limitUsd: null,
      usedUsd: null,
      remainingUsd: null,
      status: 'disabled',
      unknownCostCount: 0,
      inFlightCount: 0,
      observedAtMs: null,
      resetAtMs: Date.UTC(2026, 10, 7),
      timezone: 'UTC',
      currency: 'USD',
    },
    assessment: { accountId: 'qa-reserve', excluded: false, reasons: [] },
    quota: {
      observedAtMs: now,
      windows: [
        { windowSeconds: 18000, usedPercent: 18, resetAtMs: now + 14400000 },
        { windowSeconds: 604800, usedPercent: 22, resetAtMs: now + 345600000 },
      ],
    },
  },
]
const fixture = {
  revision: 7,
  configuration,
  evaluatedAtMs: now,
  accounts,
}

const html = await readFile(path.join(root, 'web/quota/index.html'), 'utf8')
const css = await readFile(path.join(root, 'web/quota/assets/app.css'), 'utf8')
const js = await readFile(path.join(root, 'web/quota/assets/app.js'), 'utf8')
const failures = []
const browser = await chromium.launch({ headless: true })
const results = []

function dataUrl(type, text) {
  return `data:${type};base64,${Buffer.from(text).toString('base64')}`
}

async function openPage(theme, width, options = {}) {
  const page = await browser.newPage({
    viewport: { width, height: 1040 },
    deviceScaleFactor: 1,
  })
  page.setDefaultTimeout(10000)
  page.on('pageerror', error => failures.push(error.message))
  page.on('console', (message) => {
    if (message.type() === 'error')
      failures.push(message.text())
  })
  const responseFixture = structuredClone(fixture)
  if (options.dailyStatus) {
    Object.assign(responseFixture.accounts[0].dailyUsage, {
      status: options.dailyStatus,
      usedUsd: null,
      remainingUsd: null,
      unknownCostCount: options.dailyStatus === 'unknown'
        ? 2
        : 0,
    })
    responseFixture.accounts[0].assessment.excluded = true
  }
  if (options.legacy) {
    for (const rule of responseFixture.configuration.accounts)
      delete rule.dailyLimitUsd
    for (const account of responseFixture.accounts)
      delete account.dailyUsage
  }
  if (options.dailyOnly) {
    Object.assign(responseFixture.configuration.accounts[2], {
      fiveHourLimitPercent: null,
      weeklyLimitPercent: null,
      dailyLimitUsd: '2',
    })
    Object.assign(responseFixture.accounts[2].dailyUsage, {
      limitUsd: '2',
      usedUsd: '0',
      remainingUsd: '2',
      status: 'ready',
      observedAtMs: now,
    })
  }
  const bootstrap = `window.__quotaFixture=${JSON.stringify(responseFixture)};`
    + `window.__quotaCalls=[];window.__quotaOptions=${JSON.stringify(options)};`
    + `for(const [name,value] of Object.entries(window.__quotaOptions.tokens||{})){document.documentElement.style.setProperty(name,value)};`
    + `window.codexProxyPlugin={version:2,theme:${JSON.stringify(theme)},request:async function(input){`
    + `window.__quotaCalls.push(input);let status=200;let result;`
    + `if(input.path==="quota/overview"){`
    + `if(window.__quotaOptions.failOverview){status=502;result={message:"读取额度状态失败，请重试"}}`
    + `else {result=structuredClone(window.__quotaFixture);if(window.__quotaOptions.empty){result.configuration.accounts=[];result.accounts=[]}}}`
    + `else if(input.path==="quota/accounts"){result={items:[...window.__quotaFixture.accounts,{accountId:"qa-added",name:"新建测试账号",email:"added@example.invalid",enabled:true}],nextCursor:null}}`
    + `else if(input.path==="quota/configuration"){if(window.__quotaOptions.conflict){status=409;result={message:"配置已发生变化，请刷新后重试"}}`
    + `else{window.__quotaSaved=JSON.parse(input.body);result={saved:true,reloadRequired:true}}}`
    + `else{status=404;result={message:"未知测试路由"}}`
    + `return {status:status,contentType:"application/json",body:new TextEncoder().encode(JSON.stringify(result)).buffer}}};`
  const csp = 'default-src \'none\'; script-src \'unsafe-inline\' data:; style-src \'unsafe-inline\' data:; img-src data:; font-src data:; connect-src \'none\'; base-uri \'none\'; form-action \'none\''
  const document = html
    .replace('<html lang="zh-CN">', `<html lang="zh-CN" data-theme="${theme}">`)
    .replace('<head>', `<head><meta http-equiv="Content-Security-Policy" content="${csp}"><script>${bootstrap}</script>`)
    .replace('./assets/app.css', dataUrl('text/css', css))
    .replace('./assets/app.js', dataUrl('text/javascript', js))
  const shell = `<!doctype html><html><head><meta charset="utf-8"><style>`
    + `body{margin:0;background:${theme === 'dark'
      ? '#10151d'
      : '#f4f6f9'};font-family:Segoe UI,sans-serif;padding:24px}`
      + `header{display:flex;justify-content:space-between;align-items:center;margin:0 0 20px;color:${theme === 'dark'
        ? '#e4eaf3'
        : '#172336'}}`
        + `h1{font-size:22px;margin:0;font-weight:650}p{margin:6px 0 0;font-size:13px;opacity:.65}`
        + `iframe{width:100%;height:1500px;border:0;display:block}span{font-size:12px;opacity:.6}`
        + `</style></head><body><header><div><h1>账号额度保护</h1><p>为每个账号保留可用额度</p></div><span>CPR · 插件管理</span></header>`
        + `<iframe title="Quota Guard" sandbox="allow-scripts"></iframe></body></html>`
  await page.setContent(shell)
  await page.locator('iframe').evaluate((element, value) => {
    element.srcdoc = value
  }, document)
  const frame = page.frameLocator('iframe')
  await frame.locator('main').waitFor({ state: 'visible' })
  if (!options.failOverview && !options.empty)
    await frame.getByText('Studio · 主力账号', { exact: true }).first().waitFor()
  await page.waitForTimeout(250)
  await resizeFrame(page, frame)
  return { page, frame }
}

async function resizeFrame(page, frame) {
  const height = await frame.locator('main').evaluate(element => Math.ceil(element.getBoundingClientRect().bottom))
  await page.locator('iframe').evaluate((element, value) => {
    element.style.height = `${Math.max(860, value + 8)}px`
  }, height)
}

try {
  for (const scenario of [
    { theme: 'light', width: 1360 },
    { theme: 'dark', width: 1360 },
    { theme: 'light', width: 420 },
    {
      theme: 'light',
      width: 1360,
      options: {
        tokens: { '--cp-color-bg-container': 'rgb(250, 249, 255)' },
      },
    },
  ]) {
    const { page, frame } = await openPage(scenario.theme, scenario.width, scenario.options)
    const metrics = await frame.locator('main').evaluate((main) => {
      const card = main.querySelector('[data-testid="account-card"]') || main.querySelector('article')
      const button = [
        ...main.querySelectorAll('button'),
      ].find(element => element.textContent.includes('保存配置'))
      const style = card
        ? getComputedStyle(card)
        : null
      const buttonStyle = button
        ? getComputedStyle(button)
        : null
      return {
        cardBackground: style?.backgroundColor,
        cardRadius: style?.borderRadius,
        cardPadding: style?.padding,
        buttonBackground: buttonStyle?.backgroundColor,
        buttonHeight: button?.getBoundingClientRect().height,
        viewportWidth: document.documentElement.clientWidth,
        documentWidth: document.documentElement.scrollWidth,
      }
    })
    if (!baseline) {
      assert.ok(metrics.cardBackground && metrics.cardBackground !== 'rgba(0, 0, 0, 0)', 'Account cards need a visible surface with no injected theme tokens')
      assert.ok(Number.parseFloat(metrics.cardRadius) >= 8, 'Account cards need their fallback radius')
      assert.ok(metrics.buttonHeight >= 32, 'Save controls must remain usable without host tokens')
      assert.ok(metrics.documentWidth <= metrics.viewportWidth + 2, 'Page must not overflow horizontally')
    }
    if (!baseline && scenario.options?.tokens)
      assert.equal(metrics.cardBackground, 'rgb(250, 249, 255)', 'Injected host theme tokens must stay authoritative')
    const name = `${baseline ? 'before' : 'after'}-${scenario.theme}-${scenario.width}${scenario.options?.tokens ? '-host-theme' : ''}`
    await page.screenshot({
      path: path.join(outDirectory, `${name}.png`),
      fullPage: true,
    })
    results.push({ scenario: name, metrics })
    await page.close()
  }
  if (!baseline) {
    const { page, frame } = await openPage('light', 1360)
    await frame.getByRole('searchbox', { name: '搜索保护配置' }).fill('创作')
    assert.equal(await frame.locator('[data-testid="account-card"]').count(), 1)
    await frame.getByRole('searchbox', { name: '搜索保护配置' }).fill('')
    await frame.getByRole('button', { name: '添加账号', exact: true }).click()
    await frame.getByRole('button', { name: '添加 新建测试账号', exact: true }).click()
    assert.equal(await frame.locator('[data-testid="account-card"]').count(), 4)
    assert.equal(await frame.getByRole('switch', { name: '新建测试账号 每日费用保护', exact: true }).getAttribute('aria-checked'), 'false', 'New accounts leave daily cost protection disabled')
    await frame.getByRole('button', { name: '关闭账号选择', exact: true }).click()
    const limit = frame.getByRole('spinbutton', { name: 'Studio · 主力账号 5 小时使用率上限', exact: true })
    await limit.fill('88')
    await limit.blur()
    const guardSwitch = frame.getByRole('switch', { name: 'Studio · 主力账号 5 小时保护', exact: true })
    await guardSwitch.click()
    await guardSwitch.click()
    assert.equal(await limit.inputValue(), '88', 'Toggling protection preserves the edited threshold')
    const dailyInput = frame.getByRole('textbox', { name: 'Studio · 主力账号 每日费用上限 USD', exact: true })
    const saveButton = frame.getByRole('button', { name: '保存配置', exact: true })
    for (const invalid of [
      '',
      '-1',
      '1e3',
      ' 1',
      '12.',
      '1234567890123456789',
      '1.1234567890123',
    ]) {
      await dailyInput.fill(invalid)
      assert.equal(await dailyInput.getAttribute('aria-invalid'), 'true', `Invalid amount must be marked: ${invalid}`)
      assert.equal(await saveButton.isDisabled(), true, 'Invalid daily limits cannot be saved')
    }
    assert.equal(await frame.locator('main').evaluate(() => window.__quotaCalls.filter(call => call.path === 'quota/configuration').length), 0)
    await dailyInput.fill('999999999999999999.999999999999')
    assert.equal(await saveButton.isDisabled(), false, 'Maximum fixed-point amount is valid without a float conversion')
    await dailyInput.fill('12.500000000001')
    const dailySwitch = frame.getByRole('switch', { name: 'Studio · 主力账号 每日费用保护', exact: true })
    await dailySwitch.click()
    await dailySwitch.click()
    assert.equal(await dailyInput.inputValue(), '12.500000000001', 'Toggling daily protection preserves every decimal digit')
    await frame.getByRole('button', { name: '保存配置', exact: true }).click()
    await frame.getByText(/配置已保存/).first().waitFor()
    const saved = await frame.locator('main').evaluate(() => window.__quotaSaved)
    assert.equal(saved.expectedRevision, 7)
    assert.equal(saved.configuration.accounts.find(account => account.accountId === 'qa-studio').fiveHourLimitPercent, 88)
    assert.equal(saved.configuration.accounts.find(account => account.accountId === 'qa-studio').dailyLimitUsd, '12.500000000001')
    assert.equal(saved.configuration.accounts.find(account => account.accountId === 'qa-added').dailyLimitUsd, null)
    assert.equal(saved.configuration.accounts.length, 4)
    assert.equal(saved.configuration.maxObservationAgeSeconds, 300)
    await resizeFrame(page, frame)
    await page.screenshot({
      path: path.join(outDirectory, 'saved.png'),
      fullPage: true,
    })
    results.push({ scenario: 'save-search-add', passed: true })
    await page.close()

    const zeroPage = await openPage('light', 1360)
    await zeroPage.frame.getByRole('switch', { name: '备用账号 每日费用保护', exact: true }).click()
    const zeroInput = zeroPage.frame.getByRole('textbox', { name: '备用账号 每日费用上限 USD', exact: true })
    assert.equal(await zeroInput.inputValue(), '', 'Enabling a new limit must not silently choose a budget')
    assert.equal(await zeroPage.frame.getByRole('button', { name: '保存配置', exact: true }).isDisabled(), true)
    await zeroInput.fill('0')
    await zeroPage.frame.getByText('0 将立即暂停该账号分配', { exact: true }).waitFor()
    await zeroPage.frame.getByRole('button', { name: '保存配置', exact: true }).click()
    await zeroPage.frame.getByText(/配置已保存/).first().waitFor()
    assert.equal(await zeroPage.frame.locator('main').evaluate(() => window.__quotaSaved.configuration.accounts.find(account => account.accountId === 'qa-reserve').dailyLimitUsd), '0')
    results.push({ scenario: 'daily-default-off-zero-blocks', passed: true })
    await zeroPage.page.close()

    for (const dailyStatus of ['unknown', 'stale']) {
      const costPage = await openPage('light', 1360, { dailyStatus })
      const window = costPage.frame.getByRole('region', { name: 'Studio · 主力账号 每日费用', exact: true })
      await window.getByText(dailyStatus === 'unknown'
        ? '费用待确认'
        : '记录已过期', { exact: true }).waitFor()
      assert.equal(await window.locator('.qg-daily-usage strong').textContent(), '—', 'Unknown cost must not be shown as zero')
      assert.equal(await window.getByRole('progressbar').getAttribute('aria-valuenow'), null, 'Unknown or stale cost must not advertise a measured progress value')
      await costPage.page.screenshot({
        path: path.join(outDirectory, `daily-${dailyStatus}.png`),
        fullPage: true,
      })
      results.push({ scenario: `daily-${dailyStatus}`, passed: true })
      await costPage.page.close()
    }

    const legacyPage = await openPage('light', 1360, { legacy: true })
    for (const switchControl of await legacyPage.frame.getByRole('switch', { name: /每日费用保护$/ }).all())
      assert.equal(await switchControl.getAttribute('aria-checked'), 'false')
    assert.equal(await legacyPage.frame.getByText('未保存', { exact: true }).count(), 0, 'Legacy missing fields must not create a false dirty state')
    assert.equal(await legacyPage.frame.getByRole('button', { name: '保存配置', exact: true }).isDisabled(), true)
    results.push({ scenario: 'legacy-daily-fields-optional', passed: true })
    await legacyPage.page.close()

    const dailyOnlyPage = await openPage('light', 1360, { dailyOnly: true })
    assert.match(await dailyOnlyPage.frame.locator('.qg-stat strong').first().textContent(), /^3/)
    await dailyOnlyPage.frame.getByRole('button', { name: '未保护', exact: true }).click()
    assert.equal(await dailyOnlyPage.frame.locator('[data-testid="account-card"]').count(), 0, 'A daily-only rule is still a protected account')
    results.push({ scenario: 'daily-only-account-counts', passed: true })
    await dailyOnlyPage.page.close()

    const conflictPage = await openPage('light', 1360, { conflict: true })
    const conflictLimit = conflictPage.frame.getByRole('spinbutton', { name: 'Studio · 主力账号 5 小时使用率上限', exact: true })
    await conflictLimit.fill('87')
    await conflictLimit.blur()
    await conflictPage.frame.getByRole('button', { name: '保存配置', exact: true }).click()
    await conflictPage.frame.getByText(/配置已被其他操作修改/).waitFor()
    assert.equal(await conflictPage.frame.getByRole('button', { name: '保存配置', exact: true }).isDisabled(), true)
    results.push({ scenario: 'revision-conflict', passed: true })
    await conflictPage.page.close()

    const emptyPage = await openPage('light', 420, { empty: true })
    await emptyPage.frame.getByText(/还没有配置保护账号/).waitFor()
    results.push({ scenario: 'empty-state', passed: true })
    await emptyPage.page.close()

    const errorPage = await openPage('light', 420, { failOverview: true })
    await errorPage.frame.getByRole('button', { name: '重试', exact: true }).waitFor()
    results.push({ scenario: 'error-state', passed: true })
    await errorPage.page.close()
    assert.deepEqual(failures, [], 'Built sandbox page must not emit JavaScript or CSP errors')
  }
}
finally {
  await browser.close()
  await writeFile(path.join(outDirectory, baseline ? 'before-report.json' : 'after-report.json'), JSON.stringify({ baseline, results, failures }, null, 2))
}
console.log(JSON.stringify({
  baseline,
  results,
  failures,
  outputDirectory: outDirectory,
}, null, 2))
