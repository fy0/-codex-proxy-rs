#!/usr/bin/env node
// Run the real injected bridge, parent stream manager and HTTP API functions together.
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { stripTypeScriptTypes } from 'node:module'
import path from 'node:path'
import { pathToFileURL } from 'node:url'

const core = path.resolve(process.argv[2] ?? '')
if (!process.argv[2])
  throw new Error('Usage: node verify-bridge.mjs /path/to/patched-core')
const modules = new Map()
const state = { calls: [], respond: null }
globalThis.__nativeImagesBridgeTest = state
globalThis.window = globalThis

function dataModule(source) {
  return 'data:text/javascript;base64,' + Buffer.from(source).toString('base64')
}
const requestMock = dataModule(
  'export default function request() { throw new Error("unexpected regular management request") }\n'
  + 'export const requestRaw = request;\n'
  + 'export function requestStream(config, options) { const state=globalThis.__nativeImagesBridgeTest; state.calls.push({config, options}); return state.respond(config, options); }\n',
)
const i18nMock = dataModule('export function t(value) { return value }')
const operationMock = dataModule('export function errorMessage(error, fallback) { return error instanceof Error ? error.message : fallback ?? String(error) }')

async function compile(relative, replacements = {}) {
  const source = await readFile(path.join(core, relative), 'utf8')
  let code = stripTypeScriptTypes(source, { mode: 'transform', sourceMap: false })
  for (const [name, replacement] of Object.entries(replacements)) {
    const quoted = JSON.stringify(replacement)
    code = code.replaceAll("from '" + name + "'", 'from ' + quoted)
      .replaceAll('from "' + name + '"', 'from ' + quoted)
  }
  const uri = dataModule(code + '\n//# sourceURL=' + pathToFileURL(path.join(core, relative)).href)
  modules.set(relative, uri)
  return uri
}
const prefix = 'frontend/src/views/plugins/management/'
const contract = await compile(prefix + 'bridge-contract.ts')
const api = await compile('frontend/src/api/modules/plugins/extensions.ts', { '../../request': requestMock })
const bridgeModule = await compile(prefix + 'bridge-models.ts', { './bridge-contract': contract })
const streamsModule = await compile(prefix + 'model-streams.ts', {
  './bridge-contract': contract, '@/api': api, '@/i18n': i18nMock, '@/utils/operation': operationMock,
})
const { createPluginModelBridge } = await import(bridgeModule)
const { createPluginModelStreams } = await import(streamsModule)
const { MAXIMUM_MODEL_CHUNK_BYTES } = await import(contract)
assert.equal(MAXIMUM_MODEL_CHUNK_BYTES, 64 * 1024)

let passed = 0
async function test(name, fn) {
  state.calls = []
  try {
    await fn()
    passed++
    console.log('PASS ' + name)
  }
  finally {
    state.respond = null
  }
}
function pair() {
  let id = 0
  let disposed = false
  const messages = []
  const source = {}
  let injected
  const parent = createPluginModelStreams({
    target: { instanceId: 'instance', artifactSha256: 'a'.repeat(64), revision: 7 },
    requestIds: new Set(),
    post(_source, type, messageId, payload, error) {
      messages.push({ type, id: messageId, payload, error })
      queueMicrotask(() => { if (!disposed) injected.handle({ type, id: messageId, payload, error }) })
    },
  })
  injected = createPluginModelBridge({
    nextRequestId: () => ++id,
    isDisposed: () => disposed,
    post(type, messageId, payload) {
      const message = { type, id: messageId, payload }
      messages.push(message)
      // postMessage delivers asynchronously in the actual sandbox boundary.
      queueMicrotask(() => {
        if (disposed) return
        if (type === 'models-request')
          void parent.request(source, message)
        else if (type === 'models-pull')
          void parent.pull(source, message)
        else if (type === 'models-cancel')
          parent.cancel(source, messageId)
        else throw new Error('unexpected message type: ' + type)
      })
    },
  })
  return {
    injected, parent, messages, source,
    dispose() { disposed = true; injected.dispose(); parent.dispose() },
  }
}

await test('native operation reaches the independent Images HTTP route with unchanged JSON', async () => {
  const body = {
    model: 'gpt-image-2.5-sunburst', prompt: 'a tree', size: '1152x2048',
    quality: 'high', output_format: 'webp', stream: false, n: 1,
    future: { preserve: true, order: ['z', 'a'] },
  }
  state.respond = async () => new Response('{"created":1,"data":[{"b64_json":"AAEC"}]}', {
    status: 201, headers: { 'content-type': 'application/json', 'x-request-id': 'upstream-fixture' },
  })
  const p = pair()
  try {
    const response = await p.injected.imagesGenerate({ clientKeyId: 'client-key', body })
    assert.equal(response.status, 201)
    assert.equal(response.headers.get('x-request-id'), 'upstream-fixture')
    assert.deepEqual(await response.json(), { created: 1, data: [{ b64_json: 'AAEC' }] })
    assert.equal(state.calls.length, 1)
    assert.match(state.calls[0].config.url, /\/models\/images\/generations\?clientKeyId=client-key$/)
    assert.equal(state.calls[0].config.data, JSON.stringify(body))
    assert.equal(state.calls[0].config.timeout, 0)
    assert.equal(state.calls[0].config.headers['Content-Type'], 'application/json')
    assert.equal(state.calls[0].options.accept({ header: () => '1' }), true)
    assert.equal(p.messages[0].payload.operation, 'images.generate')
  }
  finally { p.dispose() }
})

await test('large native JSON is delivered without Base64 truncation across 64 KiB pulls', async () => {
  const raw = '{"data":[{"b64_json":"' + 'A'.repeat(2 * 1024 * 1024) + '"}],"future":9007199254740993}'
  state.respond = async () => new Response(raw, { headers: { 'content-type': 'application/json' } })
  const p = pair()
  try {
    const response = await p.injected.imagesGenerate({ clientKeyId: 'client-key', body: { prompt: 'test' } })
    assert.equal(await response.text(), raw)
    const chunks = p.messages.filter(message => message.type === 'models-chunk')
    assert.ok(chunks.length > 32)
    assert.ok(chunks.every(message => message.payload.body.byteLength <= MAXIMUM_MODEL_CHUNK_BYTES))
    assert.equal(state.calls.length, 1)
  }
  finally { p.dispose() }
})

await test('Responses remains separate and retains its existing route', async () => {
  const raw = 'event: response.completed\ndata: {"type":"response.completed"}\n\n'
  state.respond = async () => new Response(raw, { headers: { 'content-type': 'text/event-stream' } })
  const p = pair()
  try {
    const response = await p.injected.responses({ clientKeyId: 'client-key', body: { model: 'gpt-6-luna', stream: true } })
    assert.equal(await response.text(), raw)
    assert.match(state.calls[0].config.url, /\/models\/responses\?/)
    assert.equal(p.messages[0].payload.operation, 'responses')
  }
  finally { p.dispose() }
})

await test('manual stream true and unknown native parameters remain intact', async () => {
  const body = { model: 'gpt-image-2.6-flare', stream: true, partial_images: 2, quality: 'high', experimental: [1, 2] }
  state.respond = async () => new Response('{"error":{"message":"upstream rejects stream"}}', {
    status: 400, headers: { 'content-type': 'application/json' },
  })
  const p = pair()
  try {
    const response = await p.injected.imagesGenerate({ clientKeyId: 'client-key', body })
    assert.equal(response.status, 400)
    assert.equal((await response.json()).error.message, 'upstream rejects stream')
    assert.equal(state.calls[0].config.data, JSON.stringify(body))
    assert.equal(state.calls.length, 1)
  }
  finally { p.dispose() }
})

await test('native HTTP 503 passes through without a Responses retry', async () => {
  const raw = '{"error":{"code":"no_available_provider"}}'
  state.respond = async () => new Response(raw, { status: 503, headers: { 'content-type': 'application/json' } })
  const p = pair()
  try {
    const response = await p.injected.imagesGenerate({ clientKeyId: 'client-key', body: { prompt: 'test' } })
    assert.equal(response.status, 503)
    assert.equal(await response.text(), raw)
    assert.equal(state.calls.length, 1)
    assert.match(state.calls[0].config.url, /\/images\/generations\?/)
  }
  finally { p.dispose() }
})

await test('parent rejects unsupported operation without dispatching HTTP', async () => {
  state.respond = async () => { throw new Error('must not dispatch') }
  const p = pair()
  try {
    await p.parent.request(p.source, { type: 'models-request', id: 9, payload: { operation: 'images.edits', clientKeyId: 'client-key', body: '{}' } })
    assert.equal(state.calls.length, 0)
    assert.equal(p.messages.at(-1).type, 'models-error')
  }
  finally { p.dispose() }
})

await test('legacy v2 Responses messages remain compatible', async () => {
  state.respond = async () => new Response('{}', { headers: { 'content-type': 'application/json' } })
  const p = pair()
  try {
    await p.parent.request(p.source, { type: 'models-request', id: 9, payload: { clientKeyId: 'client-key', body: '{}' } })
    assert.match(state.calls[0].config.url, /\/models\/responses\?/)
  }
  finally { p.dispose() }
})

await test('cancellation before headers aborts the native HTTP request', async () => {
  state.respond = config => new Promise((resolve, reject) => {
    config.signal.addEventListener('abort', () => reject(new DOMException('Canceled', 'AbortError')), { once: true })
  })
  const p = pair()
  try {
    const controller = new AbortController()
    const promise = p.injected.imagesGenerate({ clientKeyId: 'client-key', body: { prompt: 'test' }, signal: controller.signal })
    await new Promise(resolve => setImmediate(resolve))
    controller.abort()
    await assert.rejects(promise, error => error.name === 'AbortError')
    assert.equal(state.calls.length, 1)
    assert.equal(state.calls[0].config.signal.aborted, true)
  }
  finally { p.dispose() }
})

await test('canceling after native headers closes the parent reader', async () => {
  state.respond = async () => new Response('{"data":[]}', { headers: { 'content-type': 'application/json' } })
  const p = pair()
  try {
    const controller = new AbortController()
    const response = await p.injected.imagesGenerate({ clientKeyId: 'client-key', body: { prompt: 'test' }, signal: controller.signal })
    controller.abort()
    await assert.rejects(response.text())
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(state.calls[0].config.signal.aborted, true)
    assert.equal(p.parent.has(1), false)
  }
  finally { p.dispose() }
})
console.log(JSON.stringify({ passed, failed: 0, suite: 'native Images management bridge', base: 'ca5901ba568ab398948dcf32239854337c6928dc' }))
