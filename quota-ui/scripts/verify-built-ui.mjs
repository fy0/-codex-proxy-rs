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
  dailyTimezone: 'UTC',
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
      ledgerUsedUsd: '6.25',
      windowStartMs: Date.UTC(2026, 10, 6),
      windowEndMs: Date.UTC(2026, 10, 7),
      complete: true,
      knownCostCount: 7,
      ledgerKnownCostCount: 7,
      incompleteReasons: [],
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
      usedUsd: options.knownCost ?? null,
      remainingUsd: null,
      complete: false,
      unknownCostCount: options.dailyStatus === 'unknown'
        ? 2
        : 0,
    })
    responseFixture.accounts[0].assessment = {
      accountId: 'qa-studio',
      excluded: options.dailyStatus === 'reached',
      reasons: [{
        code: options.dailyStatus === 'reached' ? 'daily_limit_reached' : 'daily_usage_unknown',
        message: options.dailyStatus === 'reached' ? '今日已知费用达到日限额，暂停分配' : '费用观测尚未完整，仅提示，不因此拦截',
      }],
    }
  }
  if (options.expiredWindow) {
    responseFixture.accounts[0].quota.windows[0] = { windowSeconds: 18000, usedPercent: 100, resetAtMs: now - 1000 }
    responseFixture.accounts[0].assessment = {
      accountId: 'qa-studio',
      excluded: false,
      reasons: [{ code: 'window_reset_pending', message: '旧额度窗口已重置，等待更新，不因此拦截' }],
    }
  }
  if (options.mixedWindows)
    responseFixture.accounts[0].quota.windows.unshift({ windowSeconds: 18000, usedPercent: 100, resetAtMs: now - 1000 })
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
  if (options.windowTimezone || options.calibrated) {
    const localStart = Date.UTC(2026, 10, 1, 4)
    const localEnd = Date.UTC(2026, 10, 2, 5)
    responseFixture.configuration.dailyTimezone = 'America/Toronto'
    responseFixture.configuration.accounts[0].dailyLimitUsd = '150'
    Object.assign(responseFixture.accounts[0].dailyUsage, {
      timezone: 'America/Toronto', windowStartMs: localStart, windowEndMs: localEnd,
      resetAtMs: localEnd, limitUsd: '150', usedUsd: '50.89', remainingUsd: null,
      ledgerUsedUsd: '50.89', knownCostCount: 7, ledgerKnownCostCount: 7,
      unknownCostCount: 6, ledgerUnknownCostCount: 6, ledgerInFlightCount: 0,
      inFlightCount: 0, status: 'unknown', complete: false,
      incompleteReasons: ['unpriced_records'],
    })
    if (options.calibrated) {
      const calibration = {
        amountUsd: '134.13', atMs: Date.UTC(2026, 10, 1, 12),
        windowStartMs: localStart, windowEndMs: localEnd, timezone: 'America/Toronto',
      }
      responseFixture.configuration.accounts[0].dailyCalibration = calibration
      Object.assign(responseFixture.accounts[0].dailyUsage, {
        usedUsd: '136.38', remainingUsd: '13.62', ledgerUsedUsd: '53.14',
        postCalibrationUsedUsd: '2.25', calibration: { ...calibration, active: true },
        knownCostCount: 1, ledgerKnownCostCount: 8, unknownCostCount: 0,
        status: 'ready', complete: true, incompleteReasons: [],
      })
    }
  }
  if (options.savedAlias) {
    const calibration = {
      amountUsd: '6.25', atMs: now - 1000, timezone: 'Etc/UTC',
      windowStartMs: Date.UTC(2026, 10, 6), windowEndMs: Date.UTC(2026, 10, 7),
    }
    responseFixture.configuration.dailyTimezone = 'Etc/UTC'
    responseFixture.configuration.accounts[0].dailyCalibration = calibration
    Object.assign(responseFixture.accounts[0].dailyUsage, {
      timezone: 'Etc/UTC', calibration: { ...calibration, active: true }, postCalibrationUsedUsd: '0',
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
    + `else if(input.path==="quota/calibration"){if(window.__quotaOptions.calibrationConflict){status=409;result={message:"配置已发生变化，请刷新后重试"}}else{window.__quotaCalibrated=JSON.parse(input.body);result={saved:true,reloadRequired:true}}}`
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
    assert.equal(saved.configuration.dailyTimezone, 'UTC')
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
        ? '记录未齐'
        : '待刷新', { exact: true }).waitFor()
      assert.equal(await window.locator('.qg-daily-usage strong').textContent(), '—', 'Unknown cost must not be shown as zero')
      assert.equal(await window.getByRole('progressbar').getAttribute('aria-valuenow'), null, 'Unknown or stale cost must not advertise a measured progress value')
      const card = costPage.frame.locator('[data-testid="account-card"]').first()
      assert.equal(await card.locator('.qg-account-blocked').count(), 0)
      assert.equal(await card.evaluate(element => element.classList.contains('qg-account-blocked')), false, 'Unknown or stale daily costs do not exclude the account')
      await card.getByText('未拦截', { exact: true }).waitFor()
      assert.equal(await card.locator('.qg-account-reasons-info').count(), 1, 'Observation notices use informational styling')
      assert.equal(await window.locator('.qg-badge-warning').count(), 0)
      await costPage.page.screenshot({
        path: path.join(outDirectory, `daily-${dailyStatus}.png`),
        fullPage: true,
      })
      results.push({ scenario: `daily-${dailyStatus}`, passed: true })
      await costPage.page.close()
    }

    for (const [dailyStatus, knownCost] of [['unknown', '6.25'], ['stale', '6.25'], ['reached', '20.01']]) {
      const partialPage = await openPage('light', 1360, { dailyStatus, knownCost })
      const card = partialPage.frame.locator('[data-testid="account-card"]').first()
      const dailyWindow = card.getByRole('region', { name: 'Studio · 主力账号 每日费用', exact: true })
      assert.equal(await dailyWindow.locator('.qg-daily-usage strong').textContent(), `$${knownCost}`, 'Unpriced records must not erase known fees')
      assert.equal(await dailyWindow.getByRole('progressbar').getAttribute('aria-valuenow'), dailyStatus === 'reached' ? '100' : '31.25')
      assert.equal(await card.evaluate(element => element.classList.contains('qg-account-blocked')), dailyStatus === 'reached', 'Only confirmed exceedance blocks with partial cost data')
      await card.getByText(dailyStatus === 'reached' ? '已拦截' : '未拦截', { exact: true }).waitFor()
      results.push({ scenario: `daily-partial-${dailyStatus}`, passed: true })
      await partialPage.page.close()
    }

    const resetPage = await openPage('light', 1360, { expiredWindow: true })
    const resetCard = resetPage.frame.locator('[data-testid="account-card"]').first()
    const resetWindow = resetCard.getByRole('region', { name: 'Studio · 主力账号 5 小时额度', exact: true })
    await resetWindow.getByText('上周期', { exact: true }).waitFor()
    await resetWindow.getByText('已重置，等待新观测；旧窗口不拦截', { exact: true }).waitFor()
    assert.equal(await resetWindow.evaluate(element => element.classList.contains('qg-window-warning')), false, 'Expired 100% usage is historical and cannot look like an active block')
    await resetCard.getByText('未拦截', { exact: true }).waitFor()
    results.push({ scenario: 'reset-window-allows', passed: true })
    await resetPage.page.close()

    const mixedPage = await openPage('light', 1360, { mixedWindows: true })
    const currentWindow = mixedPage.frame.getByRole('region', { name: 'Studio · 主力账号 5 小时额度', exact: true })
    await currentWindow.getByText('42%', { exact: true }).waitFor()
    await currentWindow.getByText('已使用', { exact: true }).waitFor()
    results.push({ scenario: 'current-window-preferred-over-expired-peak', passed: true })
    await mixedPage.page.close()

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

    const timezonePage = await openPage('light', 1360)
    const timezoneInput = timezonePage.frame.getByRole('combobox', { name: '每日记账时区', exact: true })
    assert.equal(await timezoneInput.inputValue(), 'UTC')
    for (const invalidZone of ['Not/A_Zone', '+08:00', '-0500', '+00:00']) {
      await timezoneInput.fill(invalidZone)
      await timezoneInput.blur()
      assert.equal(await timezoneInput.getAttribute('aria-invalid'), 'true', 'Numeric offsets are not named IANA zones: ' + invalidZone)
      assert.equal(await timezonePage.frame.getByRole('button', { name: '保存配置', exact: true }).isDisabled(), true)
    }
    assert.equal(await timezonePage.frame.locator('main').evaluate(() => window.__quotaCalls.filter(call => call.path === 'quota/configuration').length), 0)
    await timezoneInput.fill('america/toronto')
    await timezoneInput.blur()
    assert.equal(await timezoneInput.inputValue(), 'America/Toronto', 'A changed named zone is canonicalized on change')
    await timezonePage.frame.getByText('保存后重新划分本日窗口，并清除原时区的手工校准；历史账本保持不变。', { exact: true }).waitFor()
    const timezoneDaily = timezonePage.frame.getByRole('region', { name: 'Studio · 主力账号 每日费用', exact: true })
    await timezoneDaily.locator('summary').click()
    assert.equal(await timezoneDaily.getByRole('textbox', { name: 'Studio · 主力账号 校准本日已用 USD', exact: true }).isDisabled(), true, 'Unsaved timezone edits cannot be calibrated against the old window')
    await timezonePage.frame.getByRole('button', { name: '保存配置', exact: true }).click()
    await timezonePage.frame.getByText(/配置已保存/).first().waitFor()
    const timezoneSaved = await timezonePage.frame.locator('main').evaluate(() => window.__quotaSaved)
    assert.equal(timezoneSaved.configuration.dailyTimezone, 'America/Toronto')
    assert.equal(timezoneSaved.configuration.accounts[0].weeklyLimitPercent, 95)
    results.push({ scenario: 'iana-timezone-validation-save-and-dirty-calibration-lock', passed: true })
    await timezonePage.page.close()

    const saveNormalizedPage = await openPage('light', 1360)
    await saveNormalizedPage.frame.getByRole('combobox', { name: '每日记账时区', exact: true }).fill('asia/shanghai')
    // Trigger save without moving focus, so no input change event performs normalization first.
    await saveNormalizedPage.frame.getByRole('button', { name: '保存配置', exact: true }).evaluate(button => button.click())
    await saveNormalizedPage.frame.getByText(/配置已保存/).first().waitFor()
    assert.equal(await saveNormalizedPage.frame.locator('main').evaluate(() => window.__quotaSaved.configuration.dailyTimezone), 'Asia/Shanghai')
    results.push({ scenario: 'named-timezone-canonicalized-again-at-save', passed: true })
    await saveNormalizedPage.page.close()

    const aliasPage = await openPage('light', 1360, { savedAlias: true })
    const aliasInput = aliasPage.frame.getByRole('combobox', { name: '每日记账时区', exact: true })
    assert.equal(await aliasInput.inputValue(), 'Etc/UTC')
    await aliasInput.dispatchEvent('change')
    assert.equal(await aliasInput.inputValue(), 'Etc/UTC', 'An unchanged saved alias must not be canonicalized')
    const aliasLimit = aliasPage.frame.getByRole('textbox', { name: 'Studio · 主力账号 每日费用上限 USD', exact: true })
    await aliasLimit.fill('30')
    await aliasPage.frame.getByRole('button', { name: '保存配置', exact: true }).click()
    await aliasPage.frame.getByText(/配置已保存/).first().waitFor()
    const aliasState = await aliasPage.frame.locator('main').evaluate(() => ({ saved: window.__quotaSaved, calibration: window.__quotaFixture.configuration.accounts[0].dailyCalibration }))
    assert.equal(aliasState.saved.configuration.dailyTimezone, 'Etc/UTC', 'Editing a threshold must not silently switch saved timezone aliases and clear calibration')
    assert.deepEqual(aliasState.saved.configuration.accounts[0].dailyCalibration, aliasState.calibration)
    results.push({ scenario: 'unchanged-timezone-alias-and-calibration-preserved-on-threshold-save', passed: true })
    await aliasPage.page.close()

    const calibrationPage = await openPage('light', 1360, { windowTimezone: true })
    const calibrationWindow = calibrationPage.frame.getByRole('region', { name: 'Studio · 主力账号 每日费用', exact: true })
    await calibrationWindow.getByText('有未计价请求', { exact: true }).waitFor()
    assert.equal(await calibrationWindow.locator('.qg-daily-usage strong').textContent(), '$50.89')
    assert.match(await calibrationWindow.locator('.qg-daily-range').textContent(), /11\/01 00:00.*11\/02 00:00.*America\/Toronto/)
    await calibrationWindow.locator('summary').click()
    await calibrationWindow.getByText('存在尚未计价的成功请求', { exact: true }).waitFor()
    const amountInput = calibrationWindow.getByRole('textbox', { name: 'Studio · 主力账号 校准本日已用 USD', exact: true })
    const amountApply = calibrationWindow.getByRole('button', { name: '设为当前已用', exact: true })
    assert.equal(await amountInput.inputValue(), '', 'Calibration must require the user to enter a verified amount')
    assert.equal(await amountApply.isDisabled(), true)
    for (const invalid of ['-1', '1e3', ' 134.13', '134.', '1.1234567890123']) {
      await amountInput.fill(invalid)
      assert.equal(await amountInput.getAttribute('aria-invalid'), 'true')
      assert.equal(await amountApply.isDisabled(), true)
    }
    await amountInput.fill('134.130000000001')
    await resizeFrame(calibrationPage.page, calibrationPage.frame)
    await calibrationPage.page.screenshot({ path: path.join(outDirectory, 'daily-timezone-calibration.png'), fullPage: true })
    await amountApply.click()
    await calibrationPage.frame.getByText(/配置已保存/).first().waitFor()
    const calibratedSubmission = await calibrationPage.frame.locator('main').evaluate(() => window.__quotaCalibrated)
    assert.deepEqual(calibratedSubmission, { expectedRevision: 7, accountId: 'qa-studio', amountUsd: '134.130000000001' }, 'Calibration sends exact decimal amount and CAS revision; the server owns all timestamps')
    results.push({ scenario: 'local-day-window-unpriced-reason-and-exact-calibration', passed: true })
    await calibrationPage.page.close()

    const calibratedPage = await openPage('dark', 1360, { calibrated: true })
    const calibratedWindow = calibratedPage.frame.getByRole('region', { name: 'Studio · 主力账号 每日费用', exact: true })
    await calibratedWindow.getByText('已校准', { exact: true }).waitFor()
    assert.equal(await calibratedWindow.locator('.qg-daily-usage strong').textContent(), '$136.38')
    await calibratedWindow.locator('summary').click()
    await calibratedWindow.getByText('$134.13', { exact: true }).waitFor()
    await calibratedWindow.getByText('$2.25', { exact: true }).waitFor()
    await calibratedWindow.getByText('$53.14', { exact: true }).waitFor()
    await calibratedWindow.getByText('原账本未计价记录', { exact: true }).waitFor()
    assert.equal(await calibratedWindow.locator('.qg-ledger-reason').count(), 0, 'Pre-calibration unpriced ledger records cannot make the effective interval incomplete')
    await resizeFrame(calibratedPage.page, calibratedPage.frame)
    await calibratedPage.page.screenshot({ path: path.join(outDirectory, 'daily-calibrated-dark.png'), fullPage: true })
    await calibratedWindow.getByRole('button', { name: '恢复自动记账', exact: true }).click()
    await calibratedPage.frame.getByText(/配置已保存/).first().waitFor()
    assert.deepEqual(await calibratedPage.frame.locator('main').evaluate(() => window.__quotaCalibrated), { expectedRevision: 7, accountId: 'qa-studio', amountUsd: null })
    results.push({ scenario: 'calibrated-baseline-plus-new-fees-and-clear', passed: true })
    await calibratedPage.page.close()

    const calibrationConflictPage = await openPage('light', 420, { windowTimezone: true, calibrationConflict: true })
    const conflictDaily = calibrationConflictPage.frame.getByRole('region', { name: 'Studio · 主力账号 每日费用', exact: true })
    await conflictDaily.locator('summary').click()
    await conflictDaily.getByRole('textbox', { name: 'Studio · 主力账号 校准本日已用 USD', exact: true }).fill('1')
    await conflictDaily.getByRole('button', { name: '设为当前已用', exact: true }).click()
    await calibrationConflictPage.frame.getByText(/配置已被其他操作修改/).waitFor()
    assert.equal(await conflictDaily.getByRole('button', { name: '设为当前已用', exact: true }).isDisabled(), true)
    const mobileWidth = await calibrationConflictPage.frame.locator('main').evaluate(() => ({ width: document.documentElement.scrollWidth, viewport: document.documentElement.clientWidth }))
    assert.ok(mobileWidth.width <= mobileWidth.viewport + 2, 'Expanded calibration controls must fit on mobile')
    results.push({ scenario: 'calibration-revision-conflict-mobile', passed: true })
    await calibrationConflictPage.page.close()

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
