import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath, pathToFileURL } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const moduleIndex = process.argv.indexOf('--playwright-module')
const moduleName = moduleIndex >= 0 ? process.argv[moduleIndex + 1] : 'playwright'
const { chromium } = await import(path.isAbsolute(moduleName) ? pathToFileURL(moduleName).href : moduleName)
const output = path.join(root, 'dist/debug-ui-qa')
await mkdir(output, { recursive: true })
const source = Object.fromEntries(await Promise.all(['debug.html', 'debug.css', 'debug.js', 'image-client.js'].map(async name =>
  [name, await readFile(path.join(root, 'web', name), 'utf8')])))
const browser = await chromium.launch({ headless: true })
const failures = []
const results = []
const dataUrl = (type, value) => 'data:' + type + ';base64,' + Buffer.from(value).toString('base64')
let fixtures

function installBridge(config) {
  window.__debugFixture = config.fixtures
  window.__debugScenario = config.scenario || 'large'
  window.__bridgeCalls = []
  window.__managementCalls = []
  window.__catalogs = {
    'key-fixture': { models: ['gpt-6-luna'], imageModels: ['gpt-image-2.5-sunburst'] },
    'key-other': { models: ['gpt-6-sol'], imageModels: ['gpt-image-2.6-flare'] },
    'key-empty': { models: [], imageModels: [] },
    'key-text': { models: ['gpt-6-astra'], imageModels: [] },
  }
  window.__failedCatalogs = []
  window.__heldCatalogs = []
  window.__pendingCatalogs = []
  window.__bridgeAborts = 0
  window.__readerCancels = 0
  window.__wire = ''
  const fixture = window.__debugFixture
  const responseBody = (items, status = 'completed') => ({ id: 'resp_fixture', object: 'response', status, output: items })
  const item = (id, result) => ({ type: 'image_generation_call', id, status: 'completed', result })
  function imageStream() {
    const image = { ...item('img-large', fixture.large), model: 'gpt-image-2-codex', size: '768x512', quality: 'medium', output_format: 'png' }
    const reportedTool = { type: 'image_generation', model: 'gpt-image-2-codex', size: 'auto', quality: 'auto', output_format: 'png', provider_only: 'preserved-in-raw-export' }
    return ': heartbeat\r\n\r\n'
      + 'event: vendor.custom\r\ndata: {"marker":"鲸🐋",\r\ndata: "unknown":{"nested":["kept",0,false]}}\r\n\r\n'
      + 'data: ' + JSON.stringify({ type: 'response.created', response: { id: 'resp_fixture', model: 'gpt-6-luna', status: 'in_progress', tools: [reportedTool] } }) + '\r\n\r\n'
      + 'data: ' + JSON.stringify({ type: 'response.output_item.done', item: image }) + '\r\n\r\n'
      + 'data: ' + JSON.stringify({ type: 'response.completed', response: responseBody([image]) }) + '\r\n\r\n'
      + 'data: [DONE]\r\n\r\n'
  }
  function byteStream(wire, waiting) {
    const bytes = new TextEncoder().encode(wire)
    let offset = 0
    let emptyAfterCR = false
    return new ReadableStream({
      pull(controller) {
        if (emptyAfterCR) {
          emptyAfterCR = false
          controller.enqueue(new Uint8Array(0))
          return
        }
        if (offset >= bytes.length) {
          if (waiting) return new Promise(() => {})
          controller.close()
          return
        }
        const length = offset < 512 ? 1 : 64 * 1024
        const part = bytes.slice(offset, Math.min(offset + length, bytes.length))
        offset += part.length
        emptyAfterCR = part.length === 1 && part[0] === 13
        controller.enqueue(part)
      },
      cancel() { window.__readerCancels++ },
    }, { highWaterMark: 0 })
  }
  window.codexProxyPlugin = {
    version: 2,
    theme: config.theme,
    plugin: { name: 'cpr-image-bridge' },
    page: { id: 'image-debug' },
    request: async (request) => {
      const input = request.body ? JSON.parse(request.body) : null
      window.__managementCalls.push({ method: request.method, path: request.path, clientKeyId: input?.clientKeyId })
      let body
      if (request.path === 'api/bootstrap') {
        body = {
          keys: Object.keys(window.__catalogs).map(id => ({ id, name: '调试 Key', enabled: true })),
          driverModel: 'gpt-5.6-luna',
          imageModels: ['gpt-image-2.5-sunburst', 'gpt-image-2.6-sunburst'],
          pluginVersion: '0.7.6-fixture',
          executionMode: 'management-responses',
          backendJobsEnabled: false,
        }
      } else if (request.path === 'api/models') {
        if (window.__heldCatalogs.includes(input.clientKeyId))
          await new Promise(resolve => window.__pendingCatalogs.push({ key: input.clientKeyId, resolve }))
        if (window.__failedCatalogs.includes(input.clientKeyId))
          return { status: 503, body: new TextEncoder().encode(JSON.stringify({ error: { message: 'catalog unavailable' } })).buffer }
        body = window.__catalogs[input.clientKeyId] || { models: [], imageModels: [] }
      } else throw new Error('Unexpected management route: ' + request.path)
      return { status: 200, contentType: 'application/json', body: new TextEncoder().encode(JSON.stringify(body)).buffer }
    },
    models: {
      responses: async (request) => {
        window.__bridgeCalls.push({ clientKeyId: request.clientKeyId, bodyText: JSON.stringify(request.body) })
        request.signal.addEventListener('abort', () => { window.__bridgeAborts++ }, { once: true })
        const mode = window.__debugScenario
        if (mode === 'wait-headers') {
          return new Promise((resolve, reject) => request.signal.addEventListener('abort', () => reject(request.signal.reason), { once: true }))
        }
        let wire = '', type = 'text/event-stream', status = 200
        if (mode === 'catalog-routing') {
          type = 'application/json'
          const catalog = window.__catalogs[request.clientKeyId]
          if (!catalog?.models.includes(request.body.model)) {
            status = 503
            wire = JSON.stringify({ error: { code: 'no_available_provider', message: 'No upstream provider is available for this request.' } })
          } else wire = JSON.stringify(responseBody([item('image-routed', fixture.small)]))
        } else if (mode === 'large') wire = imageStream()
        else if (mode === 'wait-stream') wire = 'data: {"type":"response.created","response":{"id":"resp_wait","status":"in_progress"}}\r\n\r\n'
        else if (mode === 'failed') wire = 'data: ' + JSON.stringify({
          type: 'response.failed',
          response: { id: 'resp_failed', status: 'failed', error: { code: 'capacity', message: 'No account slot', details: { kind: 'account_capacity_unavailable' } } },
        }) + '\n\n'
        else if (mode === 'incomplete') wire = 'data: ' + JSON.stringify({
          type: 'response.incomplete',
          response: { id: 'resp_incomplete', status: 'incomplete', incomplete_details: { reason: 'max_output_tokens' } },
        }) + '\n\n'
        else if (mode === 'http-error') {
          status = 503
          type = 'application/json'
          wire = JSON.stringify({ error: { code: 'capacity', message: 'Bearer PRIVATEVALUE sk-abcdefghijklmnopqrstuv', details: { kind: 'concurrency_queue_timeout', authorization: 'Bearer PRIVATEVALUE' } } })
        } else if (mode === 'bad-json') {
          type = 'application/json'
          wire = '{"unfinished":'
        } else if (mode === 'missing-status') {
          type = 'application/json'
          wire = JSON.stringify({ id: 'resp_no_status', output: [] })
        } else if (mode === 'multi') {
          type = 'application/json'
          wire = JSON.stringify(responseBody([item('a', fixture.small), item('a', fixture.small), item('b', fixture.small)]))
        } else if (mode === 'json-jpeg' || mode === 'json-webp' || mode === 'json-png') {
          type = 'application/json'
          const key = mode === 'json-jpeg' ? 'jpeg' : mode === 'json-webp' ? 'webp' : 'small'
          wire = JSON.stringify(responseBody([item('image-json', fixture[key])]))
        } else throw new Error('Unknown fixture mode')
        window.__wire = wire
        return new Response(byteStream(wire, mode === 'wait-stream'), {
          status,
          headers: {
            'content-type': type, 'x-request-id': 'req_fixture', authorization: 'Bearer PRIVATEVALUE',
            'x-api-key': 'opaque-key-abc123', 'x-auth-token': 'opaque-auth-abc123', 'x-access-token': 'opaque-access-abc123',
          },
        })
      },
    },
  }
}

async function waitForCatalog(frame) {
  for (let attempt = 0; attempt < 100; attempt++) {
    const ready = await frame.locator('#debugModels').evaluate(element => !element.disabled)
    if (ready) return
    await new Promise(resolve => setTimeout(resolve, 20))
  }
  throw new Error('Selected Key catalog did not finish loading')
}

async function openPage(theme = 'light', width = 1360, scenario = 'large') {
  const page = await browser.newPage({ viewport: { width, height: 1000 }, deviceScaleFactor: 1 })
  page.setDefaultTimeout(20000)
  page.on('pageerror', error => failures.push(error.message))
  page.on('console', message => { if (message.type() === 'error') failures.push(message.text()) })
  const csp = "default-src 'none'; script-src 'unsafe-inline' data:; style-src 'unsafe-inline' data:; img-src data:; font-src data:; media-src data:; connect-src 'none'; worker-src 'none'; child-src 'none'; frame-src 'none'; object-src 'none'; manifest-src 'none'; base-uri 'none'; form-action 'none'"
  const bootstrap = '(' + installBridge.toString() + ')(' + JSON.stringify({ theme, scenario, fixtures }) + ');'
  const html = source['debug.html']
    .replace('<head>', '<head><meta http-equiv="Content-Security-Policy" content="' + csp + '"><script>' + bootstrap + '</script>')
    .replace('./debug.css', dataUrl('text/css', source['debug.css']))
    .replace('./image-client.js', dataUrl('text/javascript', source['image-client.js']))
    .replace('./debug.js', dataUrl('text/javascript', source['debug.js']))
  await page.setContent('<!doctype html><html><body style="margin:0"><iframe title="Responses 调试" sandbox="allow-scripts" style="display:block;width:100%;height:2200px;border:0"></iframe></body></html>')
  await page.locator('iframe').evaluate((element, value) => { element.srcdoc = value }, html)
  const frame = page.frameLocator('iframe')
  await frame.locator('#debugRun').waitFor()
  await frame.locator('#debugBody').waitFor()
  await waitForCatalog(frame)
  await frame.locator('#debugBody').evaluate(element => {
    if (!element.value) throw new Error('Template was not loaded')
  })
  return { page, frame }
}

async function resize({ page, frame }) {
  const size = await frame.locator('main').evaluate(main => ({
    height: Math.ceil(main.getBoundingClientRect().bottom + 12),
    viewportWidth: document.documentElement.clientWidth,
    documentWidth: document.documentElement.scrollWidth,
  }))
  await page.locator('iframe').evaluate((element, height) => { element.style.height = height + 'px' }, size.height)
  assert.ok(size.documentWidth <= size.viewportWidth + 2, 'Debug page must not overflow horizontally')
  return size
}

async function direct(frame, scenario, body = { model: 'test', stream: false, extra: { keep: [false, 0, 'raw'] } }, cancel = false) {
  return frame.locator('main').evaluate(async (main, options) => {
    window.__debugScenario = options.scenario
    const before = window.__bridgeCalls.length
    const originalBody = JSON.stringify(options.body)
    const events = []
    const controller = new AbortController()
    if (options.cancel) setTimeout(() => controller.abort(), 30)
    const result = await window.CPRImageClient.run({
      clientKeyId: 'key-fixture', body: options.body, signal: controller.signal, captureRaw: true,
      onEvent(event) { events.push({ type: event.type, event: event.type === 'vendor.custom' ? event.event : null, rawLength: event.raw.length }) },
    })
    return {
      status: result.status, httpStatus: result.httpStatus, error: result.error,
      requestId: result.requestId, responseId: result.responseId, timings: result.timings,
      images: result.images.map(image => ({ itemId: image.itemId, mimeType: image.mimeType, width: image.width, height: image.height })),
      events, headers: result.headers, warnings: result.warnings,
      calls: window.__bridgeCalls.length - before, sent: window.__bridgeCalls.at(-1)?.bodyText,
      aborts: window.__bridgeAborts, cancels: window.__readerCancels,
      capture: { bytes: result.capture.bytes, storedBytes: result.capture.storedBytes, complete: result.capture.complete, truncated: result.capture.truncated },
      rawMatches: result.capture.chunks.join('') === window.__wire,
      sourceUntouched: JSON.stringify(options.body) === originalBody,
    }
  }, { scenario, body, cancel })
}

try {
  const generator = await browser.newPage()
  fixtures = await generator.evaluate(() => {
    function canvasImage(width, height, noisy) {
      const canvas = document.createElement('canvas')
      canvas.width = width
      canvas.height = height
      const context = canvas.getContext('2d')
      const image = context.createImageData(width, height)
      let seed = 2718281
      for (let i = 0; i < image.data.length; i += 4) {
        seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5
        const x = (i / 4) % width
        const y = Math.floor(i / 4 / width)
        image.data[i] = 40 + x / width * 130 + (noisy ? seed & 63 : 0)
        image.data[i + 1] = 70 + y / height * 100 + (noisy ? (seed >>> 8) & 63 : 0)
        image.data[i + 2] = 130 + (noisy ? (seed >>> 16) & 63 : 0)
        image.data[i + 3] = 255
      }
      context.putImageData(image, 0, 0)
      return canvas
    }
    const large = canvasImage(768, 512, true).toDataURL('image/png').split(',')[1]
    const small = canvasImage(32, 24, false)
    return {
      large,
      small: small.toDataURL('image/png').split(',')[1],
      jpeg: small.toDataURL('image/jpeg', .9).split(',')[1],
      webp: small.toDataURL('image/webp', .9).split(',')[1],
    }
  })
  await generator.close()
  assert.ok(fixtures.large.length > 1024 * 1024, 'Fixture must exceed the SDK event budget in a single event')
  results.push({ scenario: 'large-fixture', base64Characters: fixtures.large.length })

  const routed = await openPage('light', 1360, 'catalog-routing')
  const readTemplate = async () => JSON.parse(await routed.frame.locator('#debugBody').inputValue())
  assert.equal((await readTemplate()).model, 'gpt-6-luna', 'Initial template must use the selected Key catalog, not the stale bootstrap default')
  assert.equal((await readTemplate()).tools[0].model, 'gpt-image-2.5-sunburst')
  assert.deepEqual(await routed.frame.locator('main').evaluate(() => window.__managementCalls.filter(call => call.path === 'api/models').map(call => call.clientKeyId)), ['key-fixture'], 'Opening debug must only load the selected Key catalog')
  await routed.frame.locator('#debugRun').click()
  await routed.frame.locator('#debugRunState').getByText('完整结束', { exact: true }).waitFor()
  await routed.frame.locator('#debugKey').selectOption('key-other')
  await waitForCatalog(routed.frame)
  assert.equal((await readTemplate()).model, 'gpt-6-sol')
  assert.equal((await readTemplate()).tools[0].model, 'gpt-image-2.6-flare', 'Template image model must also belong to the selected Key')
  const manual = JSON.stringify({ model: 'manual-experiment', input: '保留原文', vendor_option: { untouched: [0, false] } }, null, 2)
  await routed.frame.locator('#debugBody').fill(manual)
  await routed.frame.locator('#debugKey').selectOption('key-fixture')
  await waitForCatalog(routed.frame)
  assert.equal(await routed.frame.locator('#debugBody').inputValue(), manual, 'Key switches must preserve manually edited raw JSON')
  await routed.frame.locator('#debugRun').click()
  await routed.frame.locator('#debugRunState').getByText('失败', { exact: true }).waitFor()
  assert.ok((await routed.frame.locator('#debugErrorDetail').textContent()).includes('no_available_provider'))
  assert.ok((await routed.frame.locator('#debugNotice').textContent()).includes('主控模型未出现在'))
  assert.equal(await routed.frame.locator('#debugBody').inputValue(), manual, 'A failed raw request must not rewrite the submitted model')
  const rejected = await routed.frame.locator('main').evaluate(() => window.__bridgeCalls)
  assert.equal(rejected.length, 2, 'No hidden retries or fallback models')
  assert.equal(rejected[1].bodyText, JSON.stringify(JSON.parse(manual)))
  await routed.frame.locator('#debugReset').click()
  assert.equal((await readTemplate()).model, 'gpt-6-luna')
  await routed.frame.locator('#debugRun').click()
  await routed.frame.locator('#debugRunState').getByText('完整结束', { exact: true }).waitFor()
  results.push({ scenario: 'selected-key-catalog-routing-and-explicit-raw-failure', passed: true })

  await routed.frame.locator('#debugKey').selectOption('key-empty')
  await waitForCatalog(routed.frame)
  assert.equal(await routed.frame.locator('#debugBody').inputValue(), '', 'An empty catalog must not fabricate a model')
  assert.equal(await routed.frame.locator('#debugRun').isDisabled(), true)
  await routed.frame.locator('#debugBody').fill(manual)
  assert.equal(await routed.frame.locator('#debugRun').isDisabled(), false, 'A raw debugger must still permit explicit experiments outside the catalog')
  await routed.frame.locator('#debugKey').selectOption('key-text')
  await waitForCatalog(routed.frame)
  await routed.frame.locator('#debugTemplate').selectOption('text')
  await routed.frame.locator('#debugReset').click()
  assert.equal((await readTemplate()).model, 'gpt-6-astra')
  assert.equal((await readTemplate()).tools, undefined)
  await routed.frame.locator('main').evaluate(() => { window.__failedCatalogs = ['key-other'] })
  await routed.frame.locator('#debugKey').selectOption('key-other')
  await waitForCatalog(routed.frame)
  assert.equal(await routed.frame.locator('#debugBody').inputValue(), '', 'Catalog errors must not restore the obsolete bootstrap model')
  assert.ok((await routed.frame.locator('#debugNotice').textContent()).includes('catalog unavailable'))
  await routed.frame.locator('#debugBody').fill(manual)
  await routed.frame.locator('#debugModels').click()
  await waitForCatalog(routed.frame)
  assert.equal(await routed.frame.locator('#debugBody').inputValue(), manual, 'Catalog failures must not erase manual JSON')
  results.push({ scenario: 'empty-error-and-text-only-catalogs', passed: true })

  await routed.frame.locator('main').evaluate(() => { window.__failedCatalogs = []; window.__heldCatalogs = ['key-other'] })
  await routed.frame.locator('#debugKey').selectOption('key-fixture')
  await waitForCatalog(routed.frame)
  await routed.frame.locator('#debugReset').click()
  await routed.frame.locator('#debugKey').selectOption('key-other')
  await routed.frame.locator('#debugKey').selectOption('key-fixture')
  await waitForCatalog(routed.frame)
  await routed.frame.locator('main').evaluate(() => {
    if (!window.__pendingCatalogs.length) throw new Error('Stale catalog fixture was not suspended')
    window.__heldCatalogs = []
    for (const pending of window.__pendingCatalogs.splice(0)) pending.resolve()
  })
  assert.equal((await readTemplate()).model, 'gpt-6-luna', 'Late previous-Key catalog must not overwrite the current template')
  results.push({ scenario: 'stale-key-catalog-response', passed: true })
  await routed.page.close()

  const large = await openPage()
  const result = await direct(large.frame, 'large')
  assert.equal(result.status, 'completed')
  assert.equal(result.images.length, 1, 'Repeated image item IDs must not become duplicate output images')
  assert.deepEqual(result.images[0], { itemId: 'img-large', mimeType: 'image/png', width: 768, height: 512 })
  assert.ok(result.events.some(event => event.rawLength > 1024 * 1024))
  assert.deepEqual(result.events.find(event => event.type === 'vendor.custom').event, { marker: '鲸🐋', unknown: { nested: ['kept', 0, false] } })
  assert.equal(result.calls, 1)
  assert.equal(result.sourceUntouched, true, 'The client must not mutate the submitted JSON object')
  assert.equal(result.sent, JSON.stringify({ model: 'test', stream: false, extra: { keep: [false, 0, 'raw'] } }))
  assert.equal(result.rawMatches, true, 'Capture must preserve original CRLF and UTF-8 response text')
  assert.equal(result.capture.bytes, result.capture.storedBytes)
  assert.equal(result.capture.complete, true)
  assert.equal(result.capture.truncated, false)
  assert.equal(result.requestId, 'req_fixture')
  assert.equal(result.responseId, 'resp_fixture')
  results.push({ scenario: 'large-sse-utf8-crlf-empty-chunks-unknown-event-and-dedup', ...result, sent: undefined, headers: undefined, events: result.events.map(event => ({ type: event.type, rawLength: event.rawLength })) })

  for (const [scenario, mime] of [['json-png', 'image/png'], ['json-jpeg', 'image/jpeg'], ['json-webp', 'image/webp']]) {
    const current = await direct(large.frame, scenario)
    assert.equal(current.status, 'completed')
    assert.equal(current.images[0].mimeType, mime)
    assert.equal(current.images[0].width, 32)
    assert.equal(current.images[0].height, 24)
    results.push({ scenario, passed: true })
  }

  const multiple = await direct(large.frame, 'multi')
  assert.deepEqual(multiple.images.map(image => image.itemId), ['a', 'b'])
  results.push({ scenario: 'different-ids-same-image-remain-multiple', passed: true })

  const failed = await direct(large.frame, 'failed')
  assert.equal(failed.httpStatus, 200)
  assert.equal(failed.status, 'failed')
  assert.equal(failed.error.code, 'capacity')
  assert.equal(failed.error.upstreamKind, 'account_capacity_unavailable')
  results.push({ scenario: 'http-200-response-failed', passed: true })

  const httpError = await direct(large.frame, 'http-error')
  assert.equal(httpError.status, 'failed')
  assert.equal(httpError.httpStatus, 503)
  assert.equal(httpError.error.kind, 'http')
  assert.equal(httpError.error.details.kind, 'concurrency_queue_timeout')
  assert.ok(!JSON.stringify(httpError.error).includes('PRIVATEVALUE'))
  assert.ok(!JSON.stringify(httpError.headers).includes('PRIVATEVALUE'))
  for (const name of ['x-api-key', 'x-auth-token', 'x-access-token'])
    assert.equal(httpError.headers.find(header => header[0] === name)?.[1], '[已脱敏]', 'Credential header must be redacted regardless of its value format')
  results.push({ scenario: 'http-capacity-details-and-credential-redaction', passed: true })

  const incomplete = await direct(large.frame, 'incomplete')
  assert.equal(incomplete.status, 'incomplete')
  assert.equal(incomplete.error.details.reason, 'max_output_tokens')
  const missing = await direct(large.frame, 'missing-status')
  assert.equal(missing.status, 'incomplete')
  assert.equal(missing.error.code, 'missing_completion')
  const badJson = await direct(large.frame, 'bad-json')
  assert.equal(badJson.status, 'failed')
  assert.equal(badJson.error.kind, 'parse')
  assert.equal(badJson.capture.complete, true, 'A full transport capture stays full even if its JSON is malformed')
  results.push({ scenario: 'incomplete-missing-status-and-fully-captured-malformed-json', passed: true })

  for (const name of ['Authorization', 'x-api-key', 'x-auth-token', 'x-access-token']) {
    const credential = await direct(large.frame, 'json-png', { model: 'x', headers: { [name]: 'opaque-credential-value' } })
    assert.equal(credential.status, 'failed')
    assert.equal(credential.error.code, 'credential_field')
    assert.equal(credential.calls, 0, 'Credential injection must be rejected before the management bridge')
  }
  const redactedText = await large.frame.locator('main').evaluate(() => window.CPRImageClient.redactText(JSON.stringify({
    'x-api-key': 'opaque-key-abc123', 'x-auth-token': 'opaque-auth-abc123', 'x-access-token': 'opaque-access-abc123',
  })))
  assert.ok(!redactedText.includes('opaque-'))
  results.push({ scenario: 'credential-headers-and-body-redacted-without-value-prefix', passed: true })

  for (const scenario of ['wait-headers', 'wait-stream']) {
    const canceled = await direct(large.frame, scenario, { model: 'x', stream: true }, true)
    assert.equal(canceled.status, 'canceled')
    assert.equal(canceled.calls, 1)
    assert.ok(canceled.aborts >= 1)
    if (scenario === 'wait-stream') assert.ok(canceled.cancels >= 1)
    assert.equal(canceled.capture.complete, false)
    results.push({ scenario: 'cancel-' + scenario, passed: true })
  }

  await large.frame.locator('main').evaluate(() => { window.__debugScenario = 'large' })
  const edited = { model: 'gpt-5.6-luna', stream: true, store: false, input: '鲸🐋', tools: [{ type: 'image_generation', model: 'gpt-image-2.5-sunburst', size: '2160x3840', quality: 'high', output_format: 'webp' }], future_option: { unchanged: ['raw', 0, false] } }
  await large.frame.locator('#debugBody').fill(JSON.stringify(edited, null, 2))
  await large.frame.locator('#debugRefresh').click()
  assert.equal(await large.frame.locator('#debugBody').inputValue(), JSON.stringify(edited, null, 2), 'Refreshing keys must not reset edited JSON')
  const beforeUiRun = await large.frame.locator('main').evaluate(() => window.__bridgeCalls.length)
  await large.frame.getByRole('button', { name: '运行一次', exact: true }).click()
  await large.frame.locator('#debugRunState').getByText('完整结束', { exact: true }).waitFor()
  assert.equal(await large.frame.locator('#debugImages img').count(), 1)
  assert.ok((await large.frame.locator('#debugEvents').textContent()).includes('预览截断'))
  assert.ok(await large.frame.locator('[data-event-type="vendor.custom"]').count())
  const snapshot = JSON.parse(await large.frame.locator('#debugRequestSnapshot').textContent())
  assert.equal(snapshot.request.body.tools[0].size, '2160x3840', 'Requested dimensions must remain separate from the server report')
  assert.equal(snapshot.serverReportedParameters.label, '服务端参数回报；实际图片尺寸见预览')
  assert.equal(snapshot.serverReportedParameters.responseModel, 'gpt-6-luna')
  assert.ok(snapshot.serverReportedParameters.imageToolObservations.some(value => value.eventType === 'response.created' && value.model === 'gpt-image-2-codex' && value.size === 'auto' && value.quality === 'auto'), 'Snapshot must expose the server tool downgrade')
  assert.ok(snapshot.serverReportedParameters.imageToolObservations.some(value => value.size === '768x512'), 'Image result metadata must remain distinct from the initial tool configuration')
  assert.ok((await large.frame.locator('#debugImages').textContent()).includes('768 × 512'), 'Preview must report decoded pixels')
  assert.equal(await large.frame.locator('main').evaluate(() => window.__bridgeCalls.length), beforeUiRun + 1, 'Observing changed server parameters must not retry the request')
  await large.frame.getByRole('button', { name: '导出调试内容', exact: true }).click()
  const exported = JSON.parse(await large.frame.locator('#debugExportText').inputValue())
  assert.equal(exported.completeCapture, true)
  assert.equal(exported.request.body.future_option.unchanged[0], 'raw')
  assert.deepEqual(exported.serverReportedParameters, snapshot.serverReportedParameters)
  assert.ok(exported.rawResponse.includes('preserved-in-raw-export'), 'Unknown upstream fields must survive in the raw export')
  assert.ok(exported.serverReportedParameters.imageToolObservations.every(value => !Object.hasOwn(value, 'provider_only')), 'The summary exposes only selected fields')
  assert.ok(exported.rawResponse.includes(fixtures.large), 'Export must contain the full Base64 despite the preview truncation')
  assert.ok(exported.rawResponse.includes('vendor.custom'))
  assert.ok(!JSON.stringify(exported.response.headers).includes('PRIVATEVALUE'))
  assert.ok(!JSON.stringify(exported.response.headers).includes('opaque-'))
  await large.frame.getByRole('button', { name: '收起', exact: true }).click()
  const screenshotSize = await resize(large)
  await large.page.screenshot({ path: path.join(output, 'debug-light-1360.png'), fullPage: true })
  const managementCalls = await large.frame.locator('main').evaluate(() => window.__managementCalls)
  assert.ok(managementCalls.every(call => ['api/bootstrap', 'api/models'].includes(call.path)))
  results.push({ scenario: 'debug-raw-body-preview-full-export-no-maintenance', passed: true, screenshotSize })
  await large.page.close()

  for (const [theme, width] of [['dark', 1360], ['light', 420]]) {
    const current = await openPage(theme, width, 'json-png')
    await current.frame.getByRole('button', { name: '运行一次', exact: true }).click()
    await current.frame.locator('#debugRunState').getByText('完整结束', { exact: true }).waitFor()
    const emptyReport = JSON.parse(await current.frame.locator('#debugRequestSnapshot').textContent()).serverReportedParameters
    assert.equal(emptyReport.responseModel, '未返回')
    assert.equal(emptyReport.imageToolObservations, '未返回', 'Missing parameter metadata must be explicit')
    const metrics = await resize(current)
    await current.page.screenshot({ path: path.join(output, 'debug-' + theme + '-' + width + '.png'), fullPage: true })
    results.push({ scenario: 'debug-' + theme + '-' + width, passed: true, metrics })
    await current.page.close()
  }
  assert.deepEqual(failures, [], 'Actual sandbox assets must have no JS/CSP errors')
} finally {
  await browser.close()
  await writeFile(path.join(output, 'report.json'), JSON.stringify({ results, failures }, null, 2))
}
console.log(JSON.stringify({ results, failures, outputDirectory: output }, null, 2))
