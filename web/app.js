/* Codex Image Bridge - 插件管理页。所有模型调用通过宿主 models.responses，绝不持有 Key 明文。 */
(function () {
  'use strict';
  const byId = (id) => document.getElementById(id);
  const fields = {
    clientKey: byId('clientKey'), driver: byId('driverModel'), imageModel: byId('imageModel'),
    prompt: byId('prompt'), size: byId('size'), quality: byId('quality'),
    generate: byId('generate'), cancel: byId('cancel'), refresh: byId('refresh'),
    status: byId('status'), notice: byId('validationMessage'),
    preview: byId('preview'), empty: byId('empty'), image: byId('resultImage'),
    save: byId('saveImage'), resultInfo: byId('resultInfo'),
    responseId: byId('responseId'), httpStatus: byId('httpStatus'),
    requestId: byId('requestId'), resultText: byId('resultText'),
    count: byId('promptCount'), presets: byId('presets')
  };
  const presets = [
    { name: '骑车鹈鹕', desc: '卡通 / 可爱', prompt: '一只胖乎乎的白色鹈鹕骑着橙色复古自行车穿过海滨小路，长长的橙色喙，轻快的动态姿势，晴朗天空和零星白云，精致的 2D 动画插画，轮廓清晰，温暖配色，高质量。' },
    { name: '微缩城市', desc: '3D / 等距', prompt: '一个充满生机的微缩城市街区等距视角 3D 渲染，小咖啡馆、街边绿树、行人和有轨电车，柔和清晨阳光，细腻的微型模型材质，干净的画面构图。' },
    { name: '雨夜未来', desc: '电影 / 氛围', prompt: '雨夜的未来都市，宽阔的街道反射橙色与青蓝色的灯光，一辆电车从街角驶过，电影摄影构图，写实光线，层次丰富的薄雾，电影级细节。' },
    { name: '产品摄影', desc: '极简 / 商业', prompt: '高端商业产品摄影：一只磨砂白色陶瓷咖啡杯，杯内精致拿铁拉花，放在暖灰色石材台面上，米色背景，侧面柔和自然光，真实的材质和阴影，极简、干净。' },
    { name: '日系田野', desc: '风景 / 绘本', prompt: '清爽的日系绘本插画，春末的乡间小路，明亮柔和的绿色草地、白色小花和远处的山坡，一座木制小屋，晴空下缓缓飘过的云，宁静温柔，细节丰富。' },
    { name: '橙色图标', desc: 'UI / 设计', prompt: '一枚为创意工具设计的可爱橙色应用图标：抽象星光与画笔组合，圆角几何造型，柔和体积光和细微阴影，精致、辨识度高，无文字，无水印，干净背景。' }
  ];
  let controller = null;
  let selected = 0;
  let configuredDriver = '';
  let supportedKeys = [];
  const host = () => {
    if (!window.codexProxyPlugin || window.codexProxyPlugin.version < 2
      || !window.codexProxyPlugin.models || !window.codexProxyPlugin.request) {
      throw new Error('宿主没有提供插件模型桥，请确认 CPR 和插件版本匹配');
    }
    return window.codexProxyPlugin;
  };
  function theme() {
    const current = window.codexProxyPlugin && window.codexProxyPlugin.theme;
    document.body.classList.toggle('dark', current === 'dark');
  }
  window.addEventListener('codex-proxy-themechange', theme);
  theme();
  function setOptions(select, options, fallback) {
    select.replaceChildren();
    options.forEach(function (option) {
      const node = document.createElement('option');
      node.value = option.value;
      node.textContent = option.label;
      select.appendChild(node);
    });
    if (fallback && options.some((item) => item.value === fallback)) select.value = fallback;
  }
  function status(kind, text) {
    fields.status.className = 'status' + (kind ? ' ' + kind : '');
    fields.status.textContent = text;
  }
  function notice(message, isError) {
    fields.notice.textContent = message || '';
    fields.notice.classList.toggle('error', !!isError);
  }
  function count() {
    fields.count.textContent = fields.prompt.value.length + ' / 10000';
    fields.generate.disabled = !!controller || !fields.clientKey.value || !fields.driver.value
      || !fields.imageModel.value || !fields.prompt.value.trim();
  }
  function presetButtons() {
    fields.presets.replaceChildren();
    presets.forEach(function (item, index) {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'preset' + (selected === index ? ' selected' : '');
      const title = document.createElement('span');
      title.className = 'name';
      title.textContent = item.name;
      const desc = document.createElement('span');
      desc.className = 'desc';
      desc.textContent = item.desc;
      b.append(title, desc);
      b.addEventListener('click', function () {
        selected = index;
        fields.prompt.value = item.prompt;
        presetButtons();
        count();
      });
      fields.presets.appendChild(b);
    });
  }
  async function invoke(method, path, body) {
    const arg = { method: method, path: path };
    if (body !== undefined) {
      arg.body = JSON.stringify(body);
      arg.contentType = 'application/json';
    }
    const response = await host().request(arg);
    const value = JSON.parse(new TextDecoder().decode(response.body));
    if (response.status < 200 || response.status >= 300) {
      throw new Error(value && value.error && value.error.message || '管理接口返回 HTTP ' + response.status);
    }
    return value;
  }
  async function loadModels() {
    if (!fields.clientKey.value) {
      setOptions(fields.driver, [], '');
      count();
      return;
    }
    const previous = fields.driver.value || configuredDriver;
    setOptions(fields.driver, [{ value: '', label: '读取模型中…' }], '');
    const result = await invoke('POST', 'api/models', { clientKeyId: fields.clientKey.value });
    const models = Array.isArray(result.models) ? result.models : [];
    const allowedImages = Array.isArray(result.imageModels) ? result.imageModels : [];
    const selectedImage = fields.imageModel.value || 'gpt-image-2';
    const imageOptions = allowedImages.map((id) => ({value: id, label: id}));
    if (imageOptions.length) {
      setOptions(fields.imageModel, imageOptions, selectedImage);
    } else {
      setOptions(fields.imageModel, [{value:'', label:'该 Key 没有可用的绘图模型'}], '');
      notice('当前 Key 对插件指定的生图账号没有权限，请检查账号和分组设置。', true);
    }
    const chatModels = models.filter((name) => typeof name === 'string'
      && !name.startsWith('gpt-image-') && name.length < 256);
    const options = chatModels.map((name) => ({value: name, label: name}));
    if (options.length === 0) {
      setOptions(fields.driver, [{value: '', label: '该 Key 没有可用的对话模型'}], '');
      notice('该客户端 Key 没有可用的对话主控模型。', true);
      return;
    }
    const preferred = chatModels.includes(previous) ? previous
      : chatModels.includes(configuredDriver) ? configuredDriver
      : chatModels[0];
    setOptions(fields.driver, options, preferred);
    count();
  }
  async function bootstrap() {
    fields.refresh.disabled = true;
    try {
      notice('');
      const state = await invoke('GET', 'api/bootstrap');
      configuredDriver = state.driverModel || 'gpt-5.6-luna';
      supportedKeys = Array.isArray(state.keys) ? state.keys.filter((key) => key.enabled) : [];
      const selectedKey = fields.clientKey.value || '';
      setOptions(fields.clientKey, supportedKeys.map((key) => ({
        value: key.id, label: key.name + ' · ' + key.id
      })), selectedKey);
      if (supportedKeys.length === 0) {
        setOptions(fields.clientKey, [{value: '', label: '没有可用的客户端 Key'}], '');
        notice('请先在 CPR 管理端创建并启用客户端 Key。', true);
      }
      const choices = Array.isArray(state.imageModels) && state.imageModels.length
        ? state.imageModels : ['gpt-image-2'];
      const selectedImage = fields.imageModel.value || 'gpt-image-2';
      setOptions(fields.imageModel, choices.map((id) => ({value: id, label: id})), selectedImage);
      if (!state.enabled) notice('尚未选择启用生图的账号或分组，请先在插件设置中配置。', true);
      await loadModels();
    } catch (error) {
      notice('加载失败：' + (error && error.message || String(error)), true);
      setOptions(fields.clientKey, [{value: '', label: '连接不可用'}], '');
    } finally {
      fields.refresh.disabled = false;
      count();
    }
  }
  function requireImage(base64) {
    if (typeof base64 !== 'string' || base64.length === 0 || base64.length > 32 * 1024 * 1024) {
      throw new Error('生成结果不存在或图片超过 32 MiB 展示上限');
    }
    const prefix = atob(base64.slice(0, 32));
    const magic = [137,80,78,71,13,10,26,10];
    if (!magic.every((byte, index) => prefix.charCodeAt(index) === byte)) {
      throw new Error('上游没有返回合法的 PNG 图片');
    }
    return 'data:image/png;base64,' + base64;
  }
  function imageItem(event) {
    const item = event && event.item;
    if (item && item.type === 'image_generation_call' && typeof item.result === 'string') return item;
    return null;
  }
  function outputImage(document) {
    const output = document && document.output;
    return Array.isArray(output) ? output.find((item) =>
      item && item.type === 'image_generation_call' && typeof item.result === 'string') : null;
  }
  function consumeLineBlock(block, state) {
    const payload = block.split('\n').filter((line) => line.startsWith('data:'))
      .map((line) => line.slice(5).trimStart()).join('\n');
    if (!payload || payload === '[DONE]') return;
    let event;
    try { event = JSON.parse(payload); } catch { return; }
    if (event.type === 'response.output_item.done') {
      const item = imageItem(event);
      if (item) state.image = item;
    } else if (event.type === 'response.completed') {
      state.document = event.response;
      state.image = outputImage(event.response) || state.image;
    } else if (event.type === 'response.output_text.delta' && typeof event.delta === 'string') {
      state.text += event.delta;
    } else if (event.type === 'response.failed') {
      const detail = event.response && event.response.error;
      throw new Error(detail && detail.message || '上游返回 response.failed');
    } else if (event.type === 'error') {
      throw new Error(event.message || event.error && event.error.message || '上游返回错误');
    }
  }
  async function consumeResponse(reply, signal) {
    const state = {image: null, document: null, text: ''};
    const contentType = (reply.headers.get('content-type') || '').toLowerCase();
    if (contentType.includes('application/json')) {
      const document = await reply.json();
      state.document = document;
      state.image = outputImage(document);
      return state;
    }
    const reader = reply.body && reply.body.getReader();
    if (!reader) throw new Error('无法读取 Responses 流');
    const decoder = new TextDecoder();
    let buffer = '';
    try {
      while (true) {
        signal.throwIfAborted();
        const chunk = await reader.read();
        if (chunk.done) break;
        buffer += decoder.decode(chunk.value, {stream:true});
        buffer = buffer.replace(/\r\n/g, '\n');
        if (buffer.length > 48 * 1024 * 1024) throw new Error('模型响应过大，已停止读取');
        let pos = buffer.indexOf('\n\n');
        while (pos >= 0) {
          consumeLineBlock(buffer.slice(0, pos), state);
          buffer = buffer.slice(pos + 2);
          pos = buffer.indexOf('\n\n');
        }
      }
      buffer += decoder.decode();
      if (buffer.trim()) consumeLineBlock(buffer, state);
    } finally { reader.releaseLock(); }
    return state;
  }
  async function generate() {
    if (controller) return;
    const key = fields.clientKey.value;
    const model = fields.driver.value;
    const imageModel = fields.imageModel.value;
    const prompt = fields.prompt.value.trim();
    if (!key || !model || !imageModel || !prompt) return;
    controller = new AbortController();
    fields.generate.disabled = true;
    fields.cancel.disabled = false;
    fields.refresh.disabled = true;
    status('working', '生成中…');
    notice('请求已提交；等待上游绘图结果。');
    fields.httpStatus.textContent = '等待响应';
    fields.requestId.textContent = '—';
    fields.responseId.textContent = '—';
    const started = performance.now();
    try {
      const response = await host().models.responses({
        clientKeyId: key,
        signal: controller.signal,
        body: {
          model: model, stream: true, store: false,
          input: [{ type: 'message', role: 'user', content: [{type: 'input_text', text: prompt}]}],
          instructions: 'Use image_generation to create exactly one image corresponding closely to the user instruction. Preserve its original language and visual constraints.',
          tools: [{
            type: 'image_generation', model: imageModel, action: 'generate',
            size: fields.size.value, quality: fields.quality.value, output_format: 'png'
          }],
          tool_choice: {type: 'image_generation'},
          parallel_tool_calls: false,
          include: ['reasoning.encrypted_content']
        }
      });
      fields.httpStatus.textContent = String(response.status);
      fields.requestId.textContent = response.headers.get('x-codex-imagegen-request-id')
        || response.headers.get('x-gateway-request-id') || response.headers.get('x-request-id') || '—';
      if (!response.ok) {
        const text = await response.text();
        let message = text.slice(0, 600);
        try { const parsed = JSON.parse(text); message = parsed.error && parsed.error.message || message; } catch {}
        throw new Error('HTTP ' + response.status + ': ' + message);
      }
      const parsed = await consumeResponse(response, controller.signal);
      if (!parsed.image || !parsed.image.result) {
        const msg = parsed.text || '上游返回了文字，未执行生图工具';
        throw new Error(msg.slice(0, 500));
      }
      const uri = requireImage(parsed.image.result);
      fields.image.src = uri;
      fields.image.hidden = false;
      fields.empty.hidden = true;
      fields.save.href = uri;
      fields.save.hidden = false;
      fields.responseId.textContent = parsed.document && parsed.document.id || '—';
      fields.resultText.textContent = parsed.text || '—';
      const elapsed = ((performance.now() - started) / 1000).toFixed(1);
      fields.resultInfo.textContent = imageModel + ' · ' + fields.size.value + ' · ' + elapsed + 's';
      status('success', '生成成功');
      notice('图片已完成。可以保存 PNG，或修改提示词继续生成。');
    } catch (error) {
      const aborted = controller.signal.aborted;
      status(aborted ? '' : 'error', aborted ? '已取消' : '生成失败');
      notice(aborted ? '你已取消本次请求。' : (error && error.message || '生成失败'), !aborted);
    } finally {
      controller = null;
      fields.cancel.disabled = true;
      fields.refresh.disabled = false;
      count();
    }
  }
  fields.prompt.addEventListener('input', function () { selected = -1; presetButtons(); count(); });
  [fields.driver, fields.imageModel, fields.size, fields.quality].forEach((el) => el.addEventListener('change', count));
  fields.clientKey.addEventListener('change', function () {
    loadModels().catch((error) => notice('模型列表加载失败：' + error.message, true)).finally(count);
  });
  fields.generate.addEventListener('click', generate);
  fields.cancel.addEventListener('click', function () { if (controller) controller.abort(); });
  fields.refresh.addEventListener('click', bootstrap);
  fields.prompt.value = presets[0].prompt;
  presetButtons();
  count();
  bootstrap();
})();