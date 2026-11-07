/* 原始请求调试：正文和响应仅在本页内存，不保存宿主状态，也不自动重试。 */
(function () {
  'use strict';
  const byId = id => document.getElementById(id);
  const client = window.CPRImageClient;
  const ui = {
    key: byId('debugKey'), refresh: byId('debugRefresh'), models: byId('debugModels'),
    template: byId('debugTemplate'), reset: byId('debugReset'), format: byId('debugFormat'),
    body: byId('debugBody'), bodySize: byId('debugBodySize'), run: byId('debugRun'),
    cancel: byId('debugCancel'), notice: byId('debugNotice'), version: byId('debugVersion'),
    state: byId('debugRunState'), http: byId('debugHttp'), headersTime: byId('debugHeadersTime'),
    firstByte: byId('debugFirstByte'), total: byId('debugTotalTime'),
    requestId: byId('debugRequestId'), responseId: byId('debugResponseId'),
    phases: byId('debugPhases'), error: byId('debugError'), errorTitle: byId('debugErrorTitle'),
    errorDetail: byId('debugErrorDetail'), images: byId('debugImages'), imageHelp: byId('debugImageHelp'),
    snapshot: byId('debugRequestSnapshot'), eventCount: byId('debugEventCount'),
    search: byId('debugEventSearch'), events: byId('debugEvents'), captureState: byId('debugCaptureState'),
    export: byId('debugExport'), exportPanel: byId('debugExportPanel'),
    exportDescription: byId('debugExportDescription'), exportText: byId('debugExportText'),
    copyExport: byId('debugCopyExport'), closeExport: byId('debugCloseExport'),
    copyState: byId('debugCopyState'), catalog: byId('debugCatalog'), modelList: byId('debugModelList')
  };
  const phaseLabels = {
    validating: '校验正文', requesting: '调用管理桥', headers: '收到响应头', reading: '读取响应',
    'decoding-images': '解码图片', completed: '完整结束', incomplete: '未完整结束',
    failed: '调用失败', canceled: '已取消'
  };
  const stateLabels = { completed: '完整结束', incomplete: '未完整结束', failed: '失败', canceled: '已取消' };
  let bootstrap = null, controller = null, latest = null, currentRequest = null, currentHeaders = null;
  let elapsedTimer = null, loadingKeys = false, closed = false, visibleEvents = [];
  const bytes = value => value >= 1024 * 1024 ? (value / 1024 / 1024).toFixed(2) + ' MiB'
    : value >= 1024 ? (value / 1024).toFixed(1) + ' KiB' : value + ' B';
  const duration = value => Number.isFinite(value) ? (value / 1000).toFixed(2) + ' s' : '—';
  function safeText(value) { return client.redactText(value == null ? '' : value); }
  function notice(message, error) {
    if (closed) return;
    ui.notice.textContent = safeText(message);
    ui.notice.classList.toggle('error', !!error);
  }
  function theme() {
    document.documentElement.dataset.theme = window.codexProxyPlugin && window.codexProxyPlugin.theme === 'dark' ? 'dark' : 'light';
  }
  function controls() {
    const active = controller !== null;
    ui.run.disabled = active || !ui.key.value || !ui.body.value.trim() || !window.codexProxyPlugin?.models?.responses;
    ui.cancel.disabled = !active;
    ui.key.disabled = active || loadingKeys;
    ui.refresh.disabled = active || loadingKeys;
    ui.models.disabled = active || !ui.key.value;
    ui.reset.disabled = active;
    ui.format.disabled = active;
    ui.template.disabled = active;
    ui.body.readOnly = active;
    ui.export.disabled = active || !latest;
    ui.bodySize.textContent = bytes(new TextEncoder().encode(ui.body.value).byteLength) + ' / 8 MiB';
  }
  async function request(method, path, body) {
    const bridge = window.codexProxyPlugin;
    if (!bridge || bridge.version < 2 || typeof bridge.request !== 'function') throw new Error('CPR 管理桥不可用');
    const options = { method, path };
    if (body !== undefined) {
      options.contentType = 'application/json';
      options.body = JSON.stringify(body);
    }
    const reply = await bridge.request(options);
    const document = JSON.parse(new TextDecoder().decode(reply.body));
    if (reply.status < 200 || reply.status >= 300)
      throw new Error(document?.error?.message || '管理接口返回 HTTP ' + reply.status);
    return document;
  }
  function template() {
    const driver = bootstrap?.driverModel || 'gpt-5.6-luna';
    const configured = Array.isArray(bootstrap?.imageModels) ? bootstrap.imageModels : [];
    const imageModel = configured.includes('gpt-image-2.5-sunburst') ? 'gpt-image-2.5-sunburst'
      : configured[0] || 'gpt-image-2.5-sunburst';
    if (ui.template.value === 'text') return { model: driver, stream: true, store: false, input: '仅回复 OK。' };
    return {
      model: driver, stream: true, store: false,
      input: [{ type: 'message', role: 'user', content: [{ type: 'input_text', text: '一只白色鹈鹕骑着橙色自行车，晴朗的海边小路，精致插画。' }] }],
      instructions: 'Use image_generation to create exactly one image corresponding closely to the user instruction. Preserve its original language and visual constraints.',
      tools: [{ type: 'image_generation', model: imageModel, action: 'auto', size: '1024x1024', quality: 'auto', output_format: 'png' }],
      tool_choice: 'auto', parallel_tool_calls: false, include: ['reasoning.encrypted_content']
    };
  }
  async function refreshKeys() {
    if (loadingKeys || controller) return;
    loadingKeys = true;
    controls();
    try {
      const state = await request('GET', 'api/bootstrap');
      if (closed) return;
      bootstrap = state;
      const old = ui.key.value;
      const keys = (Array.isArray(state.keys) ? state.keys : []).filter(key => key && typeof key.id === 'string' && key.enabled !== false);
      ui.key.replaceChildren();
      for (const key of keys) {
        const option = document.createElement('option');
        option.value = key.id;
        option.textContent = (key.name || 'Client Key') + ' · ' + key.id;
        ui.key.appendChild(option);
      }
      if (!keys.length) {
        const option = document.createElement('option');
        option.value = '';
        option.textContent = '没有已启用的 Client Key';
        ui.key.appendChild(option);
      } else if (keys.some(key => key.id === old)) ui.key.value = old;
      if (!ui.body.value.trim()) ui.body.value = JSON.stringify(template(), null, 2);
      ui.version.textContent = '插件 ' + safeText(state.pluginVersion || '—') + ' · 管理桥 v' + window.codexProxyPlugin.version;
      notice(keys.length ? '已读取 ' + keys.length + ' 个 Client Key。编辑 JSON 后点击“运行一次”。'
        + (state.keysNextCursor ? ' 当前为宿主返回的第一页 Key。' : '') : '宿主没有返回可选的 Client Key。', !keys.length);
    } catch (error) {
      if (!ui.body.value.trim()) ui.body.value = JSON.stringify(template(), null, 2);
      notice('读取 Key 失败：' + (error.message || error), true);
    } finally {
      loadingKeys = false;
      controls();
    }
  }
  function renderSnapshot() {
    ui.snapshot.textContent = JSON.stringify(client.redact({
      submittedTo: 'CPR 管理桥 models.responses',
      request: currentRequest,
      response: currentHeaders
    }, { maxString: 16000, maxItems: 100 }), null, 2);
  }
  function appendPhase(value) {
    if (closed) return;
    const node = document.createElement('li');
    node.appendChild(document.createTextNode((phaseLabels[value.phase] || value.phase) + ' '));
    const time = document.createElement('span');
    time.textContent = duration(value.elapsedMs);
    node.appendChild(time);
    ui.phases.appendChild(node);
  }
  function filterEvents() {
    const query = ui.search.value.trim().toLowerCase();
    for (const item of visibleEvents) item.node.hidden = !!query && !item.search.includes(query);
  }
  function appendEvent(value) {
    if (closed) return;
    if (!visibleEvents.length) ui.events.replaceChildren();
    const type = safeText(value.type);
    const text = value.event === null ? client.preview(value.raw) : client.preview(value.event);
    const node = document.createElement('details');
    node.className = 'event-item';
    node.dataset.eventType = type;
    node.open = type === 'error' || type.includes('failed');
    const summary = document.createElement('summary');
    for (const [className, content] of [
      ['event-number', '#' + value.index], ['event-type', type], ['event-time', '+' + duration(value.elapsedMs)]
    ]) {
      const label = document.createElement('span');
      label.className = className;
      label.textContent = content;
      summary.appendChild(label);
    }
    const preview = document.createElement('pre');
    preview.textContent = text;
    node.append(summary, preview);
    ui.events.appendChild(node);
    visibleEvents.push({ node, search: (type + '\n' + text).toLowerCase() });
    if (visibleEvents.length > 300) visibleEvents.shift().node.remove();
    ui.eventCount.textContent = String(value.index);
    ui.captureState.textContent = value.index > 300
      ? '仅展示最近 300 条事件预览；导出仍包含已捕获的完整响应内容。'
      : '正在捕获响应，预览中的 Base64 截断不改变捕获内容。';
    filterEvents();
  }
  function openExport(text, description) {
    ui.exportText.value = text;
    ui.exportDescription.textContent = description;
    ui.exportPanel.hidden = false;
    ui.copyState.textContent = '点击全选并复制，或在文本框内使用 Ctrl+A / Ctrl+C 保存到本地。';
    ui.exportPanel.scrollIntoView({ behavior: 'auto', block: 'nearest' });
  }
  function renderImages(images) {
    ui.images.replaceChildren();
    ui.imageHelp.hidden = !images.length;
    images.forEach((value, index) => {
      const figure = document.createElement('figure');
      const image = document.createElement('img');
      image.src = value.dataUrl;
      image.alt = '管理桥返回的第 ' + (index + 1) + ' 张图片';
      const caption = document.createElement('figcaption');
      const information = document.createElement('span');
      information.textContent = value.width + ' × ' + value.height + ' · ' + value.mimeType;
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'text-button';
      button.textContent = '显示完整 Data URL';
      button.addEventListener('click', () => openExport(value.dataUrl,
        '第 ' + (index + 1) + ' 张图片的完整 Data URL，格式由图片魔数和浏览器解码确认。'));
      caption.append(information, button);
      figure.append(image, caption);
      ui.images.appendChild(figure);
    });
  }
  function renderResult(result) {
    ui.state.textContent = stateLabels[result.status] || result.status;
    ui.state.className = 'state-badge ' + result.status;
    ui.http.textContent = result.httpStatus === null ? '未收到' : String(result.httpStatus);
    ui.headersTime.textContent = duration(result.timings.headersMs);
    ui.firstByte.textContent = duration(result.timings.firstByteMs);
    ui.total.textContent = duration(result.timings.totalMs);
    ui.requestId.textContent = result.requestId || '未返回请求 ID';
    ui.responseId.textContent = result.responseId || '未返回响应 ID';
    ui.error.hidden = !result.error;
    if (result.error) {
      ui.errorTitle.textContent = safeText(result.error.message);
      ui.errorDetail.textContent = JSON.stringify(client.redact({
        kind: result.error.kind, code: result.error.code, stage: result.error.stage,
        httpStatus: result.httpStatus, upstreamKind: result.error.upstreamKind,
        upstreamStage: result.error.upstreamStage, details: result.error.details
      }, { maxString: 8000, maxItems: 100 }), null, 2);
    }
    renderImages(result.images);
    const capture = result.capture;
    const complete = capture && capture.complete && !capture.truncated;
    ui.captureState.textContent = (complete ? '响应内容已完整捕获' : '仅捕获部分响应')
      + ' · ' + bytes(capture?.storedBytes || 0) + ' · ' + result.eventCount + ' 个事件'
      + (capture?.truncated ? ' · 已达到捕获上限，导出不是完整响应' : '')
      + (result.eventCount > 300 ? ' · 预览仅展示最近 300 条' : '')
      + (result.warnings.length ? ' · ' + result.warnings.join('；') : '');
    notice(result.status === 'completed' ? '本次管理桥响应完整结束。' : '本次状态：' + (stateLabels[result.status] || result.status) + '。可查看错误字段或导出诊断。',
      result.status === 'failed' || result.status === 'incomplete');
  }
  async function runOnce() {
    if (controller || closed) return;
    let body;
    try { body = JSON.parse(ui.body.value); }
    catch (error) { notice('JSON 格式错误：' + error.message, true); return; }
    controller = new AbortController();
    latest = null;
    currentHeaders = null;
    currentRequest = { clientKeyId: ui.key.value, body };
    visibleEvents = [];
    ui.events.replaceChildren();
    ui.eventCount.textContent = '0';
    ui.phases.replaceChildren();
    ui.error.hidden = true;
    ui.images.replaceChildren();
    ui.imageHelp.hidden = true;
    ui.exportPanel.hidden = true;
    ui.exportText.value = '';
    ui.captureState.textContent = '等待管理桥响应…';
    ui.http.textContent = '等待响应';
    ui.headersTime.textContent = '—';
    ui.firstByte.textContent = '—';
    ui.requestId.textContent = '尚未收到';
    ui.responseId.textContent = '尚未收到';
    ui.state.textContent = '运行中';
    ui.state.className = 'state-badge running';
    renderSnapshot();
    controls();
    notice('本次请求正在执行；参数已按当前 JSON 提交，不会自动重试。');
    const began = performance.now();
    elapsedTimer = setInterval(() => { if (!closed) ui.total.textContent = duration(performance.now() - began); }, 100);
    try {
      latest = await client.run({
        clientKeyId: currentRequest.clientKeyId, body, signal: controller.signal, captureRaw: true,
        onPhase: appendPhase,
        onEvent: appendEvent,
        onHeaders(value) {
          if (closed) return;
          currentHeaders = value;
          ui.http.textContent = String(value.status);
          ui.headersTime.textContent = duration(value.elapsedMs);
          ui.requestId.textContent = value.requestId || '未返回请求 ID';
          renderSnapshot();
        }
      });
      if (!closed) renderResult(latest);
    } catch (error) {
      notice('调试界面处理失败：' + (error.message || error), true);
      ui.state.textContent = '界面错误';
      ui.state.className = 'state-badge failed';
    } finally {
      clearInterval(elapsedTimer);
      elapsedTimer = null;
      controller = null;
      controls();
    }
  }
  function exportRun() {
    if (!latest || controller) return;
    const capture = latest.capture;
    const complete = !!capture?.complete && !capture?.truncated;
    const document = {
      format: 'cpr-management-responses-debug-v1',
      source: '提交给 CPR 管理桥的 JSON 与管理桥返回的响应；不是上游字节抓包',
      startedAt: new Date(latest.startedAtMs).toISOString(),
      completeCapture: complete,
      truncated: !!capture?.truncated,
      status: latest.status,
      request: client.redact(currentRequest),
      response: { httpStatus: latest.httpStatus, requestId: latest.requestId, responseId: latest.responseId, headers: latest.headers },
      timings: latest.timings,
      error: client.redact(latest.error),
      warnings: latest.warnings,
      images: latest.images.map(image => ({ itemId: image.itemId, mimeType: image.mimeType, width: image.width, height: image.height, base64Characters: image.base64.length })),
      capturedBytes: capture?.storedBytes || 0,
      rawResponse: client.redactText((capture?.chunks || []).join(''))
    };
    openExport(JSON.stringify(document, null, 2), (complete ? '完整捕获内容' : '部分捕获内容')
      + '，包含未做预览截断的事件和图片 Base64；凭据字段及凭据形态字符串已脱敏。');
  }
  async function copyExport() {
    ui.exportText.focus();
    ui.exportText.select();
    let copied = false;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(ui.exportText.value);
        copied = true;
      }
    } catch {}
    if (!copied) {
      try { copied = document.execCommand('copy'); } catch {}
    }
    ui.copyState.textContent = copied ? '已复制。可粘贴到本地文件保存。' : '文本已全选，请按 Ctrl+C（macOS 使用 Command+C）复制。';
  }

  ui.refresh.addEventListener('click', () => { void refreshKeys(); });
  ui.models.addEventListener('click', async () => {
    if (!ui.key.value || controller) return;
    ui.models.disabled = true;
    try {
      const catalog = await request('POST', 'api/models', { clientKeyId: ui.key.value });
      if (closed) return;
      ui.catalog.hidden = false;
      ui.catalog.open = true;
      ui.modelList.textContent = JSON.stringify(client.redact(catalog), null, 2);
    } catch (error) { notice('读取模型目录失败：' + (error.message || error), true); }
    finally { controls(); }
  });
  ui.reset.addEventListener('click', () => {
    ui.body.value = JSON.stringify(template(), null, 2);
    notice('已载入 ' + ui.template.selectedOptions[0].textContent + '。请检查参数后运行。');
    controls();
  });
  ui.format.addEventListener('click', () => {
    try { ui.body.value = JSON.stringify(JSON.parse(ui.body.value), null, 2); controls(); }
    catch (error) { notice('JSON 格式错误：' + error.message, true); }
  });
  ui.body.addEventListener('input', controls);
  ui.key.addEventListener('change', () => { ui.catalog.hidden = true; controls(); });
  ui.search.addEventListener('input', filterEvents);
  ui.run.addEventListener('click', () => { void runOnce(); });
  ui.cancel.addEventListener('click', () => {
    if (controller) { notice('正在取消本次请求…'); controller.abort(); }
  });
  ui.export.addEventListener('click', exportRun);
  ui.copyExport.addEventListener('click', () => { void copyExport(); });
  ui.closeExport.addEventListener('click', () => { ui.exportPanel.hidden = true; ui.exportText.value = ''; });
  window.addEventListener('codex-proxy-themechange', theme);
  window.addEventListener('pagehide', () => {
    closed = true;
    if (controller) controller.abort();
    clearInterval(elapsedTimer);
  }, { once: true });
  theme();
  controls();
  void refreshKeys();
})();
