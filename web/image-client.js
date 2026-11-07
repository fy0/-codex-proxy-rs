/* 共享 Responses 客户端：只使用公开管理桥，调用一次，保持请求 JSON 不变。 */
(function (global) {
  'use strict';
  const limits = Object.freeze({
    requestBytes: 8 * 1024 * 1024,
    responseBytes: 256 * 1024 * 1024,
    eventCharacters: 192 * 1024 * 1024,
    imageCharacters: 160 * 1024 * 1024,
    timeoutMs: 10 * 60 * 1000
  });
  const credentialNames = new Set([
    'authorization', 'proxyauthorization', 'cookie', 'setcookie',
    'apikey', 'authtoken', 'accesstoken', 'refreshtoken', 'clientsecret', 'password', 'privatekey',
    'xapikey', 'xauthtoken', 'xaccesstoken', 'xrefreshtoken'
  ]);
  const encoder = new TextEncoder();
  const clock = () => global.performance ? global.performance.now() : Date.now();
  const isRecord = value => value !== null && typeof value === 'object' && !Array.isArray(value);
  const sensitiveName = name => credentialNames.has(String(name).replace(/[-_]/g, '').toLowerCase());

  function redactText(value) {
    return String(value)
      .replace(/("(?:authorization|proxy[-_]?authorization|cookie|set[-_]?cookie|(?:x[-_]?)?(?:api[-_]?key|auth[-_]?token|access[-_]?token|refresh[-_]?token)|client[-_]?secret|password|private[-_]?key)"\s*:\s*)"(?:\\.|[^"\\])*"/gi, '$1"[已脱敏]"')
      .replace(/\bBearer[ \t]+[A-Za-z0-9._~+/=-]+/gi, 'Bearer [已脱敏]')
      .replace(/\b(?:sk-|sess-)[A-Za-z0-9_-]{12,}/g, '[已脱敏]')
      .replace(/\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}/g, '[已脱敏]')
      .replace(/([?&](?:api[_-]?key|access[_-]?token|refresh[_-]?token|token|signature|sig|x-amz-signature|x-amz-credential)=)[^&#\s"']*/gi, '$1[已脱敏]');
  }
  function redact(value, options) {
    const settings = options || {};
    const maxString = settings.maxString || Infinity;
    const maxItems = settings.maxItems || Infinity;
    const seen = new WeakSet();
    function visit(item, depth) {
      if (typeof item === 'string') {
        const part = item.length > maxString ? item.slice(0, maxString) : item;
        return redactText(part) + (item.length > maxString ? ' … [预览截断，原长 ' + item.length + ' 字符]' : '');
      }
      if (item === null || typeof item !== 'object') return item;
      if (seen.has(item)) return '[循环引用]';
      if (depth > 64) return '[嵌套过深，详情见响应内容]';
      seen.add(item);
      if (Array.isArray(item)) {
        const items = item.slice(0, maxItems).map(child => visit(child, depth + 1));
        if (item.length > maxItems) items.push('[另有 ' + (item.length - maxItems) + ' 项，预览截断]');
        return items;
      }
      const output = Object.create(null);
      const keys = Object.keys(item);
      for (const key of keys.slice(0, maxItems))
        output[key] = sensitiveName(key) ? '[已脱敏]' : visit(item[key], depth + 1);
      if (keys.length > maxItems) output['[预览截断]'] = '另有 ' + (keys.length - maxItems) + ' 个字段';
      return output;
    }
    return visit(value, 0);
  }
  function preview(value, maximum) {
    const length = maximum || 16000;
    const text = typeof value === 'string'
      ? redactText(value.slice(0, length))
      : JSON.stringify(redact(value, { maxString: 768, maxItems: 80 }), null, 2);
    return String(text).slice(0, length) + (String(text).length > length || typeof value === 'string' && value.length > length
      ? '\n… [仅预览截断；导出保留已捕获内容]' : '');
  }
  function fault(kind, code, message, details) {
    const error = new Error(message);
    error.clientKind = kind;
    error.code = code;
    error.details = details || null;
    return error;
  }
  function safeIdentifier(value) {
    return typeof value === 'string' && value.length <= 512 && !/[\x00-\x1f\x7f]/.test(value)
      ? redactText(value) : null;
  }
  function checkBody(value) {
    if (!isRecord(value)) throw fault('validation', 'invalid_body', 'Responses 请求正文必须是 JSON 对象');
    const seen = new WeakSet();
    function inspect(item, path, depth) {
      if (item === null || typeof item !== 'object') return;
      if (depth > 100) throw fault('validation', 'body_nesting_limit', '请求 JSON 嵌套超过 100 层');
      if (seen.has(item)) return;
      seen.add(item);
      for (const key of Object.keys(item)) {
        if (sensitiveName(key))
          throw fault('validation', 'credential_field', '请求正文不能注入凭据字段 ' + path + key + '；请选择 Client Key ID');
        inspect(item[key], path + key + '.', depth + 1);
      }
    }
    inspect(value, '', 0);
    let text;
    try { text = JSON.stringify(value); }
    catch { throw fault('validation', 'invalid_json', '请求正文不能编码为 JSON'); }
    const bytes = encoder.encode(text).byteLength;
    if (bytes > limits.requestBytes) throw fault('validation', 'request_size_limit', '管理桥请求正文超过 8 MiB');
    return { text, bytes };
  }
  function abortable(promise, signal) {
    return new Promise((resolve, reject) => {
      const abort = () => reject(signal.reason || new DOMException('已取消', 'AbortError'));
      if (signal.aborted) { abort(); return; }
      signal.addEventListener('abort', abort, { once: true });
      Promise.resolve(promise).then(value => {
        signal.removeEventListener('abort', abort);
        resolve(value);
      }, error => {
        signal.removeEventListener('abort', abort);
        reject(error);
      });
    });
  }
  function mimeFromBase64(base64) {
    let head;
    try { head = atob(base64.slice(0, 64)); }
    catch { throw fault('image', 'invalid_base64', '图片结果不是有效 Base64'); }
    const byte = index => head.charCodeAt(index);
    if ([137,80,78,71,13,10,26,10].every((value, index) => byte(index) === value)) return 'image/png';
    if (byte(0) === 255 && byte(1) === 216 && byte(2) === 255) return 'image/jpeg';
    if ([82,73,70,70].every((value, index) => byte(index) === value)
      && [87,69,66,80].every((value, index) => byte(index + 8) === value)) return 'image/webp';
    throw fault('image', 'unknown_image_format', '图片魔数不是 PNG、JPEG 或 WebP');
  }
  async function decodeImage(item, signal) {
    let base64 = item.base64;
    const dataPrefix = /^data:(image\/(?:png|jpeg|webp));base64,/i.exec(base64);
    if (dataPrefix) base64 = base64.slice(dataPrefix[0].length);
    if (/\s/.test(base64)) base64 = base64.replace(/\s/g, '');
    if (!base64 || base64.length > limits.imageCharacters || !/^[A-Za-z0-9+/]*={0,2}$/.test(base64) || base64.length % 4 === 1)
      throw fault('image', 'invalid_image_result', '图片 Base64 无效或超过 160 MiB 字符上限');
    const mimeType = mimeFromBase64(base64);
    if (dataPrefix && dataPrefix[1].toLowerCase() !== mimeType)
      throw fault('image', 'image_mime_mismatch', '图片 data URL 声明与文件格式不一致');
    const dataUrl = 'data:' + mimeType + ';base64,' + base64;
    const image = new Image();
    const dimensions = await abortable(new Promise((resolve, reject) => {
      image.onload = () => image.naturalWidth > 0 && image.naturalHeight > 0
        ? resolve({ width: image.naturalWidth, height: image.naturalHeight })
        : reject(fault('image', 'invalid_image_dimensions', '图片没有有效像素尺寸'));
      image.onerror = () => reject(fault('image', 'image_decode_failed', '浏览器无法解码图片结果'));
      image.src = dataUrl;
    }), signal).finally(() => {
      image.onload = null;
      image.onerror = null;
      if (signal.aborted) image.removeAttribute('src');
    });
    return { itemId: item.itemId, base64, mimeType, dataUrl, width: dimensions.width, height: dimensions.height };
  }

  async function run(options) {
    const input = options || {};
    const startedAtMs = Date.now();
    const started = clock();
    const result = {
      status: 'incomplete', images: [], text: '', requestId: null, responseId: null, httpStatus: null,
      responseModel: null, imageToolObservations: [],
      request: null, headers: [], error: null, warnings: [], eventCount: 0, startedAtMs,
      timings: { headersMs: null, firstByteMs: null, firstEventMs: null, firstImageMs: null, totalMs: 0 },
      capture: input.captureRaw ? { chunks: [], bytes: 0, storedBytes: 0, truncated: false, complete: false } : null
    };
    const controller = new AbortController();
    let reader = null, timer = null, eof = false, timedOut = false, phase = 'validating', terminal = null;
    let bytes = 0, eventIndex = 0, linePieces = [], lineLength = 0, blockLines = [], blockLength = 0, skipLF = false;
    const imageItems = new Map();
    const jsonPieces = [];
    const elapsed = () => Math.max(0, clock() - started);
    function notify(name, value) {
      if (typeof input[name] !== 'function') return;
      try { input[name](value); }
      catch {
        const message = '界面回调 ' + name + ' 失败，响应读取继续';
        if (!result.warnings.includes(message)) result.warnings.push(message);
      }
    }
    function changePhase(next) {
      phase = next;
      notify('onPhase', { phase, elapsedMs: elapsed() });
    }
    function errorRecord(source, kind, fallback) {
      const value = isRecord(source) ? source : {};
      const details = value.details !== undefined ? value.details : value.detail;
      return {
        kind, code: value.code == null ? (value.type == null ? null : redactText(value.type)) : redactText(value.code),
        stage: phase,
        message: redactText(typeof value.message === 'string' ? value.message : fallback),
        details: details == null ? null : redact(details),
        upstreamKind: safeIdentifier(value.kind) || (isRecord(details) ? safeIdentifier(details.kind) : null),
        upstreamStage: safeIdentifier(value.stage) || (isRecord(details) ? safeIdentifier(details.stage) : null)
      };
    }
    function observeImageTool(tool, eventType, source) {
      if (!isRecord(tool) || !['image_generation', 'image_generation_call'].includes(tool.type)) return;
      const parameters = {};
      for (const key of ['model', 'size', 'quality', 'output_format', 'action']) {
        const value = safeIdentifier(tool[key]);
        if (value && value.length <= 128) parameters[key] = value;
      }
      if (typeof tool.output_compression === 'number' && Number.isFinite(tool.output_compression))
        parameters.output_compression = tool.output_compression;
      if (!Object.keys(parameters).length) return;
      const fingerprint = JSON.stringify({ source, ...parameters });
      if (result.imageToolObservations.some(({ eventType: previousType, ...observed }) => JSON.stringify(observed) === fingerprint)) return;
      // 这是服务端响应中的参数回报，不是对其内部最终执行参数的保证。完整原始事件由调试页另行捕获。
      if (result.imageToolObservations.length === 32) result.imageToolObservations.shift();
      result.imageToolObservations.push({ eventType, source, ...parameters });
    }
    function collectItem(item) {
      if (!isRecord(item) || item.type !== 'image_generation_call' || typeof item.result !== 'string' || !item.result) return;
      const itemId = safeIdentifier(item.id);
      const key = itemId ? 'id:' + itemId : 'image:' + item.result;
      imageItems.set(key, { itemId, base64: item.result });
      if (result.timings.firstImageMs === null) result.timings.firstImageMs = elapsed();
    }
    function documentResult(document, type) {
      if (!isRecord(document)) return;
      const responseId = safeIdentifier(document.id);
      if (responseId) result.responseId = responseId;
      const responseModel = safeIdentifier(document.model);
      if (responseModel) result.responseModel = responseModel;
      if (Array.isArray(document.tools))
        document.tools.forEach(tool => observeImageTool(tool, type, 'response.tools'));
      const requestId = safeIdentifier(document.request_id);
      if (!result.requestId && requestId) result.requestId = requestId;
      if (Array.isArray(document.output)) {
        document.output.forEach(collectItem);
        document.output.forEach(item => observeImageTool(item, type, 'response.output'));
        const texts = [];
        for (const item of document.output)
          for (const part of Array.isArray(item && item.content) ? item.content : [])
            if (part && part.type === 'output_text' && typeof part.text === 'string') texts.push(part.text);
        if (texts.length) result.text = texts.join('\n');
      }
      const status = document.status;
      if (document.error) {
        result.error = errorRecord(document.error, 'upstream', '管理桥响应包含错误');
        terminal = 'failed';
      } else if (status === 'failed' || type === 'response.failed') {
        result.error = errorRecord(document.error || document, 'upstream', '管理桥返回 response.failed');
        terminal = 'failed';
      } else if (status === 'cancelled' || status === 'canceled' || type === 'response.cancelled' || type === 'response.canceled') {
        result.error = errorRecord(document.error || document, 'upstream', '模型响应已取消');
        terminal = 'canceled';
      } else if (status === 'incomplete' || type === 'response.incomplete') {
        result.error = errorRecord({ code: 'response_incomplete', message: '模型响应未完整结束', details: document.incomplete_details }, 'upstream', '响应不完整');
        terminal = 'incomplete';
      } else if ((type === 'response.completed' || type === 'json') && status === 'completed' && terminal !== 'failed' && terminal !== 'canceled') {
        terminal = 'completed';
      } else if (type === 'response.completed' && !status) {
        result.error = errorRecord({ code: 'missing_response_status' }, 'protocol', '完成事件缺少 response.status');
        terminal = 'incomplete';
      }
    }
    function eventResult(event, type, raw) {
      eventIndex++;
      result.eventCount = eventIndex;
      if (result.timings.firstEventMs === null) result.timings.firstEventMs = elapsed();
      if (isRecord(event)) {
        if (typeof event.request_id === 'string' && !result.requestId) result.requestId = safeIdentifier(event.request_id);
        if (type === 'response.output_item.done') collectItem(event.item);
        if (isRecord(event.item)) observeImageTool(event.item, type, 'event.item');
        if (type === 'response.output_text.delta' && typeof event.delta === 'string') result.text += event.delta;
        if (isRecord(event.response)) documentResult(event.response, type);
        else if (type === 'json') documentResult(event, type);
        if (type === 'error' || type === 'response.failed' && !event.response) {
          result.error = errorRecord(event.error || event, 'upstream', '管理桥返回错误事件');
          terminal = 'failed';
        }
      }
      notify('onEvent', { index: eventIndex, type, event, raw, elapsedMs: elapsed() });
    }
    function dispatchBlock() {
      if (!blockLines.length) return;
      const lines = blockLines;
      blockLines = [];
      blockLength = 0;
      const raw = lines.join('\n');
      const data = [];
      let name = '';
      for (const line of lines) {
        if (line.startsWith(':')) continue;
        const colon = line.indexOf(':');
        const field = colon < 0 ? line : line.slice(0, colon);
        let value = colon < 0 ? '' : line.slice(colon + 1);
        if (value.startsWith(' ')) value = value.slice(1);
        if (field === 'data') data.push(value);
        if (field === 'event') name = value;
      }
      if (!data.length) { eventResult(null, name || 'comment', raw); return; }
      const payload = data.join('\n');
      if (payload === '[DONE]') { eventResult(null, '[DONE]', raw); return; }
      let event = null;
      try { event = JSON.parse(payload); }
      catch {
        if (!result.warnings.includes('存在无法解析为 JSON 的 SSE 事件，已保留原始内容'))
          result.warnings.push('存在无法解析为 JSON 的 SSE 事件，已保留原始内容');
      }
      eventResult(event, isRecord(event) && typeof event.type === 'string' ? event.type : name || 'message', raw);
    }
    function appendLine(text) {
      if (text) { linePieces.push(text); lineLength += text.length; }
      if (lineLength + blockLength > limits.eventCharacters)
        throw fault('limit', 'event_size_limit', '单个响应事件超过 192 MiB 字符上限');
    }
    function finishLine() {
      const line = linePieces.join('');
      linePieces = [];
      lineLength = 0;
      if (!line) { dispatchBlock(); return; }
      blockLines.push(line);
      blockLength += line.length + 1;
    }
    // 按块保存未完成的长行，只在换行时拼接，避免大 Base64 行逐块重复复制。
    function consumeSse(text) {
      if (!text) return;
      let offset = 0;
      if (skipLF) {
        if (text[0] === '\n') offset = 1;
        skipLF = false;
      }
      while (offset < text.length) {
        const lf = text.indexOf('\n', offset);
        const cr = text.indexOf('\r', offset);
        const end = lf < 0 ? cr : cr < 0 ? lf : Math.min(lf, cr);
        if (end < 0) { appendLine(text.slice(offset)); break; }
        appendLine(text.slice(offset, end));
        finishLine();
        offset = end + 1;
        if (text[end] === '\r') {
          if (text[offset] === '\n') offset++;
          else if (offset === text.length) skipLF = true;
        }
      }
    }
    function storeChunk(text, size) {
      bytes += size;
      if (result.capture) result.capture.bytes = bytes;
      if (bytes > limits.responseBytes) {
        if (result.capture) result.capture.truncated = true;
        throw fault('limit', 'response_size_limit', '响应超过 256 MiB 捕获上限，已取消；导出仅包含已捕获部分');
      }
      if (result.capture) {
        if (text) result.capture.chunks.push(text);
        result.capture.storedBytes += size;
      }
    }
    const externalAbort = () => {
      controller.abort(input.signal && input.signal.reason || new DOMException('用户取消', 'AbortError'));
      if (reader) reader.cancel().catch(() => {});
    };

    try {
      changePhase('validating');
      if (input.signal !== undefined && !(input.signal instanceof AbortSignal))
        throw fault('validation', 'invalid_abort_signal', 'signal 必须是 AbortSignal');
      if (input.signal && input.signal.aborted) { externalAbort(); throw controller.signal.reason; }
      if (input.signal) input.signal.addEventListener('abort', externalAbort, { once: true });
      const clientKeyId = input.clientKeyId;
      if (typeof clientKeyId !== 'string' || !clientKeyId || clientKeyId !== clientKeyId.trim()
        || clientKeyId.length > 128 || /[\x00-\x1f\x7f]/.test(clientKeyId))
        throw fault('validation', 'invalid_client_key_id', '请选择有效的 Client Key ID');
      const encoded = checkBody(input.body);
      result.request = { clientKeyId, bodyText: encoded.text, bytes: encoded.bytes };
      const bridge = input.bridge || global.codexProxyPlugin;
      if (!bridge || bridge.version < 2 || !bridge.models || typeof bridge.models.responses !== 'function')
        throw fault('bridge', 'bridge_unavailable', '宿主没有提供管理页 Responses 模型桥');
      timer = setTimeout(() => {
        timedOut = true;
        controller.abort(new DOMException('模型调用超过 10 分钟', 'TimeoutError'));
        if (reader) reader.cancel().catch(() => {});
      }, limits.timeoutMs);
      changePhase('requesting');
      // 不追加 stream、model、tools 或任何兼容参数；管理桥自行序列化同一个正文。
      const response = await abortable(bridge.models.responses({ clientKeyId, body: input.body, signal: controller.signal }), controller.signal);
      result.httpStatus = response.status;
      result.timings.headersMs = elapsed();
      result.headers = Array.from(response.headers.entries(), ([name, value]) => [name, sensitiveName(name) ? '[已脱敏]' : redactText(value)]);
      for (const name of ['x-request-id', 'x-cpr-request-id', 'request-id', 'openai-request-id', 'x-correlation-id']) {
        const id = safeIdentifier(response.headers.get(name));
        if (id) { result.requestId = id; break; }
      }
      changePhase('headers');
      notify('onHeaders', {
        status: response.status, statusText: redactText(response.statusText || ''),
        headers: result.headers, requestId: result.requestId, elapsedMs: elapsed()
      });
      const streaming = (response.headers.get('content-type') || '').toLowerCase().includes('text/event-stream');
      const decoder = new TextDecoder();
      reader = response.body && response.body.getReader();
      if (!reader) throw fault('protocol', 'missing_response_body', '管理桥响应没有正文');
      changePhase('reading');
      while (true) {
        if (controller.signal.aborted) throw controller.signal.reason;
        const chunk = await abortable(reader.read(), controller.signal);
        if (chunk.done) { eof = true; break; }
        if (!(chunk.value instanceof Uint8Array)) throw fault('protocol', 'invalid_response_chunk', '管理桥返回了无效的字节块');
        if (chunk.value.length && result.timings.firstByteMs === null) result.timings.firstByteMs = elapsed();
        const text = decoder.decode(chunk.value, { stream: true });
        storeChunk(text, chunk.value.byteLength);
        if (streaming) consumeSse(text);
        else jsonPieces.push(text);
      }
      const tail = decoder.decode();
      storeChunk(tail, 0);
      if (result.capture) result.capture.complete = true;
      if (streaming) {
        consumeSse(tail);
        if (linePieces.length) finishLine();
        dispatchBlock();
      } else {
        jsonPieces.push(tail);
        const text = jsonPieces.join('');
        let document;
        try { document = JSON.parse(text); }
        catch {
          eventResult(null, 'unparsed', text);
          throw fault('parse', 'invalid_response_json', '管理桥返回了无法解析的 JSON 正文');
        }
        eventResult(document, 'json', text);
      }
      if (response.status < 200 || response.status >= 300) {
        if (!result.error) result.error = errorRecord({ code: 'http_' + response.status }, 'http', '管理桥返回 HTTP ' + response.status);
        else result.error.kind = 'http';
        terminal = 'failed';
      }
      changePhase('decoding-images');
      for (const item of imageItems.values()) {
        try { result.images.push(await decodeImage(item, controller.signal)); }
        catch (error) {
          if (controller.signal.aborted) throw error;
          if (!result.error) result.error = errorRecord(error, error.clientKind || 'image', error.message || '图片解码失败');
          terminal = 'failed';
        }
      }
      if (!terminal) {
        result.error = result.error || errorRecord({ code: 'missing_completion' }, 'protocol', '响应结束但没有完整的 completed 状态');
        terminal = 'incomplete';
      }
      result.status = terminal;
    } catch (error) {
      if (result.capture && error && error.clientKind === 'limit') result.capture.truncated = true;
      if (timedOut) {
        result.status = 'failed';
        result.error = errorRecord({ code: 'request_timeout', message: '模型调用超过 10 分钟' }, 'timeout', '模型调用超时');
      } else if (controller.signal.aborted) {
        result.status = 'canceled';
        result.error = errorRecord({ code: 'aborted', message: '请求已取消；未自动重试' }, 'canceled', '请求已取消');
      } else {
        result.status = 'failed';
        result.error = errorRecord(error, error && error.clientKind || (phase === 'validating' ? 'validation' : 'bridge'),
          error && error.message || '管理桥调用失败');
      }
    } finally {
      clearTimeout(timer);
      if (input.signal instanceof AbortSignal) input.signal.removeEventListener('abort', externalAbort);
      if (!eof) {
        controller.abort();
        if (reader) { try { await reader.cancel(); } catch {} }
      }
      if (reader) { try { reader.releaseLock(); } catch {} }
      result.timings.totalMs = elapsed();
      changePhase(result.status);
    }
    return result;
  }

  Object.defineProperty(global, 'CPRImageClient', {
    value: Object.freeze({ run, redact, redactText, preview, limits }), configurable: false, writable: false
  });
})(window);
