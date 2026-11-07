/* 原始请求调试：正文和响应仅在本页内存，不保存宿主状态，也不自动重试。 */
(function () {
  'use strict';
  const byId = id => document.getElementById(id);
  const client = window.CPRImageClient;
  const ui = {
    operation: byId('debugOperation'), endpoint: byId('debugEndpoint'), capability: byId('debugCapability'), operationHelp: byId('debugOperationHelp'),
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
  let bootstrap = null, controller = null, latest = null, currentRequest = null, currentHeaders = null, currentTransport = null;
  let elapsedTimer = null, loadingKeys = false, loadingCatalog = false, closed = false, visibleEvents = [];
  let modelCatalog = null, catalogKey = '', catalogRequest = 0, generatedTemplate = '';
  let operation = 'responses';
  const drafts = new Map();
  const templateOptions = {
    responses: [['v051', 'v0.5.1 生图模板'], ['text', '最小文本请求']],
    'images.generate': [['images-json', 'Images 原生 JSON'], ['images-stream', 'Images 流式实验']]
  };
  function target() { return client.operations[operation]; }
  function operationInformation() {
    const current = target();
    const supported = client.supports(operation);
    ui.endpoint.textContent = 'POST ' + current.endpoint;
    ui.capability.textContent = supported ? '宿主已提供 ' + current.bridgeMethod
      : '当前 Core 缺少 ' + current.bridgeMethod + '，需要安装带此管理桥能力的 Core 后运行；正文仍可编辑。';
    ui.capability.classList.toggle('unavailable', !supported);
    ui.operationHelp.textContent = operation === 'images.generate'
      ? '独立 Images 接口：顶层 model / prompt / size，不经过对话模型。默认非流式 JSON；手动填写 stream:true 可实验，当前 Core 可能不支持。'
      : 'Responses 接口：顶层 model 是对话主控，图片模型与尺寸放在 tools 中的 image_generation。两种通路分别保留编辑内容。';
    ui.body.placeholder = operation === 'images.generate' ? '粘贴完整 Images JSON 对象' : '粘贴完整 Responses JSON 对象';
  }
  function switchOperation(next) {
    if (controller || !client.operations[next] || next === operation) return;
    drafts.set(operation, { body: ui.body.value, generatedTemplate, template: ui.template.value, key: ui.key.value });
    operation = next;
    const saved = drafts.get(next);
    ui.template.replaceChildren();
    for (const [value, label] of templateOptions[next]) {
      const option = document.createElement('option');
      option.value = value; option.textContent = label; ui.template.appendChild(option);
    }
    if (saved) ui.template.value = saved.template;
    ui.body.value = saved?.body || '';
    generatedTemplate = saved?.generatedTemplate || '';
    let templateError = '';
    if (!saved || saved.key !== ui.key.value && generatedTemplate && ui.body.value === generatedTemplate) {
      ui.body.value = ''; generatedTemplate = '';
      if (modelCatalog && catalogKey === ui.key.value) {
        try { loadTemplate(); }
        catch (error) { templateError = error.message + ' 仍可手工输入本通路 JSON。'; }
      }
    }
    controls();
    notice(templateError || '已切换至 ' + target().label + '，保留各自编辑稿。' + (client.supports(operation)
      ? '检查正文后点击运行。' : '当前 Core 尚未公开此通路，未发送任何请求。'), !!templateError || !client.supports(operation));
  }
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
    ui.run.disabled = active || loadingKeys || loadingCatalog || !ui.key.value || !ui.body.value.trim() || !client.supports(operation);
    ui.cancel.disabled = !active;
    ui.key.disabled = active || loadingKeys;
    ui.refresh.disabled = active || loadingKeys;
    ui.models.disabled = active || loadingKeys || loadingCatalog || !ui.key.value;
    ui.reset.disabled = active || loadingKeys || loadingCatalog;
    ui.format.disabled = active;
    ui.template.disabled = active;
    ui.operation.disabled = active;
    operationInformation();
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
    if (!modelCatalog || catalogKey !== ui.key.value) throw new Error('请先读取所选 Key 的模型目录。');
    const images = (Array.isArray(modelCatalog.imageModels) ? modelCatalog.imageModels : [])
      .filter(model => typeof model === 'string' && model.startsWith('gpt-image-'));
    const imageModel = ['gpt-image-2.5-sunburst', 'gpt-image-2.6-sunburst'].find(model => images.includes(model)) || images[0];
    const prompt = '一只白色鹈鹕骑着橙色自行车，晴朗的海边小路，精致插画。';
    if (operation === 'images.generate') {
      if (!imageModel) throw new Error('所选 Key 的目录没有图片模型，无法生成 Images 模板。');
      const body = { model: imageModel, prompt, n: 1, size: '1024x1024', quality: 'auto', output_format: 'webp', stream: false };
      if (ui.template.value === 'images-stream') { body.stream = true; body.partial_images = 1; }
      return body;
    }
    const drivers = (Array.isArray(modelCatalog.models) ? modelCatalog.models : [])
      .filter(model => typeof model === 'string' && model && !model.startsWith('gpt-image-'));
    const driver = ['gpt-6-luna', 'gpt-5.6-luna', bootstrap?.driverModel].find(model => drivers.includes(model)) || drivers[0];
    if (!driver) throw new Error('所选 Key 的目录没有可用的对话模型，无法生成 Responses 模板。');
    if (ui.template.value === 'text') return { model: driver, stream: true, store: false, input: '仅回复 OK。' };
    if (!imageModel) throw new Error('所选 Key 的目录没有可用的图片模型。可以选择纯文本模板检查 Responses 通路。');
    return {
      model: driver, stream: true, store: false,
      input: [{ type: 'message', role: 'user', content: [{ type: 'input_text', text: prompt }] }],
      instructions: 'Use image_generation to create exactly one image corresponding closely to the user instruction. Preserve its original language and visual constraints.',
      tools: [{ type: 'image_generation', model: imageModel, action: 'auto', size: '1024x1024', quality: 'auto', output_format: 'png' }],
      tool_choice: 'auto', parallel_tool_calls: false, include: ['reasoning.encrypted_content']
    };
  }
  function loadTemplate() {
    generatedTemplate = JSON.stringify(template(), null, 2);
    ui.body.value = generatedTemplate;
  }
  async function loadCatalog(show = false) {
    const sequence = ++catalogRequest;
    const key = ui.key.value;
    modelCatalog = null;
    catalogKey = '';
    ui.catalog.hidden = true;
    if (!ui.body.value.trim() || ui.body.value === generatedTemplate) {
      ui.body.value = '';
      generatedTemplate = '';
    }
    loadingCatalog = !!key;
    controls();
    if (!key) { notice('宿主没有返回可选的 Client Key。', true); return; }
    notice('正在读取所选 Key 的模型目录…');
    try {
      const catalog = await request('POST', 'api/models', { clientKeyId: key });
      if (closed || sequence !== catalogRequest || key !== ui.key.value) return;
      modelCatalog = catalog;
      catalogKey = key;
      ui.modelList.textContent = JSON.stringify(client.redact(catalog), null, 2);
      if (show) { ui.catalog.hidden = false; ui.catalog.open = true; }
      if (!ui.body.value.trim() || ui.body.value === generatedTemplate) {
        try {
          loadTemplate();
          notice('已按所选 Key 的模型目录载入模板。请检查参数后运行。');
        } catch (error) { notice(error.message + ' 也可以手动输入 JSON 进行调试。', true); }
      } else {
        notice('已读取所选 Key 的模型目录，保留当前手写 JSON。点击“载入模板”可使用该 Key 的可用模型。');
      }
    } catch (error) {
      if (!closed && sequence === catalogRequest)
        notice('读取模型目录失败：' + (error.message || error) + '。可重试读取目录，或保留手写 JSON 运行。', true);
    } finally {
      if (sequence === catalogRequest) { loadingCatalog = false; controls(); }
    }
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
      ui.version.textContent = '插件 ' + safeText(state.pluginVersion || '—') + ' · 管理桥 v' + window.codexProxyPlugin.version;
      await loadCatalog();
    } catch (error) {
      notice('读取 Key 失败：' + (error.message || error), true);
    } finally {
      loadingKeys = false;
      controls();
    }
  }
  function serverParameters(result) {
    const observations = Array.isArray(result?.imageToolObservations) ? result.imageToolObservations : [];
    return {
      label: '服务端参数回报；实际图片尺寸见预览',
      responseModel: result?.responseModel || '未返回',
      imageToolObservations: observations.length ? observations : '未返回'
    };
  }
  function renderSnapshot() {
    ui.snapshot.textContent = JSON.stringify(client.redact({
      submittedTo: currentTransport ? 'CPR 管理桥 ' + currentTransport.transport : '尚未调用管理桥',
      intendedRoute: currentRequest ? client.operations[currentRequest.operation] : null,
      actualInvocation: currentTransport || { bridgeInvoked: false, transport: null, endpoint: null },
      endpointMeaning: 'API 合同路径；不代表最终上游 URL 或上游原始字节',
      request: currentRequest,
      response: currentHeaders,
      serverReportedParameters: serverParameters(latest)
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
      information.textContent = (value.partial ? '部分图片 · 尚未完成 · ' : '') + value.width + ' × ' + value.height + ' · ' + value.mimeType;
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
    renderSnapshot();
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
    renderImages(result.images.length ? result.images : (result.partialImages || []).map(image => ({ ...image, partial: true })));
    const capture = result.capture;
    const complete = capture && capture.complete && !capture.truncated;
    ui.captureState.textContent = (complete ? '响应内容已完整捕获' : '仅捕获部分响应')
      + ' · ' + bytes(capture?.storedBytes || 0) + ' · ' + result.eventCount + ' 个事件'
      + (capture?.truncated ? ' · 已达到捕获上限，导出不是完整响应' : '')
      + (result.eventCount > 300 ? ' · 预览仅展示最近 300 条' : '')
      + (result.warnings.length ? ' · ' + result.warnings.join('；') : '');
    if (result.error?.code === 'no_available_provider') {
      const model = currentRequest?.body?.model;
      const native = currentRequest?.operation === 'images.generate';
      const models = native ? modelCatalog?.imageModels : modelCatalog?.models;
      const listed = catalogKey === currentRequest?.clientKeyId && Array.isArray(models) ? models.includes(model) : null;
      notice(listed === false
        ? '提交的' + (native ? '图片' : '主控') + '模型未出现在所选 Key 的模型目录中。可点击“载入模板”选取可用模型；本次 JSON 已保留。'
        : '宿主未找到可用的 Provider。请核对该 Key 的模型授权、账号状态及额度规则；本次响应尚不能确定具体原因。', true);
    } else {
      notice(result.status === 'completed' ? '本次管理桥响应完整结束。' : '本次状态：' + (stateLabels[result.status] || result.status) + '。可查看错误字段或导出诊断。',
        result.status === 'failed' || result.status === 'incomplete');
    }
  }
  async function runOnce() {
    if (controller || closed) return;
    let body;
    try { body = JSON.parse(ui.body.value); }
    catch (error) { notice('JSON 格式错误：' + error.message, true); return; }
    controller = new AbortController();
    latest = null;
    currentHeaders = null;
    currentTransport = null;
    currentRequest = { operation, clientKeyId: ui.key.value, body };
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
    notice('正在调用 ' + target().bridgeMethod + '；参数按当前 JSON 提交，不会自动重试或切换通路。');
    const began = performance.now();
    elapsedTimer = setInterval(() => { if (!closed) ui.total.textContent = duration(performance.now() - began); }, 100);
    try {
      latest = await client.run({
        operation: currentRequest.operation, clientKeyId: currentRequest.clientKeyId, body, signal: controller.signal, captureRaw: true,
        onPhase: appendPhase,
        onEvent: appendEvent,
        onTransport(value) { currentTransport = value; if (!closed) renderSnapshot(); },
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
      format: 'cpr-management-image-debug-v2',
      operation: latest.operation,
      intendedRoute: client.operations[currentRequest.operation],
      actualInvocation: { bridgeInvoked: latest.bridgeInvoked, transport: latest.transport, endpoint: latest.endpoint },
      endpointMeaning: 'API 合同路径；不代表最终上游 URL 或上游原始字节',
      source: '提交给 CPR 管理桥的 JSON 与管理桥返回的响应；不是上游字节抓包',
      startedAt: new Date(latest.startedAtMs).toISOString(),
      completeCapture: complete,
      truncated: !!capture?.truncated,
      status: latest.status,
      request: client.redact(currentRequest),
      response: { httpStatus: latest.httpStatus, requestId: latest.requestId, responseId: latest.responseId, headers: latest.headers },
      serverReportedParameters: client.redact(serverParameters(latest)),
      timings: latest.timings,
      error: client.redact(latest.error),
      warnings: latest.warnings,
      images: latest.images.map(image => ({ itemId: image.itemId, mimeType: image.mimeType, width: image.width, height: image.height, base64Characters: image.base64.length })),
      partialImages: (latest.partialImages || []).map(image => ({ itemId: image.itemId, mimeType: image.mimeType, width: image.width, height: image.height, base64Characters: image.base64.length })),
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

  ui.operation.addEventListener('change', () => switchOperation(ui.operation.value));
  ui.refresh.addEventListener('click', () => { void refreshKeys(); });
  ui.models.addEventListener('click', () => { if (!controller) void loadCatalog(true); });
  ui.reset.addEventListener('click', () => {
    try {
      loadTemplate();
      notice('已按所选 Key 载入 ' + ui.template.selectedOptions[0].textContent + '。请检查参数后运行。');
    } catch (error) { notice(error.message, true); }
    controls();
  });  ui.format.addEventListener('click', () => {
    try { ui.body.value = JSON.stringify(JSON.parse(ui.body.value), null, 2); controls(); }
    catch (error) { notice('JSON 格式错误：' + error.message, true); }
  });
  ui.body.addEventListener('input', controls);
  ui.key.addEventListener('change', () => { void loadCatalog(); });
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