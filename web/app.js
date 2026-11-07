/* Codex Image Bridge - 插件管理页。所有模型调用通过宿主 models.responses，绝不持有 Key 明文。 */
(function () {
  'use strict';
  const byId = (id) => document.getElementById(id);
  const fields = {
    clientKey: byId('clientKey'), driver: byId('driverModel'), imageModel: byId('imageModel'),
    prompt: byId('prompt'), size: byId('size'), quality: byId('quality'),
    batchCount: byId('batchCount'), parallelism: byId('parallelism'),
    generate: byId('generate'), cancel: byId('cancel'), refresh: byId('refresh'),
    status: byId('status'), notice: byId('validationMessage'),
    taskGrid: byId('taskGrid'), queueInfo: byId('queueInfo'),
    clearFinished: byId('clearFinished'), empty: byId('empty'),
    count: byId('promptCount'), presets: byId('presets')
  };
  const configFields = {
    accounts: byId('configAccounts'), groups: byId('configGroups'), models: byId('configModels'),
    accountsCount: byId('configAccountsCount'), groupsCount: byId('configGroupsCount'),
    modelsCount: byId('configModelsCount'),
    accountExtra: byId('configAccountExtra'), groupExtra: byId('configGroupExtra'),
    modelNew: byId('configModelNew'), modelAdd: byId('configModelAdd'),
    driver: byId('configDriver'), mode: byId('configMode'),
    backendConcurrency: byId('configBackendConcurrency'),
    timeout: byId('configTimeout'), json: byId('configJson'),
    copy: byId('copyConfig'), reset: byId('resetConfig'), notice: byId('configNotice')
  };
  const builderState = {
    accounts: [], groups: [], candidates: [],
    selectedAccounts: new Set(), selectedGroups: new Set(), selectedModels: new Set(),
    saved: null, accountOptionsError: false, truncated: false
  };
  const isImageModel = (model) => /^gpt-image-[A-Za-z0-9_.-]+$/.test(model) && model.length <= 128;
  function parseIds(value, field) {
    const ids = value.trim() ? value.trim().split(/[\s,;]+/).filter(Boolean) : [];
    if (ids.length > 256 || ids.some((id) => id.length > 128 || /[\x00-\x1f\x7f]/.test(id))) {
      throw new Error(field + ' ID 格式错误或数量超过 256');
    }
    return ids;
  }
  function unique(values) {
    return [...new Set(values)];
  }
  function makeOption(container, title, detail, selected, onChange, disabled) {
    const row = document.createElement('label');
    row.className = 'option-item' + (selected ? ' checked' : '');
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.checked = selected;
    input.disabled = !!disabled;
    const display = document.createElement('span');
    const label = document.createElement('span');
    label.className = 'option-title' + (disabled ? ' disabled' : '');
    label.textContent = title;
    display.appendChild(label);
    if (detail) {
      const sub = document.createElement('span');
      sub.className = 'option-secondary';
      sub.textContent = detail;
      display.appendChild(sub);
    }
    row.append(input, display);
    input.addEventListener('change', () => {
      row.classList.toggle('checked', input.checked);
      onChange(input.checked);
    });
    container.appendChild(row);
  }
  function renderAccountOptions() {
    configFields.accounts.replaceChildren();
    const selected = builderState.selectedAccounts;
    const available = builderState.accounts;
    const candidates = available.map((item) => item.id);
    const missing = [...selected].filter((id) => !candidates.includes(id));
    if (!available.length && !missing.length) {
      const empty = document.createElement('div');
      empty.className = 'placeholder';
      empty.textContent = builderState.accountOptionsError
        ? '账号目录不可用；请在下方手动输入账号 ID'
        : '没有可选的 OpenAI 账号；可手动输入 ID';
      configFields.accounts.appendChild(empty);
    }
    available.forEach((account) => {
      const email = typeof account.email === 'string' ? account.email.trim() : '';
      const accountName = account.name.trim();
      const title = email || (accountName && accountName !== account.id ? accountName : account.id);
      const details = [
        accountName && accountName !== title && accountName !== account.id ? accountName : '',
        'ID ' + account.id,
        account.groupIds && account.groupIds.length ? '分组 ' + account.groupIds.join(', ') : '',
        account.enabled ? '' : '已停用'
      ].filter(Boolean).join(' · ');
      makeOption(configFields.accounts, title, details,
        selected.has(account.id), (checked) => {
          if (checked) selected.add(account.id);
          else selected.delete(account.id);
          refreshConfigJson();
        }, !account.enabled);
    });
    missing.forEach((id) => {
      makeOption(configFields.accounts, id, '原配置账号（当前列表未找到）', true, (checked) => {
        if (!checked) selected.delete(id);
        refreshConfigJson();
      });
    });
  }
  function renderGroupOptions() {
    configFields.groups.replaceChildren();
    const selected = builderState.selectedGroups;
    const ids = unique([...builderState.groups, ...selected]);
    if (!ids.length) {
      const empty = document.createElement('div');
      empty.className = 'placeholder';
      empty.textContent = '暂无分组信息；可以在下方输入分组 ID';
      configFields.groups.appendChild(empty);
    }
    ids.forEach((id) => makeOption(configFields.groups, id,
      '分组 ID · 与账号白名单取并集', selected.has(id), (checked) => {
        if (checked) selected.add(id);
        else selected.delete(id);
        refreshConfigJson();
      }));
  }
  function renderModelOptions() {
    configFields.models.replaceChildren();
    const selected = builderState.selectedModels;
    const choices = unique([...builderState.candidates, ...selected]);
    choices.forEach((id) => {
      const item = document.createElement('label');
      item.className = 'model-chip' + (selected.has(id) ? ' active' : '');
      const checkbox = document.createElement('input');
      checkbox.type = 'checkbox';
      checkbox.checked = selected.has(id);
      const text = document.createElement('span');
      text.textContent = id;
      item.append(checkbox, text);
      checkbox.addEventListener('change', () => {
        if (checkbox.checked && selected.size >= 32) {
          checkbox.checked = false;
          configFields.notice.textContent = '最多可配置 32 个绘图模型';
          configFields.notice.classList.add('error');
          return;
        }
        if (checkbox.checked) selected.add(id);
        else selected.delete(id);
        item.classList.toggle('active', checkbox.checked);
        refreshConfigJson();
      });
      configFields.models.appendChild(item);
    });
  }
  function currentConfig() {
    const imageModels = [...builderState.selectedModels];
    const accounts = unique([...builderState.selectedAccounts, ...parseIds(configFields.accountExtra.value, '账号')]);
    const groups = unique([...builderState.selectedGroups, ...parseIds(configFields.groupExtra.value, '分组')]);
    const driverModel = configFields.driver.value.trim();
    if (!driverModel || !/^[A-Za-z0-9_.\/-]+$/.test(driverModel)
        || driverModel.length > 128 || driverModel.split('/').some((segment) => !segment || segment === '.' || segment === '..')) {
      throw new Error('请输入合法的对话主控模型 ID');
    }
    if (imageModels.length > 32 || imageModels.some((id) => !isImageModel(id))) {
      throw new Error('绘图模型数量或 ID 格式错误');
    }
    if (accounts.length > 256 || groups.length > 256) {
      throw new Error('账号或分组数量超过上限 256');
    }
    return {
      ...builderState.saved,
      timeoutMs: Number(configFields.timeout.value),
      bridgeMode: configFields.mode.value,
      driverModel,
      imageModels,
      imageGroupIds: groups,
      imageAccountIds: accounts,
      concurrencyPerAccount: Number(configFields.backendConcurrency.value),
      quotaGuard: Array.isArray(builderState.saved?.quotaGuard) ? builderState.saved.quotaGuard : [],
      maxObservationAgeSeconds: Number(builderState.saved?.maxObservationAgeSeconds || 300)
    };
  }
  function refreshConfigJson() {
    try {
      const payload = currentConfig();
      configFields.json.value = JSON.stringify(payload, null, 2);
      configFields.accountsCount.textContent = payload.imageAccountIds.length + ' 个';
      configFields.groupsCount.textContent = payload.imageGroupIds.length + ' 个';
      configFields.modelsCount.textContent = payload.imageModels.length + ' 个';
      configFields.copy.disabled = false;
      const warning = payload.imageModels.length === 0 ? '未选择绘图模型，保存后不会向外提供生图模型'
        : payload.imageAccountIds.length === 0 && payload.imageGroupIds.length === 0
          ? '账号和分组都为空：保存后生图仍处于禁用状态'
          : '配置已生成。复制到「插件设置 → 插件参数」保存，随后刷新此页面。';
      configFields.notice.textContent = warning;
      configFields.notice.classList.toggle('error', payload.imageModels.length === 0
        || payload.imageAccountIds.length === 0 && payload.imageGroupIds.length === 0);
    } catch (error) {
      configFields.json.value = '';
      configFields.copy.disabled = true;
      configFields.notice.textContent = error && error.message || '配置无效';
      configFields.notice.classList.add('error');
    }
  }
  function setConfigState(config) {
    builderState.selectedAccounts = new Set(Array.isArray(config.imageAccountIds) ? config.imageAccountIds : []);
    builderState.selectedGroups = new Set(Array.isArray(config.imageGroupIds) ? config.imageGroupIds : []);
    builderState.selectedModels = new Set(Array.isArray(config.imageModels) ? config.imageModels : builderState.candidates);
    configFields.accountExtra.value = '';
    configFields.groupExtra.value = '';
    configFields.driver.value = config.driverModel || 'gpt-5.6-luna';
    configFields.mode.value = config.bridgeMode === 'responses' ? 'responses' : 'auto';
    configFields.backendConcurrency.value = String(config.concurrencyPerAccount || 8);
    const timeout = String(config.timeoutMs || 300000);
    configFields.timeout.value = [...configFields.timeout.options].some((option) => option.value === timeout)
      ? timeout : '300000';
    renderAccountOptions();
    renderGroupOptions();
    renderModelOptions();
    refreshConfigJson();
  }
  function loadConfigBuilder(state) {
    const config = state.configuration || {
      timeoutMs: 300000, bridgeMode: 'auto', driverModel: 'gpt-5.6-luna',
      imageModels: ['gpt-image-2'], imageAccountIds: [], imageGroupIds: []
    };
    builderState.saved = JSON.parse(JSON.stringify(config));
    builderState.accounts = (Array.isArray(state.accountOptions) ? state.accountOptions : [])
      .filter((account) => account && typeof account.id === 'string'
        && typeof account.name === 'string' && Array.isArray(account.groupIds));
    builderState.groups = (Array.isArray(state.groupOptions) ? state.groupOptions : [])
      .filter((group) => typeof group === 'string');
    builderState.candidates = unique(
      (Array.isArray(state.candidateImageModels) ? state.candidateImageModels : ['gpt-image-2'])
      .filter((id) => typeof id === 'string' && isImageModel(id))
    );
    builderState.accountOptionsError = !!state.accountOptionsError;
    builderState.truncated = !!state.accountOptionsTruncated;
    setConfigState(builderState.saved);
    if (builderState.truncated) {
      configFields.notice.textContent += ' · 账号较多，仅显示前 600 个，可手动补充账号 ID。';
    } else if (builderState.accountOptionsError) {
      configFields.notice.textContent += ' · 无法自动读取账号目录，可手动填写 ID。';
    }
  }
  function addCustomModel() {
    const id = configFields.modelNew.value.trim();
    if (!isImageModel(id)) {
      configFields.notice.textContent = '自定义绘图模型必须以 gpt-image- 开头，且只含字母、数字、点、横线、下划线（最长 128 字符）';
      configFields.notice.classList.add('error');
      return;
    }
    if (!builderState.selectedModels.has(id) && builderState.selectedModels.size >= 32) {
      configFields.notice.textContent = '最多 32 个绘图模型';
      configFields.notice.classList.add('error');
      return;
    }
    if (!builderState.candidates.includes(id)) builderState.candidates.push(id);
    builderState.selectedModels.add(id);
    configFields.modelNew.value = '';
    renderModelOptions();
    refreshConfigJson();
  }
  async function copyConfig() {
    if (!configFields.json.value) return;
    const text = configFields.json.value;
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        await navigator.clipboard.writeText(text);
      } else {
        configFields.json.focus();
        configFields.json.select();
        if (!document.execCommand || !document.execCommand('copy')) throw new Error('copy unavailable');
      }
      configFields.notice.textContent = '已复制配置 JSON。请到「插件设置 → 插件参数」粘贴并保存，保存后再刷新画图测试页面。';
      configFields.notice.classList.remove('error');
    } catch {
      configFields.json.focus();
      configFields.json.select();
      configFields.notice.textContent = '自动复制被浏览器阻止；已选中 JSON，请按 Ctrl+C / ⌘C 复制。';
      configFields.notice.classList.add('error');
    }
  }
  [configFields.accountExtra, configFields.groupExtra, configFields.driver].forEach((input) =>
    input.addEventListener('input', refreshConfigJson));
  [configFields.mode, configFields.timeout, configFields.backendConcurrency].forEach((select) =>
    select.addEventListener('change', refreshConfigJson));
  configFields.modelAdd.addEventListener('click', addCustomModel);
  configFields.modelNew.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') { event.preventDefault(); addCustomModel(); }
  });
  configFields.copy.addEventListener('click', copyConfig);
  configFields.reset.addEventListener('click', () => {
    if (builderState.saved) setConfigState(builderState.saved);
  });

  const presets = [
    { name: '骑车鹈鹕', desc: '卡通 / 可爱', prompt: '一只胖乎乎的白色鹈鹕骑着橙色复古自行车穿过海滨小路，长长的橙色喙，轻快的动态姿势，晴朗天空和零星白云，精致的 2D 动画插画，轮廓清晰，温暖配色，高质量。' },
    { name: '微缩城市', desc: '3D / 等距', prompt: '一个充满生机的微缩城市街区等距视角 3D 渲染，小咖啡馆、街边绿树、行人和有轨电车，柔和清晨阳光，细腻的微型模型材质，干净的画面构图。' },
    { name: '雨夜未来', desc: '电影 / 氛围', prompt: '雨夜的未来都市，宽阔的街道反射橙色与青蓝色的灯光，一辆电车从街角驶过，电影摄影构图，写实光线，层次丰富的薄雾，电影级细节。' },
    { name: '产品摄影', desc: '极简 / 商业', prompt: '高端商业产品摄影：一只磨砂白色陶瓷咖啡杯，杯内精致拿铁拉花，放在暖灰色石材台面上，米色背景，侧面柔和自然光，真实的材质和阴影，极简、干净。' },
    { name: '日系田野', desc: '风景 / 绘本', prompt: '清爽的日系绘本插画，春末的乡间小路，明亮柔和的绿色草地、白色小花和远处的山坡，一座木制小屋，晴空下缓缓飘过的云，宁静温柔，细节丰富。' },
    { name: '橙色图标', desc: 'UI / 设计', prompt: '一枚为创意工具设计的可爱橙色应用图标：抽象星光与画笔组合，圆角几何造型，柔和体积光和细微阴影，精致、辨识度高，无文字，无水印，干净背景。' }
  ];
  let selected = 0;
  let configuredDriver = '';
  let supportedKeys = [];
  let modelLists = new Map();
  let eligibleKeys = [];
  let selectedKeyTouched = false;
  let preferredKeyId = '';
  let scanEpoch = 0;
  let storedSelection = null;
  let stateVersion = null;
  let preferencesReady = false;
  let preferenceDirty = null;
  let preferenceSaving = false;
  let preferenceTimer = null;
  let activeTasks = 0;
  let taskCounter = 0;
  let tasks = [];
  const MAX_TASKS = 12;
  const MAX_PARALLEL = 4;
  let closed = false;
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
  function liveTaskCount() {
    return tasks.filter((t)=>t.status==='queued'||t.status==='running').length;
  }
  function count() {
    fields.count.textContent=fields.prompt.value.length+' / 10000';
    const number=Number(fields.batchCount.value)||1;
    fields.generate.disabled=closed||!fields.clientKey.value||!fields.driver.value||!fields.imageModel.value
      ||!fields.prompt.value.trim()||fields.prompt.value.length>10000
      ||!bridgeReady||!Number.isInteger(number)||number<1||number>10
      ||tasks.length+number>MAX_TASKS;
    fields.cancel.disabled=liveTaskCount()===0;
    fields.clearFinished.disabled=!tasks.some((t)=>!['queued','running'].includes(t.status));
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
  function applyRememberedSettings() {
    const saved = storedSelection;
    if (!saved) return;
    for (const [element, value] of [
      [fields.size, saved.size], [fields.quality, saved.quality],
      [fields.batchCount, String(saved.count)], [fields.parallelism, String(saved.parallelism)]
    ]) {
      if (typeof value === 'string' && [...element.options].some((option) => option.value === value)) {
        element.value = value;
      }
    }
  }
  function preferenceValue() {
    return {
      clientKeyId: fields.clientKey.value,
      driverModel: fields.driver.value,
      imageModel: fields.imageModel.value,
      size: fields.size.value,
      quality: fields.quality.value,
      count: Number(fields.batchCount.value) || 1,
      parallelism: Number(fields.parallelism.value) || 2
    };
  }
  function queuePreferenceSave() {
    if (!preferencesReady || closed || !fields.clientKey.value || !fields.imageModel.value || !fields.driver.value) return;
    preferenceDirty = preferenceValue();
    if (preferenceTimer !== null) clearTimeout(preferenceTimer);
    preferenceTimer = setTimeout(() => { preferenceTimer = null; void flushPreferences(); }, 180);
  }
  async function flushPreferences() {
    if (preferenceSaving || closed) return;
    preferenceSaving = true;
    try {
      while (preferenceDirty && !closed) {
        const selection = preferenceDirty;
        preferenceDirty = null;
        let attempts = 0;
        while (attempts < 2 && !closed) {
          try {
            const result = await invoke('POST', 'api/preferences', {
              selection, expectedVersion: stateVersion
            });
            stateVersion = result.version;
            storedSelection = selection;
            break;
          } catch (error) {
            attempts += 1;
            if (attempts >= 2) {
              notice('无法保存上次选择：' + (error.message || error), true);
              break;
            }
            // 可能有其他管理员同时修改了同一插件实例的偏好，刷新 CAS 版本。
            try {
              const current = await invoke('GET', 'api/preferences');
              stateVersion = current.version === null ? null : Number(current.version);
            } catch {
              break;
            }
          }
        }
      }
    } finally {
      preferenceSaving = false;
      if (preferenceDirty && !closed) queuePreferenceSave();
    }
  }
  function chooseModel(models, candidates) {
    return candidates.find((model) => model && models.includes(model)) || models[0] || '';
  }
  function refreshImageOptions() {
    const model = fields.imageModel.value;
    const largeSizeSupported = /^gpt-image-2(?:[.-]|$)/.test(model);
    const maxQualitySupported = /^gpt-image-2\.(?:5|6)(?:[.-]|$)/.test(model);
    for (const option of fields.size.options) {
      option.disabled = !largeSizeSupported && !['auto','1024x1024','1536x1024','1024x1536'].includes(option.value);
    }
    for (const option of fields.quality.options) {
      option.disabled = !maxQualitySupported && ['xhigh','max'].includes(option.value);
    }
    if (fields.size.selectedOptions[0]?.disabled) fields.size.value = '1024x1024';
    if (fields.quality.selectedOptions[0]?.disabled) fields.quality.value = 'auto';
  }
  function applyModelsForKey() {
    const selectedId = fields.clientKey.value;
    const response = modelLists.get(selectedId);
    if (!response) {
      setOptions(fields.driver, [{value:'',label:'正在读取对话模型…'}], '');
      setOptions(fields.imageModel, [{value:'',label:'正在核实绘图模型…'}], '');
      count();
      return;
    }
    const models = Array.isArray(response.models) ? response.models.filter((name) =>
      typeof name === 'string' && !name.startsWith('gpt-image-')) : [];
    const images = Array.isArray(response.imageModels) ? response.imageModels.filter((name) =>
      typeof name === 'string' && name.startsWith('gpt-image-')) : [];
    const preferredDriver = chooseModel(models, [
      storedSelection?.driverModel,
      'gpt-6-luna', 'gpt-5.6-luna',
      configuredDriver, fields.driver.value
    ]);
    const preferredImage = chooseModel(images, [
      storedSelection?.imageModel, 'gpt-image-2.5-sunburst',
      'gpt-image-2.6-sunburst', fields.imageModel.value
    ]);
    setOptions(fields.driver, models.map((id)=>({value:id,label:id})), preferredDriver);
    setOptions(fields.imageModel, images.map((id)=>({value:id,label:id})), preferredImage);
    refreshImageOptions();
    count();
    if (preferencesReady) queuePreferenceSave();
  }
  function updateEligibleKeys() {
    if (closed) return;
    const current = fields.clientKey.value;
    const keyOrder = new Map(supportedKeys.map((key,i)=>[key.id,i]));
    eligibleKeys.sort((a,b) => (keyOrder.get(a.id) ?? 0)-(keyOrder.get(b.id) ?? 0));
    const preferred = preferredKeyId || '';
    const keyId = !selectedKeyTouched && eligibleKeys.some((key)=>key.id===preferred)
      ? preferred
      : eligibleKeys.some((key)=>key.id===current) ? current : eligibleKeys[0]?.id || '';
    if (!eligibleKeys.length) {
      setOptions(fields.clientKey, [{value:'',label:'正在核实可生图客户端 Key…'}],'');
      fields.clientKey.disabled = true;
    } else {
      const selectedChanged = current !== keyId;
      setOptions(fields.clientKey, eligibleKeys.map((key)=>({
        value:key.id,label:key.name+' · '+key.id
      })), keyId);
      fields.clientKey.disabled = false;
      // 后台探测新的 Key 时，不反复重置用户正在编辑的模型选择。
      if (selectedChanged || !fields.driver.value || !fields.imageModel.value) {
        applyModelsForKey();
      }
    }
  }
  async function scanKeys(keys, epoch) {
    let index = 0;
    let invalidCount = 0;
    let errorCount = 0;
    const worker = async () => {
      while (index < keys.length && epoch === scanEpoch && !closed) {
        // 已有生图时暂停无关 Key 的后台探测。
        if (activeTasks > 0) { await new Promise(resolve => setTimeout(resolve, 500)); continue; }
        const key = keys[index++];
        try {
          const response = await invoke('POST','api/models',{clientKeyId:key.id});
          if (epoch !== scanEpoch || closed) break;
          modelLists.set(key.id,response);
          const images = Array.isArray(response.imageModels) ? response.imageModels : [];
          const mainline = Array.isArray(response.models) ? response.models.filter((id)=>
            typeof id === 'string' && !id.startsWith('gpt-image-')) : [];
          if (images.length > 0 && mainline.length > 0) {
            eligibleKeys.push(key);
            updateEligibleKeys();
          } else {
            invalidCount++;
          }
          if (key.id === preferredKeyId && !preferencesReady) {
            preferencesReady=true;
            queuePreferenceSave();
          }
        } catch (error) {
          errorCount++;
        }
      }
    };
    await worker();
    if (epoch !== scanEpoch || closed) return;
    preferencesReady=true;
    if (eligibleKeys.length) queuePreferenceSave();
    if (!eligibleKeys.length) {
      setOptions(fields.clientKey,[{value:'',label:'没有有权生图的客户端 Key'}],'');
      setOptions(fields.imageModel,[{value:'',label:'无可用绘图模型'}],'');
      setOptions(fields.driver,[{value:'',label:'无可用对话模型'}],'');
      notice('未找到同时具有对话模型和绘图模型权限的 Key。请检查插件账号/分组和 Client Key 授权。'+
        (errorCount ? ' 另有 '+errorCount+' 个 Key 查询失败。' : ''),true);
    } else {
      notice('已筛除 '+invalidCount+' 个无生图模型或无对话模型的 Key。'+
        (errorCount ? ' 另有 '+errorCount+' 个查询失败，未加入列表。' : ''));
    }
    count();
  }
  async function bootstrap() {
    const epoch = ++scanEpoch;
    fields.refresh.disabled = true;
    preferencesReady = false;
    const previousSelected = fields.clientKey.value;
    accountOptionsLoaded = false;
    try {
      notice('读取配置与上次选择…');
      const [boot, saved] = await Promise.allSettled([
        invoke('GET', 'api/bootstrap'), invoke('GET', 'api/preferences')
      ]);
      if (closed || epoch !== scanEpoch) return;
      if (boot.status !== 'fulfilled') throw boot.reason;
      const state = boot.value;
      loadConfigBuilder(state);
      bridgeReady = typeof host().models.responses === 'function' && !!window.CPRImageClient;
      const configuredTimeout = Number(state.configuration?.timeoutMs);
      requestTimeoutMs = Number.isFinite(configuredTimeout) ? Math.max(30000, Math.min(600000, configuredTimeout)) : 300000;
      const warning = byId('backendNotice');
      warning.textContent = bridgeReady
        ? '即时生成：点击后立即调用 CPR 管理桥。生成期间请保持页面打开；关闭或刷新会取消本页请求。'
        : '即时管理桥不可用，请检查插件资源与 CPR 版本。';
      warning.classList.toggle('error', !bridgeReady);
      configuredDriver = state.driverModel || 'gpt-5.6-luna';
      const stored = saved.status === 'fulfilled' ? saved.value : {};
      storedSelection = stored.selection && typeof stored.selection === 'object' ? stored.selection : null;
      stateVersion = Number.isSafeInteger(stored.version) ? stored.version : null;
      applyRememberedSettings();
      if (![...fields.parallelism.options].some(option => option.value === fields.parallelism.value)) fields.parallelism.value = '2';
      fields.parallelism.disabled = false;
      supportedKeys = Array.isArray(state.keys) ? state.keys.filter(key => key.enabled) : [];
      modelLists = new Map();
      eligibleKeys = [];
      updateEligibleKeys();
      if (!supportedKeys.length) {
        notice('没有已启用的客户端 Key，请先在 CPR 管理端配置。', true);
        return;
      }
      if (!state.enabled || !Array.isArray(state.imageModels) || !state.imageModels.length) {
        notice('尚未开放生图账号/分组或图片模型，请展开配置生成器。', true);
      }
      const initial = supportedKeys.find(key => key.id === previousSelected) ||
        supportedKeys.find(key => key.id === storedSelection?.clientKeyId);
      preferredKeyId = initial?.id || '';
      selectedKeyTouched = false;
      preferencesReady = !preferredKeyId;
      void scanKeys(initial ? [initial, ...supportedKeys.filter(key => key.id !== initial.id)] : supportedKeys, epoch);
      if (byId('configPanel').open) void loadAccountOptions();
    } catch (error) {
      bridgeReady = false;
      notice('加载失败：' + (error.message || error), true);
      setOptions(fields.clientKey, [{value:'',label:'管理接口不可用'}], '');
    } finally {
      if (epoch === scanEpoch) fields.refresh.disabled = false;
      count();
    }
  }  function detectImageMime(magic) {
    if ([137,80,78,71,13,10,26,10].every((v,i)=>magic[i]===v)) return 'image/png';
    if (magic[0]===255 && magic[1]===216 && magic[2]===255) return 'image/jpeg';
    if ([82,73,70,70].every((v,i)=>magic[i]===v)
       && [87,69,66,80].every((v,i)=>magic[i+8]===v)) return 'image/webp';
    throw new Error('图片文件格式无效，仅支持 PNG、JPEG、WebP');
  }
  function imageChunksToDataUrl(chunks, expectedBytes, declaredMime) {
    if(!Number.isSafeInteger(expectedBytes) || expectedBytes<16 || expectedBytes>120*1024*1024)
      throw Error('后端图片长度不合法或超出大小限制');
    const encoded=[];
    let pending=new Uint8Array(0), observedBytes=0, signature=[];
    for(const chunk of chunks) {
      if(!(chunk instanceof Uint8Array))throw Error('图片分片不是字节数组');
      observedBytes+=chunk.length;
      if(observedBytes>expectedBytes)throw Error('图片分片长度超出预期');
      for(let i=0;i<chunk.length && signature.length<12;i++)signature.push(chunk[i]);
      const combined=new Uint8Array(pending.length+chunk.length);
      combined.set(pending);
      combined.set(chunk,pending.length);
      const aligned=combined.length-combined.length%3;
      for(let offset=0;offset<aligned;offset+=24576) {
        encoded.push(btoa(String.fromCharCode(...combined.subarray(offset,Math.min(offset+24576,aligned)))));
      }
      pending=combined.slice(aligned);
    }
    if(observedBytes!==expectedBytes)throw Error('图片字节数与后端响应不一致');
    const mime=detectImageMime(signature);
    if(declaredMime && declaredMime!==mime) throw Error('图片文件格式与后台元数据不一致');
    if(pending.length)encoded.push(btoa(String.fromCharCode(...pending)));
    return 'data:'+mime+';base64,'+encoded.join('');
  }

  // 正常生图只走官方即时管理桥；旧后台队列仅按需读取历史。
  let bridgeReady = false;
  let requestTimeoutMs = 300000;
  let stopwatchTimer = null;
  let accountOptionsLoaded = false;
  let accountOptionsLoading = false;
  let legacyTasks = [];
  let historyLoading = false;
  const fetching = new Set();

  function seconds(ms) {
    return Number.isFinite(ms) ? (Math.max(0, ms) / 1000).toFixed(1) + 's' : '—';
  }
  function timingText(task) {
    const t = task.timings;
    if (!t) return '';
    return '响应头 ' + seconds(t.headersMs) + ' · 首个数据 ' + seconds(t.firstByteMs)
      + ' · 图片返回 ' + seconds(t.firstImageMs) + ' · 总计 ' + seconds(t.totalMs);
  }
  function elapsedTime(task) {
    if (!Number.isFinite(task.startedMs)) return '';
    const end = task.completedMs || Date.now();
    return seconds(end - task.startedMs);
  }
  function createTaskCard(task, container) {
    const card = document.createElement('article');
    card.className = 'task-card';
    card.dataset.taskId = task.id;
    const head = document.createElement('div');
    head.className = 'task-head';
    const title = document.createElement('span');
    title.className = 'task-id';
    title.textContent = (task.legacy ? '旧任务 #' : '图片 #') + (task.number || task.id.slice(0, 8));
    const state = document.createElement('span');
    state.className = 'task-state';
    head.append(title, state);
    const preview = document.createElement('div');
    preview.className = 'task-preview';
    const info = document.createElement('span');
    info.className = 'task-preview-text';
    const img = document.createElement('img');
    img.hidden = true;
    img.alt = (task.prompt || '').slice(0, 100);
    preview.append(info, img);
    const details = document.createElement('div');
    details.className = 'task-details';
    const prompt = document.createElement('p');
    prompt.className = 'task-prompt';
    prompt.textContent = task.prompt || '';
    const model = document.createElement('span');
    model.className = 'task-model';
    model.textContent = task.imageModel + ' · ' + task.size + ' · ' + task.quality;
    const timing = document.createElement('div');
    timing.className = 'task-model';
    const error = document.createElement('div');
    error.className = 'task-error';
    const requestId = document.createElement('div');
    requestId.className = 'task-model';
    details.append(prompt, model, timing, error, requestId);
    const actions = document.createElement('div');
    actions.className = 'task-actions';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'task-button cancel';
    cancel.textContent = task.legacy ? '取消旧记录' : '停止此张';
    cancel.addEventListener('click', () => { void cancelTask(task); });
    const download = document.createElement('a');
    download.className = 'task-button';
    download.textContent = '下载图片';
    download.hidden = true;
    const load = document.createElement('button');
    load.type = 'button';
    load.className = 'task-button';
    load.textContent = '读取旧图片';
    load.hidden = true;
    load.addEventListener('click', () => { void downloadLegacyImage(task); });
    actions.append(cancel, download, load);
    card.append(head, preview, details, actions);
    container.appendChild(card);
    task.node = {card, state, info, img, model, timing, error, requestId, cancel, download, load};
    renderTask(task);
  }
  function renderTask(task) {
    if (!task.node || closed) return;
    const n = task.node;
    const pending = ['queued', 'running'].includes(task.status);
    const labels = {queued:'等待本页空闲',running:'生成中',succeeded:'已完成',failed:'失败',canceled:'已取消',interrupted:'中断'};
    const label = task.legacy && pending ? '旧后台已停用，未重发' : labels[task.status] || task.status;
    n.state.className = 'task-state ' + (task.status === 'succeeded' ? 'succeeded' : task.status === 'failed' ? 'failed' : task.status === 'running' && !task.legacy ? 'running' : '');
    n.state.textContent = label + (!task.legacy && task.startedMs ? ' ' + elapsedTime(task) : '');
    n.card.className = 'task-card ' + task.status;
    n.img.hidden = !task.imageUrl;
    if (task.imageUrl && n.img.src !== task.imageUrl) n.img.src = task.imageUrl;
    n.info.hidden = !!task.imageUrl;
    n.info.textContent = task.legacy && pending ? '仅保留历史记录，不会自动重新生图'
      : task.status === 'running' ? (task.phaseText || '请求已提交，等待模型响应')
      : task.status === 'queued' ? '已有请求运行，空位释放后立即开始'
      : task.status === 'succeeded' ? '点击读取旧图片' : '本次未交付完整图片';
    n.model.textContent = task.imageModel + ' · 选择 ' + task.size + (task.actualWidth ? ' · 实际 ' + task.actualWidth + '×' + task.actualHeight : '') + ' · ' + task.quality;
    n.timing.textContent = timingText(task);
    n.timing.hidden = !n.timing.textContent;
    n.error.textContent = task.error || '';
    n.error.hidden = !task.error;
    n.requestId.textContent = task.requestId ? '请求 ID：' + task.requestId : '';
    n.requestId.hidden = !task.requestId;
    n.cancel.hidden = !pending;
    n.download.hidden = !task.imageUrl;
    if (task.imageUrl) {
      n.download.href = task.imageUrl;
      n.download.download = 'codex-image-' + task.id.slice(0, 12) + '.' + (task.resultMime === 'image/webp' ? 'webp' : task.resultMime === 'image/jpeg' ? 'jpg' : 'png');
    }
    n.load.hidden = !task.legacy || task.status !== 'succeeded' || !!task.imageUrl;
    n.load.disabled = fetching.has(task.id);
  }
  function refreshStopwatches() {
    if (!closed) for (const task of tasks) if (task.status === 'running') renderTask(task);
  }
  function updateQueueInfo() {
    const running = tasks.filter(t => t.status === 'running').length;
    const queued = tasks.filter(t => t.status === 'queued').length;
    const done = tasks.filter(t => t.status === 'succeeded').length;
    const failed = tasks.filter(t => t.status === 'failed').length;
    fields.queueInfo.textContent = running + ' 运行中 · ' + queued + ' 等待本页空闲 · ' + done + ' 完成 · ' + failed + ' 失败';
    fields.empty.hidden = tasks.length > 0;
    if (running) status('working', '即时生成中');
    else if (queued) status('working', '等待本页空闲');
    else if (done) status('success', '已完成');
    else if (failed) status('error', '请求失败');
    else status('', tasks.length ? '暂无进行中的任务' : '尚无任务');
    count();
  }
  function imageRequest(task) {
    // 保留用户验证可用的 v0.5.1 请求形状，不加入后台账号绑定或压缩参数。
    return {
      model: task.driverModel, stream: true, store: false,
      input: [{type:'message',role:'user',content:[{type:'input_text',text:task.prompt}]}],
      instructions: 'Use image_generation to create exactly one image corresponding closely to the user instruction. Preserve its original language and visual constraints.',
      tools: [{type:'image_generation',model:task.imageModel,action:'auto',size:task.size,quality:task.quality,output_format:'png'}],
      tool_choice:'auto', parallel_tool_calls:false, include:['reasoning.encrypted_content']
    };
  }
  async function runTask(task) {
    const controller = new AbortController();
    task.controller = controller;
    task.status = 'running';
    task.startedMs = Date.now();
    let timedOut = false;
    const budget = task.timeoutMs;
    const timeout = setTimeout(() => { timedOut = true; controller.abort(); }, budget);
    renderTask(task);
    try {
      const result = await window.CPRImageClient.run({
        clientKeyId:task.clientKeyId, body:task.body, signal:controller.signal,
        onPhase(info) {
          task.phaseText = info.phase === 'headers' ? '已收到响应头，读取模型输出'
            : info.phase === 'image' ? '已收到图片，等待完整响应'
            : '请求已提交，等待模型完成';
          renderTask(task);
        }
      });
      task.requestId = result.requestId || result.responseId || null;
      task.timings = result.timings;
      if (controller.signal.aborted || result.status === 'canceled') {
        task.status = timedOut ? 'failed' : 'canceled';
        task.error = timedOut ? '达到本页请求超时 ' + seconds(budget) : null;
        return;
      }
      if (result.status !== 'completed') {
        const error = result.error || {};
        task.status = 'failed';
        task.error = [
          error.code || error.kind || result.status,
          error.message || '响应没有完整完成',
          error.stage ? 'stage=' + error.stage : '',
          error.upstreamKind ? 'kind=' + error.upstreamKind : '',
          result.httpStatus ? 'HTTP ' + result.httpStatus : ''
        ].filter(Boolean).join(' · ');
        return;
      }
      if (result.images.length !== 1) throw new Error('完整响应包含 ' + result.images.length + ' 张图片，当前任务预期 1 张；可在原始请求调试页检查返回。');
      const image = result.images[0];
      if (!image.width || !image.height || image.width * image.height > 12000000) throw new Error('图片像素信息无效或超过显示上限');
      task.imageUrl = image.dataUrl;
      task.resultMime = image.mimeType;
      task.actualWidth = image.width;
      task.actualHeight = image.height;
      if (task.size !== 'auto' && task.size !== image.width + 'x' + image.height) {
        throw new Error('图片已返回，但请求尺寸 ' + task.size + '，实际尺寸 ' + image.width + 'x' + image.height + '；不会将其标记为所选尺寸生成成功。');
      }
      task.status = 'succeeded';
      task.error = null;
    } catch (error) {
      task.status = controller.signal.aborted && !timedOut ? 'canceled' : 'failed';
      task.error = task.status === 'canceled' ? null : String(error.message || error);
    } finally {
      clearTimeout(timeout);
      task.completedMs = Date.now();
      task.controller = null;
      activeTasks--;
      renderTask(task);
      updateQueueInfo();
      pump();
    }
  }
  function pump() {
    if (closed || !bridgeReady) return;
    const parallel = Math.max(1, Math.min(MAX_PARALLEL, Number(fields.parallelism.value) || 1));
    while (activeTasks < parallel) {
      const task = tasks.find(item => item.status === 'queued');
      if (!task) break;
      activeTasks++;
      task.status = 'running';
      void runTask(task);
    }
    updateQueueInfo();
  }
  function queueBatch() {
    const n = Number(fields.batchCount.value);
    if (closed || !bridgeReady || !fields.clientKey.value || !fields.driver.value || !fields.imageModel.value
        || !fields.prompt.value.trim() || !Number.isInteger(n) || n < 1 || n > 10) return;
    if (tasks.length + n > MAX_TASKS) {
      notice('本页最多保留 ' + MAX_TASKS + ' 张；请先保存图片并清理已结束任务。', true);
      return;
    }
    for (let index = 0; index < n; index++) {
      const task = {
        id:'live_' + Date.now().toString(36) + '_' + (++taskCounter),
        number:taskCounter, clientKeyId:fields.clientKey.value, driverModel:fields.driver.value,
        imageModel:fields.imageModel.value, prompt:fields.prompt.value,
        size:fields.size.value, quality:fields.quality.value, status:'queued', createdMs:Date.now(),
        imageUrl:null, node:null, error:null, timeoutMs:requestTimeoutMs
      };
      task.body = imageRequest(task);
      tasks.push(task);
      createTaskCard(task, fields.taskGrid);
    }
    notice('已开始即时调用。生成期间请保持本页打开；更多参数可在“原始请求调试”页编辑。');
    pump();
  }
  async function cancelTask(task) {
    if (!['queued', 'running'].includes(task.status)) return;
    if (task.legacy) {
      try {
        await invoke('POST', 'api/jobs/cancel', {id:task.id});
        task.status = 'canceled';
        renderTask(task);
      } catch (error) { byId('legacyNotice').textContent = '取消旧记录失败：' + (error.message || error); }
      return;
    }
    if (task.controller) task.controller.abort();
    else {
      task.status = 'canceled';
      task.completedMs = Date.now();
      renderTask(task);
      updateQueueInfo();
    }
  }
  function cancelAllTasks() {
    for (const task of tasks) if (task.status === 'queued') {
      task.status = 'canceled';
      task.completedMs = Date.now();
      renderTask(task);
    }
    for (const task of tasks) if (task.controller) task.controller.abort();
    updateQueueInfo();
  }
  function forgetTask(task, collection) {
    if (task.node) task.node.card.remove();
    task.imageUrl = null;
    task.body = null;
    const index = collection.indexOf(task);
    if (index >= 0) collection.splice(index, 1);
  }
  function clearFinishedTasks() {
    for (const task of tasks.slice()) if (!['queued', 'running'].includes(task.status)) forgetTask(task, tasks);
    updateQueueInfo();
  }
  async function loadHistory() {
    if (closed || historyLoading) return;
    historyLoading = true;
    byId('loadHistory').disabled = true;
    try {
      const result = await invoke('GET', 'api/jobs');
      if (closed) return;
      for (const task of legacyTasks.slice()) forgetTask(task, legacyTasks);
      for (const item of result.jobs || []) {
        const task = {...item, legacy:true, imageUrl:null, node:null};
        legacyTasks.push(task);
        createTaskCard(task, byId('legacyGrid'));
      }
      byId('legacyNotice').textContent = legacyTasks.length
        ? '共 ' + legacyTasks.length + ' 条旧记录。旧队列不再执行，也不会自动重发；已完成图片按需读取。'
        : '没有旧后台任务记录。';
    } catch (error) { byId('legacyNotice').textContent = '读取旧记录失败：' + (error.message || error); }
    finally { historyLoading = false; byId('loadHistory').disabled = false; }
  }
  async function downloadLegacyImage(task) {
    if (fetching.has(task.id) || task.imageUrl) return;
    fetching.add(task.id);
    renderTask(task);
    try {
      const chunks = [];
      const total = task.resultBytes;
      if (!Number.isSafeInteger(total) || total < 16 || total > 120 * 1024 * 1024) throw new Error('旧图片总长度无效');
      let offset = 0;
      for (let step = 0; offset < total && step < 512; step++) {
        const response = await host().request({method:'POST',path:'api/jobs/image',contentType:'application/json',body:JSON.stringify({id:task.id,offset})});
        if (response.status !== 200) throw new Error('读取旧图片返回 HTTP ' + response.status);
        const chunk = new Uint8Array(response.body);
        if (!chunk.length || chunk.length > 256 * 1024 || offset + chunk.length > total) throw new Error('旧图片分片长度无效');
        chunks.push(chunk);
        offset += chunk.length;
        if (closed) return;
      }
      if (offset !== total) throw new Error('旧图片分片未完整结束');
      task.imageUrl = imageChunksToDataUrl(chunks, total, task.resultMime);
      task.resultMime = task.imageUrl.slice(5, task.imageUrl.indexOf(';'));
      task.error = null;
    } catch (error) { task.error = '读取旧图片失败：' + (error.message || error); }
    finally { fetching.delete(task.id); renderTask(task); }
  }
  async function loadAccountOptions() {
    if (accountOptionsLoaded || accountOptionsLoading || closed) return;
    accountOptionsLoading = true;
    const epoch = scanEpoch;
    try {
      const data = await invoke('GET', 'api/account-options');
      if (closed || epoch !== scanEpoch) return;
      builderState.accounts = Array.isArray(data.accountOptions) ? data.accountOptions : [];
      builderState.groups = Array.isArray(data.groupOptions) ? data.groupOptions : [];
      builderState.truncated = !!data.accountOptionsTruncated;
      builderState.accountOptionsError = false;
      renderAccountOptions();
      renderGroupOptions();
      accountOptionsLoaded = true;
    } catch (error) {
      builderState.accountOptionsError = true;
      configFields.notice.textContent = '账号选项读取失败，可手动输入 ID：' + (error.message || error);
    } finally { accountOptionsLoading = false; }
  }
  function closePage() {
    closed = true;
    scanEpoch++;
    if (stopwatchTimer) clearInterval(stopwatchTimer);
    if (preferenceTimer !== null) clearTimeout(preferenceTimer);
    for (const task of tasks) if (task.controller) task.controller.abort();
  }

  fields.prompt.addEventListener('input', () => { selected = -1; presetButtons(); count(); });
  fields.clientKey.addEventListener('change', () => { selectedKeyTouched = true; applyModelsForKey(); queuePreferenceSave(); });
  fields.driver.addEventListener('change', () => { queuePreferenceSave(); count(); });
  fields.imageModel.addEventListener('change', () => { refreshImageOptions(); queuePreferenceSave(); count(); });
  [fields.size, fields.quality, fields.batchCount].forEach(el => el.addEventListener('change', () => { queuePreferenceSave(); count(); }));
  fields.parallelism.addEventListener('change', () => { queuePreferenceSave(); pump(); });
  fields.generate.addEventListener('click', queueBatch);
  fields.cancel.addEventListener('click', cancelAllTasks);
  fields.clearFinished.addEventListener('click', clearFinishedTasks);
  fields.refresh.addEventListener('click', bootstrap);
  byId('configPanel').addEventListener('toggle', () => { if (byId('configPanel').open) void loadAccountOptions(); });
  byId('legacyPanel').addEventListener('toggle', () => { if (byId('legacyPanel').open && !legacyTasks.length) void loadHistory(); });
  byId('loadHistory').addEventListener('click', loadHistory);
  window.addEventListener('pagehide', closePage, {once:true});
  loadConfigBuilder({
    configuration:{timeoutMs:300000,bridgeMode:'auto',driverModel:'gpt-5.6-luna',
      imageModels:['gpt-image-2.6-sunburst','gpt-image-2.6-flare','gpt-image-2.5-sunburst'],imageGroupIds:[],imageAccountIds:[]},
    candidateImageModels:['gpt-image-2.6-sunburst','gpt-image-2.6-flare','gpt-image-2.5-sunburst','gpt-image-2.5-flare','gpt-image-2']
  });
  configFields.backendConcurrency.disabled = true;
  fields.prompt.value = presets[0].prompt;
  presetButtons();
  updateQueueInfo();
  stopwatchTimer = setInterval(refreshStopwatches, 200);
  void bootstrap();
})();
