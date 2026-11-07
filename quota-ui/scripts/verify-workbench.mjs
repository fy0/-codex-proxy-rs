import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath, pathToFileURL } from 'node:url'

const directory = path.dirname(fileURLToPath(import.meta.url))
const root = path.resolve(directory, '../..')
const moduleFlag = process.argv.indexOf('--playwright-module')
const playwrightModule = moduleFlag >= 0 ? process.argv[moduleFlag + 1] : 'playwright'
const { chromium } = await import(playwrightModule.startsWith('.') || path.isAbsolute(playwrightModule)
  ? pathToFileURL(path.resolve(playwrightModule)).href
  : playwrightModule)
const output = path.join(root, 'quota-ui/dist/qa')
await mkdir(output, { recursive: true })
const files = Object.fromEntries(await Promise.all(['index.html', 'app.css', 'app.js', 'image-client.js']
  .map(async name => [name, await readFile(path.join(root, 'web', name), 'utf8')])))
const csp = "default-src 'none'; script-src 'unsafe-inline' data:; style-src 'unsafe-inline' data:; img-src data:; font-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'"
const report = {
  kind: 'browser-fixture',
  evidenceBoundary: '正式 CSP 与 sandbox 下的本地模拟 Responses 流，不是真实 CPR 宿主或上游生图验收。',
  baselineCommit: 'e0d473d2aca0917e43c4da67d057e0fb75639dc0',
  sourceSha256: Object.fromEntries(Object.entries(files).map(([name, value]) =>
    [name, createHash('sha256').update(value).digest('hex')])),
  checks: [],
}
const failures = []
const browser = await chromium.launch({ headless: true })
let currentPage

function installFixture(images) {
  const initialSelection = {
    clientKeyId: 'qa-client-key', driverModel: 'gpt-6-luna',
    imageModel: 'gpt-image-2.5-sunburst', size: '1024x1024',
    quality: 'auto', count: 1, parallelism: 2,
  }
  const configuration = {
    timeoutMs: 300000, bridgeMode: 'auto', driverModel: 'gpt-6-luna',
    imageModels: ['gpt-image-2.5-sunburst', 'gpt-image-2.6-sunburst'],
    imageAccountIds: ['qa-account'], imageGroupIds: ['qa-group'],
    imageStorageDir: 'C:\\fixture\\old-images', backgroundConcurrency: 7,
    concurrencyPerAccount: 8,
    quotaGuard: [{ accountId: 'qa-account', fiveHourLimitPercent: 90, weeklyLimitPercent: 95, dailyLimitUsd: '12.5' }],
    maxObservationAgeSeconds: 300,
  }
  const largeBytes = Uint8Array.from(atob(images.large.split(',')[1]), value => value.charCodeAt(0))
  const fixture = {
    images, configuration, calls: [], modelCalls: [], active: 0, maximumActive: 0,
    violations: [], preferences: { version: 7, selection: initialSelection },
    release(index, mode = 'completed', large = false) {
      const call = this.modelCalls[index]
      if (!call || call.finished || call.released) throw new Error('无效的测试流释放')
      call.released = true
      const item = {
        id: 'ig_fixture_' + index, type: 'image_generation_call', status: 'completed',
        result: (large ? images.large : images.small).split(',')[1],
      }
      const response = { id: 'resp_fixture_' + index, status: mode, output: [item] }
      if (mode === 'failed') response.error = { code: 'fixture_failed', message: 'Synthetic response failure' }
      if (mode === 'incomplete') response.incomplete_details = { reason: 'max_output_tokens' }
      const events = [
        { type: 'response.created', response: { id: response.id, status: 'in_progress' } },
        { type: 'response.output_item.done', output_index: 0, item },
        { type: 'response.' + mode, response },
      ]
      call.bytes = new TextEncoder().encode(events.map(event =>
        'event: ' + event.type + '\r\ndata: ' + JSON.stringify(event) + '\r\n\r\n').join('')
        + 'data: [DONE]\r\n\r\n')
      call.responseBytes = call.bytes.length
      call.unblock()
    },
  }
  window.__workbenchFixture = fixture
  document.addEventListener('securitypolicyviolation', event => {
    fixture.violations.push({ directive: event.violatedDirective, blocked: event.blockedURI })
  })
  const json = value => ({
    status: 200, contentType: 'application/json',
    body: new TextEncoder().encode(JSON.stringify(value)).buffer,
  })
  const records = [
    { id: 'legacy-complete', status: 'succeeded', resultBytes: largeBytes.length, resultMime: 'image/png' },
    { id: 'legacy-queued', status: 'queued' },
    { id: 'legacy-running', status: 'running' },
    { id: 'legacy-unknown', status: 'future_host_state' },
    { id: 'legacy-failed', status: 'failed', error: 'Synthetic historical failure' },
  ].map(item => ({
    prompt: 'Historical image fixture', imageModel: 'gpt-image-2.5-sunburst',
    size: '1024x1024', quality: 'auto', createdMs: 1000, ...item,
  }))
  window.codexProxyPlugin = {
    version: 2, theme: 'light',
    async request(input) {
      const body = input.body ? JSON.parse(input.body) : null
      fixture.calls.push({ method: input.method, path: input.path, body })
      if (input.path === 'api/bootstrap') return json({
        enabled: true, driverModel: configuration.driverModel,
        configuration: structuredClone(configuration), imageModels: configuration.imageModels,
        candidateImageModels: configuration.imageModels,
        keys: [{ id: 'qa-client-key', name: 'Fixture key', enabled: true }],
      })
      if (input.path === 'api/preferences') {
        if (input.method === 'POST') {
          if (body.expectedVersion !== fixture.preferences.version) throw new Error('偏好 CAS 版本不匹配')
          fixture.preferences = { version: fixture.preferences.version + 1, selection: structuredClone(body.selection) }
        }
        return json(structuredClone(fixture.preferences))
      }
      if (input.path === 'api/models') return json({
        models: ['gpt-6-luna', 'gpt-5.6-luna'],
        imageModels: configuration.imageModels,
      })
      if (input.path === 'api/account-options') return json({
        accountOptions: [{ id: 'qa-account', name: 'Fixture account', enabled: true, groupIds: ['qa-group'] }],
        groupOptions: ['qa-group'], accountOptionsTruncated: false,
      })
      if (input.path === 'api/jobs' && input.method === 'GET') return json({ jobs: structuredClone(records) })
      if (input.path === 'api/jobs/image') {
        if (body.id !== 'legacy-complete' || !Number.isSafeInteger(body.offset)) throw new Error('旧图片请求参数无效')
        const chunk = largeBytes.slice(body.offset, body.offset + 256 * 1024)
        return { status: 200, contentType: 'application/octet-stream', body: chunk.buffer }
      }
      if (input.path === 'api/jobs/cancel') return json({ canceled: true })
      throw new Error('意外管理请求：' + input.method + ' ' + input.path)
    },
    models: {
      responses(input) {
        const call = {
          index: fixture.modelCalls.length, clientKeyId: input.clientKeyId,
          body: structuredClone(input.body), keys: Object.keys(input).sort(),
          aborted: false, readerCanceled: false, released: false, finished: false,
          chunks: 0, maximumChunk: 0, responseBytes: 0, offset: 0,
        }
        const ready = new Promise(resolve => { call.unblock = resolve })
        fixture.modelCalls.push(call)
        fixture.active++
        fixture.maximumActive = Math.max(fixture.maximumActive, fixture.active)
        let streamController
        const finish = () => {
          if (call.finished) return
          call.finished = true
          fixture.active--
          input.signal.removeEventListener('abort', abort)
        }
        const abort = () => {
          if (call.finished) return
          call.aborted = true
          finish()
          streamController.error(new DOMException('Fixture aborted', 'AbortError'))
          call.unblock()
        }
        const body = new ReadableStream({
          start(controller) {
            streamController = controller
            input.signal.addEventListener('abort', abort, { once: true })
            if (input.signal.aborted) abort()
          },
          async pull(controller) {
            await ready
            if (call.finished) return
            if (call.offset === call.bytes.length) {
              finish()
              controller.close()
              return
            }
            const sizes = [1, 17, 65536, 7, 4096, 65536]
            const chunk = call.bytes.slice(call.offset, call.offset + sizes[call.chunks % sizes.length])
            call.offset += chunk.length
            call.chunks++
            call.maximumChunk = Math.max(call.maximumChunk, chunk.length)
            controller.enqueue(chunk)
          },
          cancel() {
            call.readerCanceled = true
            finish()
            call.unblock()
          },
        })
        return Promise.resolve(new Response(body, {
          status: 200, headers: { 'content-type': 'text/event-stream', 'x-request-id': 'fixture-request-' + call.index },
        }))
      },
    },
  }
}

function dataUrl(type, value) {
  return 'data:' + type + ';base64,' + Buffer.from(value).toString('base64')
}
function expectedBody(selection, prompt) {
  // 独立抄录 v0.5.1 固定提交的模板，不能从当前 app.js 动态推导预期。
  return {
    model: selection.driverModel, stream: true, store: false,
    input: [{ type: 'message', role: 'user', content: [{ type: 'input_text', text: prompt }] }],
    instructions: 'Use image_generation to create exactly one image corresponding closely to the user instruction. Preserve its original language and visual constraints.',
    tools: [{
      type: 'image_generation', model: selection.imageModel, action: 'auto',
      size: selection.size, quality: selection.quality, output_format: 'png',
    }],
    tool_choice: 'auto', parallel_tool_calls: false, include: ['reasoning.encrypted_content'],
  }
}
async function until(frame, predicate, argument) {
  // 不放宽正式 CSP；CDP evaluate 有界轮询，避开 waitForFunction 内部字符串 eval。
  const deadline = Date.now() + 12000
  while (Date.now() < deadline) {
    try {
      if (await frame.evaluate(predicate, argument)) return
    } catch (error) {
      if (!/Execution context was destroyed|Cannot find context/.test(String(error))) throw error
    }
    await new Promise(resolve => setTimeout(resolve, 30))
  }
  throw new Error('浏览器行为等待超时：' + predicate.toString().slice(0, 180))
}

async function openPage(images) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } })
  currentPage = page
  page.setDefaultTimeout(12000)
  page.on('pageerror', error => failures.push(error.message))
  page.on('console', message => { if (message.type() === 'error') failures.push(message.text()) })
  const bootstrap = '(' + installFixture.toString() + ')(' + JSON.stringify(images) + ');'
  const document = files['index.html']
    .replace('<head>', '<head><meta http-equiv="Content-Security-Policy" content="' + csp + '"><script>' + bootstrap + '</script>')
    .replace('./app.css', dataUrl('text/css', files['app.css']))
    .replace('./image-client.js', dataUrl('text/javascript', files['image-client.js']))
    .replace('./app.js', dataUrl('text/javascript', files['app.js']))
  await page.setContent('<!doctype html><html><body style="margin:0"><iframe title="Workbench fixture" sandbox="allow-scripts" style="width:100%;height:2400px;border:0"></iframe></body></html>')
  await page.locator('iframe').evaluate((element, value) => { element.srcdoc = value }, document)
  const frame = await (await page.locator('iframe').elementHandle()).contentFrame()
  await until(frame, () => window.__workbenchFixture && !document.getElementById('generate').disabled)
  assert.equal(await page.locator('iframe').getAttribute('sandbox'), 'allow-scripts')
  return { page, frame }
}
async function assertClean(frame) {
  const state = await frame.evaluate(() => ({
    violations: window.__workbenchFixture.violations,
    forbiddenJobs: window.__workbenchFixture.calls.filter(call => call.method === 'POST' && call.path === 'api/jobs'),
    maximumActive: window.__workbenchFixture.maximumActive,
  }))
  assert.deepEqual(state.violations, [])
  assert.deepEqual(state.forbiddenJobs, [])
  assert.ok(state.maximumActive <= 4)
  assert.deepEqual(failures, [])
}
async function release(frame, index, mode = 'completed', large = false) {
  await frame.evaluate(({ index, mode, large }) => window.__workbenchFixture.release(index, mode, large), { index, mode, large })
}
async function waitCalls(frame, count) {
  await until(frame, value => window.__workbenchFixture.modelCalls.length === value, count)
}
async function waitCards(frame, state, count) {
  await until(frame, ({ state, count }) => document.querySelectorAll('#taskGrid .task-card.' + state).length === count, { state, count })
}
async function clearCards(frame) {
  await frame.locator('#clearFinished').click()
  await until(frame, () => document.querySelectorAll('#taskGrid .task-card').length === 0)
}
function passed(name, details = {}) {
  report.checks.push({ name, passed: true, ...details })
  console.log('PASS: ' + name)
}

try {
  const canvasPage = await browser.newPage()
  const images = await canvasPage.evaluate(() => {
    const canvas = document.createElement('canvas')
    canvas.width = canvas.height = 1024
    const context = canvas.getContext('2d')
    context.fillStyle = '#f07030'
    context.fillRect(0, 0, 1024, 1024)
    const small = canvas.toDataURL('image/png')
    const pixels = context.createImageData(1024, 1024)
    let value = 123456789
    for (let index = 0; index < pixels.data.length; index += 4) {
      value ^= value << 13; value ^= value >>> 17; value ^= value << 5
      pixels.data[index] = value & 255
      pixels.data[index + 1] = (value >>> 8) & 255
      pixels.data[index + 2] = (value >>> 16) & 255
      pixels.data[index + 3] = 255
    }
    context.putImageData(pixels, 0, 0)
    return { small, large: canvas.toDataURL('image/png') }
  })
  await canvasPage.close()
  const largeBytes = Buffer.from(images.large.split(',')[1], 'base64').length
  assert.ok(largeBytes > 1024 * 1024, '测试 PNG 必须真实超过 1 MiB')
  report.fixturePngBytes = largeBytes

  const primary = await openPage(images)
  const { frame } = primary
  const initial = await frame.evaluate(() => ({
    paths: window.__workbenchFixture.calls.map(call => call.path),
    modelCalls: window.__workbenchFixture.modelCalls.length,
  }))
  assert.equal(initial.modelCalls, 0)
  assert.ok(!initial.paths.includes('api/account-options'))
  assert.ok(!initial.paths.includes('api/jobs'))
  assert.ok(!initial.paths.includes('api/jobs/image'))

  const prompt = 'Fixture image: 中文圆形'
  await frame.locator('#prompt').fill(prompt)
  const immediateCount = await frame.evaluate(() => {
    document.getElementById('generate').click()
    return window.__workbenchFixture.modelCalls.length
  })
  assert.equal(immediateCount, 1, '空队列点击必须立即调用管理模型桥')
  const firstCall = await frame.evaluate(() => {
    const call = window.__workbenchFixture.modelCalls[0]
    return { keys: call.keys, clientKeyId: call.clientKeyId, body: call.body }
  })
  assert.deepEqual(firstCall.keys, ['body', 'clientKeyId', 'signal'])
  assert.equal(firstCall.clientKeyId, 'qa-client-key')
  assert.deepEqual(firstCall.body, expectedBody({
    driverModel: 'gpt-6-luna', imageModel: 'gpt-image-2.5-sunburst', size: '1024x1024', quality: 'auto',
  }, prompt))
  await release(frame, 0, 'completed', true)
  await waitCards(frame, 'succeeded', 1)
  await until(frame, () => document.querySelector('#taskGrid img').naturalWidth === 1024)
  const preview = await frame.evaluate(() => {
    const image = document.querySelector('#taskGrid img')
    const download = document.querySelector('#taskGrid .task-actions a')
    const call = window.__workbenchFixture.modelCalls[0]
    return {
      mime: image.src.slice(5, image.src.indexOf(';')),
      width: image.naturalWidth, height: image.naturalHeight,
      exactBytes: image.src === window.__workbenchFixture.images.large,
      downloadMatches: download.href === image.src, filename: download.download,
      chunks: call.chunks, maximumChunk: call.maximumChunk, calls: window.__workbenchFixture.modelCalls.length,
      actualSize: document.querySelector('#taskGrid .task-model').textContent,
    }
  })
  assert.equal(preview.mime, 'image/png')
  assert.equal(preview.width, 1024)
  assert.equal(preview.height, 1024)
  assert.equal(preview.exactBytes, true)
  assert.equal(preview.downloadMatches, true)
  assert.match(preview.filename, /\.png$/)
  assert.ok(preview.chunks > 1 && preview.maximumChunk <= 65536)
  assert.equal(preview.calls, 1)
  assert.match(preview.actualSize, /实际 1024×1024/)
  await primary.page.screenshot({ path: path.join(output, 'workbench-preview.png'), fullPage: false })
  passed('即时单次桥调用、v0.5.1 JSON 深比较、无账号绑定、大图跨块预览与下载属性', preview)

  await frame.locator('#imageModel').selectOption('gpt-image-2.6-sunburst')
  await frame.locator('#generate').click()
  await waitCalls(frame, 2)
  const secondBody = await frame.evaluate(() => window.__workbenchFixture.modelCalls[1].body)
  assert.deepEqual(secondBody, expectedBody({
    driverModel: 'gpt-6-luna', imageModel: 'gpt-image-2.6-sunburst', size: '1024x1024', quality: 'auto',
  }, prompt))
  await release(frame, 1)
  await waitCards(frame, 'succeeded', 2)
  await clearCards(frame)
  passed('两个 sunburst 图片模型均使用兼容请求模板')

  for (const [index, mode] of ['failed', 'incomplete'].entries()) {
    await frame.locator('#generate').click()
    await waitCalls(frame, index + 3)
    await release(frame, index + 2, mode)
    await waitCards(frame, 'failed', 1)
    assert.equal(await frame.locator('#taskGrid img').evaluate(image => image.hidden), true)
    assert.ok((await frame.locator('#taskGrid .task-error').innerText()).length > 0)
    assert.equal(await frame.locator('#taskGrid .succeeded').count(), 0)
    await clearCards(frame)
  }
  assert.equal(await frame.evaluate(() => window.__workbenchFixture.modelCalls.length), 4)
  passed('HTTP 200 的 failed/incomplete 即使含图片也不误报成功、不重发')

  await frame.locator('#configPanel > summary').click()
  await until(frame, () => window.__workbenchFixture.calls.some(call => call.path === 'api/account-options'))
  const generated = JSON.parse(await frame.locator('#configJson').inputValue())
  assert.equal(generated.imageStorageDir, 'C:\\fixture\\old-images')
  assert.equal(generated.backgroundConcurrency, 7)
  assert.equal(generated.concurrencyPerAccount, 8)
  assert.equal(generated.quotaGuard[0].dailyLimitUsd, '12.5')
  await frame.locator('#configPanel > summary').click()
  await frame.locator('#configPanel > summary').click()
  assert.equal(await frame.evaluate(() => window.__workbenchFixture.calls.filter(call => call.path === 'api/account-options').length), 1)

  await frame.locator('#legacyPanel > summary').click()
  await frame.locator('#legacyGrid [data-task-id="legacy-complete"]').waitFor()
  assert.equal(await frame.evaluate(() => window.__workbenchFixture.calls.filter(call => call.path === 'api/jobs/image').length), 0)
  assert.equal(await frame.locator('#legacyGrid .task-card').count(), 5)
  await frame.locator('#legacyGrid [data-task-id="legacy-complete"]').getByText('读取旧图片', { exact: true }).click()
  await until(frame, () => document.querySelector('#legacyGrid [data-task-id="legacy-complete"] img').naturalWidth === 1024)
  const history = await frame.evaluate(() => ({
    exactBytes: document.querySelector('#legacyGrid [data-task-id="legacy-complete"] img').src === window.__workbenchFixture.images.large,
    reads: window.__workbenchFixture.calls.filter(call => call.path === 'api/jobs/image').map(call => call.body.offset),
    modelCalls: window.__workbenchFixture.modelCalls.length,
    unknown: document.querySelector('#legacyGrid [data-task-id="legacy-unknown"] .task-state').textContent,
  }))
  assert.equal(history.exactBytes, true)
  assert.equal(history.reads.length, Math.ceil(largeBytes / (256 * 1024)))
  assert.deepEqual(history.reads, history.reads.map((_, index) => index * 256 * 1024))
  assert.equal(history.modelCalls, 4)
  assert.equal(history.unknown, 'future_host_state')
  passed('配置和历史按需读取，旧图片分片完整，未知/旧队列不重发，兼容配置字段保留', { legacyChunks: history.reads.length })

  const expectedSelection = {
    clientKeyId: 'qa-client-key', driverModel: 'gpt-5.6-luna', imageModel: 'gpt-image-2.6-sunburst',
    size: '2048x2048', quality: 'high', count: 3, parallelism: 3,
  }
  for (const [id, value] of Object.entries({
    driverModel: expectedSelection.driverModel, imageModel: expectedSelection.imageModel,
    size: expectedSelection.size, quality: expectedSelection.quality, batchCount: '3', parallelism: '3',
  })) await frame.locator('#' + id).selectOption(value)
  await until(frame, expected => JSON.stringify(window.__workbenchFixture.preferences.selection) === JSON.stringify(expected), expectedSelection)
  await frame.locator('#refresh').click()
  await until(frame, () => !document.getElementById('generate').disabled && document.getElementById('driverModel').value === 'gpt-5.6-luna')
  const remembered = await frame.evaluate(() => ({
    clientKeyId: document.getElementById('clientKey').value,
    driverModel: document.getElementById('driverModel').value,
    imageModel: document.getElementById('imageModel').value,
    size: document.getElementById('size').value,
    quality: document.getElementById('quality').value,
    count: Number(document.getElementById('batchCount').value),
    parallelism: Number(document.getElementById('parallelism').value),
  }))
  assert.deepEqual(remembered, expectedSelection)
  assert.equal(await frame.evaluate(() => window.__workbenchFixture.configuration.imageStorageDir), 'C:\\fixture\\old-images')
  await assertClean(frame)
  await primary.page.close()
  passed('偏好 CAS 保存与刷新恢复，原配置不被偏好覆盖')

  const queue = await openPage(images)
  await queue.frame.locator('#parallelism').selectOption('4')
  await queue.frame.locator('#batchCount').selectOption('6')
  await queue.frame.locator('#generate').click()
  await waitCalls(queue.frame, 4)
  await waitCards(queue.frame, 'running', 4)
  assert.equal(await queue.frame.locator('#taskGrid .queued').count(), 2)
  await release(queue.frame, 2)
  await waitCalls(queue.frame, 5)
  assert.equal(await queue.frame.evaluate(() => window.__workbenchFixture.modelCalls[0].finished), false)
  await release(queue.frame, 1)
  await waitCalls(queue.frame, 6)
  for (const index of [0, 3, 4, 5]) await release(queue.frame, index)
  await waitCards(queue.frame, 'succeeded', 6)
  assert.equal(await queue.frame.evaluate(() => window.__workbenchFixture.maximumActive), 4)
  await clearCards(queue.frame)
  passed('本页并发上限4，任意一张完成即补位，无整批等待')

  await queue.frame.locator('#generate').click()
  await waitCalls(queue.frame, 10)
  await queue.frame.locator('#taskGrid .task-card').first().getByText('停止此张', { exact: true }).click()
  await waitCalls(queue.frame, 11)
  assert.equal(await queue.frame.evaluate(() => window.__workbenchFixture.modelCalls[6].aborted), true)
  await queue.frame.locator('#cancel').click()
  await waitCards(queue.frame, 'canceled', 6)
  const canceled = await queue.frame.evaluate(() => ({
    count: window.__workbenchFixture.modelCalls.length,
    aborted: window.__workbenchFixture.modelCalls.slice(6).filter(call => call.aborted).length,
    active: window.__workbenchFixture.active,
  }))
  assert.deepEqual(canceled, { count: 11, aborted: 5, active: 0 })
  await assertClean(queue.frame)
  await queue.page.close()
  passed('单张取消与停止全部传播 AbortSignal，未启动任务不发送')

  const closing = await openPage(images)
  await closing.frame.locator('#batchCount').selectOption('6')
  await closing.frame.locator('#generate').click()
  await waitCalls(closing.frame, 2)
  await closing.frame.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: false })))
  await until(closing.frame, () => window.__workbenchFixture.active === 0)
  assert.deepEqual(await closing.frame.evaluate(() => ({
    count: window.__workbenchFixture.modelCalls.length,
    aborted: window.__workbenchFixture.modelCalls.filter(call => call.aborted).length,
  })), { count: 2, aborted: 2 })
  await assertClean(closing.frame)
  await closing.page.close()
  passed('pagehide 取消在途桥请求，剩余队列不再发送')

  report.passed = true
  console.log(JSON.stringify({ passed: true, checks: report.checks.length, fixturePngBytes: largeBytes }))
} catch (error) {
  report.passed = false
  report.error = error.stack || String(error)
  if (currentPage && !currentPage.isClosed()) {
    await currentPage.screenshot({ path: path.join(output, 'workbench-failure.png'), fullPage: false }).catch(() => {})
  }
  console.error(report.error)
  process.exitCode = 1
} finally {
  report.browserErrors = failures
  await writeFile(path.join(output, 'workbench-report.json'), JSON.stringify(report, null, 2))
  await browser.close()
}
