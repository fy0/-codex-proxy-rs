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
  const MAX_TASKS = 30;
  const MAX_RETAINED_IMAGES = 12;
  const MAX_PARALLEL = 3;
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
    fields.generate.disabled=closed||!fields.clientKey.value||!fields.imageModel.value
      ||!fields.prompt.value.trim()||fields.prompt.value.trim().length>4096
      ||!backendEnabled||!Number.isInteger(number)||number<1||number>10
      ||liveTaskCount()+number>MAX_TASKS;
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
    await Promise.all(Array.from({length:Math.min(3,keys.length)},worker));
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
    try {
      notice('读取插件配置与上次选择…');
      const state = await invoke('GET','api/bootstrap');
      if (closed || epoch !== scanEpoch) return;
      loadConfigBuilder(state);
      backendEnabled=!!state.backendJobsEnabled;
      const actualConcurrency=String(state.concurrencyPerAccount||8);
      if ([...fields.parallelism.options].some(option=>option.value===actualConcurrency)) {
        fields.parallelism.value=actualConcurrency;
      }
      fields.parallelism.disabled=true;
      const warning=byId('backendNotice');
      if(warning) {
        warning.textContent=backendEnabled
          ? '后台任务已启用：关闭页面后仍继续执行，图片放在服务器临时目录；每账号默认并发 8。'
          : '后台任务不可用：请检查 CPR 插件维护调度。';
        warning.classList.toggle('error',!backendEnabled);
      }
      startPolling();
      configuredDriver = state.driverModel || 'gpt-5.6-luna';
      try {
        const stored = await invoke('GET','api/preferences');
        storedSelection = stored.selection && typeof stored.selection === 'object' ? stored.selection : null;
        stateVersion = Number.isSafeInteger(stored.version) ? stored.version : null;
      } catch (error) {
        storedSelection = null;
        stateVersion = null;
        notice('偏好存储暂不可用：本次可绘图，但下次可能无法恢复选择。',true);
      }
      if (closed || epoch !== scanEpoch) return;
      applyRememberedSettings();
      // 旧偏好不能覆盖已经冻结在后端插件配置中的真实并发上限。
      fields.parallelism.value=actualConcurrency;
      supportedKeys = Array.isArray(state.keys) ? state.keys.filter((key)=>key.enabled) : [];
      modelLists = new Map();
      eligibleKeys = [];
      updateEligibleKeys();
      if (!supportedKeys.length) {
        notice('没有已启用的客户端 Key，请先在 CPR 管理端配置。',true);
        return;
      }
      if (!state.enabled || !Array.isArray(state.imageModels) || !state.imageModels.length) {
        notice('尚未开放生图账号/分组或图片模型。请展开上方配置生成器。',true);
      }
      const initial = supportedKeys.find((key)=>key.id===previousSelected) ||
        supportedKeys.find((key)=>key.id===storedSelection?.clientKeyId);
      preferredKeyId = initial?.id || '';
      selectedKeyTouched=false;
      const ordered = initial ? [initial,...supportedKeys.filter((key)=>key.id!==initial.id)] : supportedKeys;
      preferencesReady = !preferredKeyId;
      // 各 Key 的模型权限由官方管理 API 分批验证。无图片模型的 Key 不出现在下拉框。
      void scanKeys(ordered,epoch);
    } catch (error) {
      notice('加载失败：'+(error.message||error),true);
      setOptions(fields.clientKey,[{value:'',label:'管理接口不可用'}],'');
      count();
    } finally {
      if (epoch === scanEpoch) fields.refresh.disabled = false;
    }
  }
  function detectImageMime(magic) {
    if ([137,80,78,71,13,10,26,10].every((v,i)=>magic[i]===v)) return 'image/png';
    if (magic[0]===255 && magic[1]===216 && magic[2]===255) return 'image/jpeg';
    if ([82,73,70,70].every((v,i)=>magic[i]===v)
       && [87,69,66,80].every((v,i)=>magic[i+8]===v)) return 'image/webp';
    throw new Error('图片文件格式无效，仅支持 PNG、JPEG、WebP');
  }
  function requireImage(base64) {
    if (typeof base64 !== 'string' || !base64.length || base64.length > 160*1024*1024) {
      throw new Error('没有生成图片，或结果超过 160 MiB 的显示上限');
    }
    const head=atob(base64.slice(0,24));
    const mime=detectImageMime(Array.from(head,c=>c.charCodeAt(0)));
    return 'data:'+mime+';base64,'+base64;
  }
  function releaseImageUrl(url) {
    if (typeof url === 'string' && url.startsWith('blob:')) URL.revokeObjectURL(url);
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
        if (buffer.length > 180 * 1024 * 1024) throw new Error('模型响应过大，已停止读取');
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
  // 所有请求由 CPR 后端 maintenance 拥有；此处仅提交、轮询并取回已保存的 PNG。
  let backendEnabled=false;
  let workerActive=false;
  let dispatchIntervalMs=30000;
  let polling=false;
  let pollStarted=false;
  let pollTimer=null;
  let stopwatchTimer=null;
  const hiddenFinished=new Set();
  const fetching=new Set();

  function createTaskCard(task) {
    const card=document.createElement('article');
    card.className='task-card';
    card.dataset.taskId=task.id;
    const head=document.createElement('div');
    head.className='task-head';
    const title=document.createElement('span');
    title.className='task-id';
    title.textContent='图片 #'+task.id.slice(0,8);
    const state=document.createElement('span');
    state.className='task-state';
    head.append(title,state);
    const preview=document.createElement('div');
    preview.className='task-preview';
    const info=document.createElement('span');
    info.className='task-preview-text';
    const img=document.createElement('img');
    img.hidden=true;
    img.alt=task.prompt.slice(0,100);
    preview.append(info,img);
    const details=document.createElement('div');
    details.className='task-details';
    const prompt=document.createElement('p');
    prompt.className='task-prompt';
    prompt.textContent=task.prompt;
    prompt.title=task.prompt;
    const model=document.createElement('span');
    model.className='task-model';
    model.textContent=task.imageModel+' · '+task.size+' · '+task.quality;
    const error=document.createElement('div');
    error.className='task-error';
    error.hidden=true;
    const requestId=document.createElement('div');
    requestId.className='task-model';
    requestId.hidden=true;
    details.append(prompt,model,error,requestId);
    const actions=document.createElement('div');
    actions.className='task-actions';
    const cancel=document.createElement('button');
    cancel.type='button';
    cancel.className='task-button cancel';
    cancel.textContent='停止此张';
    cancel.addEventListener('click',()=>{void cancelTask(task);});
    const download=document.createElement('a');
    download.className='task-button';
    download.textContent='保存图片';
    download.hidden=true;
    download.download='codex-image-'+task.id.slice(0,8)+'.'+(task.resultMime==='image/jpeg'?'jpg':task.resultMime==='image/webp'?'webp':'png');
    actions.append(cancel,download);
    card.append(head,preview,details,actions);
    fields.taskGrid.append(card);
    task.node={card,state,info,img,model,error,requestId,cancel,download};
    renderTask(task);
  }

  function elapsedTime(job) {
    if (!Number.isFinite(job.startedMs) || job.startedMs<=0) return '';
    const end=job.status==='running'?Date.now()
      :(Number.isFinite(job.completedMs)?job.completedMs:Date.now());
    return Math.max(0,(end-job.startedMs)/1000).toFixed(1)+'s';
  }
  function queueLabel() {
    if (!workerActive) return '待后台唤醒';
    return tasks.some((task)=>task.status==='running') ? '等待后台分派' : '正在分派';
  }
  function waitingTime(task) {
    return Number.isFinite(task.createdMs) ? Math.max(0,(Date.now()-task.createdMs)/1000).toFixed(1)+'s' : '';
  }
  function refreshStopwatches() {
    if (closed)return;
    for (const task of tasks) {
      if (!task.node) continue;
      if (task.status==='running') task.node.state.textContent='生成中 '+elapsedTime(task);
      else if (task.status==='queued') task.node.state.textContent=queueLabel()+' '+waitingTime(task);
    }
  }
  function renderTask(task) {
    if (!task.node || closed) return;
    const stateKey=task.status || 'queued';
    const n=task.node;
    const labels={
      queued:queueLabel(),running:'生成中',succeeded:'已完成',
      failed:'失败',canceled:'已取消',interrupted:'中断（请手动重试）'
    };
    n.state.className='task-state '+(
      stateKey==='succeeded'?'succeeded':stateKey==='running'?'running':
        stateKey==='failed'||stateKey==='interrupted'?'failed':'');
    n.state.textContent=(labels[stateKey]||stateKey)
      + (stateKey==='queued'?' '+waitingTime(task):task.startedMs?' '+elapsedTime(task):'');
    n.card.className='task-card '+stateKey;
    n.img.hidden=!(stateKey==='succeeded' && task.imageUrl);
    if (!n.img.hidden && n.img.src!==task.imageUrl) n.img.src=task.imageUrl;
    n.info.hidden=!n.img.hidden;
    if (!n.info.hidden) {
      n.info.textContent=stateKey==='queued'
        ? (workerActive
          ? '后台工作器已运行，将按账号权限与并发规则领取任务…'
          : '等待 CPR 后台唤醒；宿主维护间隔约 '+Math.round(dispatchIntervalMs/1000)+' 秒，不代表账号队列已满。')
        :stateKey==='running'?'CPR 后端生成中，关闭页面不会停止…'
        :stateKey==='succeeded'?'图片已保存在后端，读取预览中…'
        :stateKey==='interrupted'?'维护任务曾被中断；不会自动重复扣费'
        :'本次任务无可用图片';
    }
    n.error.hidden=!(task.error && ['failed','interrupted'].includes(stateKey));
    n.error.textContent=task.error||'';
    n.requestId.hidden=!task.requestId;
    n.requestId.textContent=task.requestId?'请求 ID · '+task.requestId:'';
    n.cancel.hidden=!['queued','running'].includes(stateKey);
    n.download.hidden=!(stateKey==='succeeded'&&task.imageUrl);
    if(!n.download.hidden) {
      n.download.href=task.imageUrl;
      n.download.download='codex-image-'+task.id.slice(0,8)+'.'+(
        task.resultMime==='image/jpeg'?'jpg':task.resultMime==='image/webp'?'webp':'png');
    }
    const dimensions=task.actualWidth && task.actualHeight
      ? ' · 实际 '+task.actualWidth+'×'+task.actualHeight : '';
    n.model.textContent=task.imageModel+' · 选择 '+task.size+dimensions+' · '+task.quality;
  }
  function forgetTask(task) {
    if(task.imageUrl)releaseImageUrl(task.imageUrl);
    task.node?.card.remove();
    const at=tasks.indexOf(task);
    if(at>=0)tasks.splice(at,1);
  }
  async function downloadImage(task) {
    if (fetching.has(task.id) || task.imageUrl || !task.resultBytes || task.status!=='succeeded')return;
    fetching.add(task.id);
    try{
      let offset=0;
      const chunks=[];
      while (offset<task.resultBytes && !closed && task.status==='succeeded') {
        const response=await host().request({
          method:'POST',path:'api/jobs/image',
          contentType:'application/json',
          body:JSON.stringify({id:task.id,offset})
        });
        if (response.status!==200)throw Error('读取图片分片失败：HTTP '+response.status);
        const data=new Uint8Array(response.body);
        if (data.length<1 || data.length>256*1024)throw Error('图片分片无效');
        offset+=data.length;
        if (offset>task.resultBytes || offset>120*1024*1024)throw Error('图片大小超过限制');
        chunks.push(data);
      }
      if (offset===task.resultBytes && !closed && task.status==='succeeded') {
        const url=imageChunksToDataUrl(chunks,task.resultBytes,task.resultMime);
        if (task.imageUrl)releaseImageUrl(task.imageUrl);
        task.imageUrl=url;
        const detectedMime=/^data:(image\/(?:png|jpeg|webp));base64,/.exec(url);
        if (detectedMime) task.resultMime=detectedMime[1];
        renderTask(task);
      }
    }catch(error){
      // 文件可能因后端清理、目录未挂载等原因暂时不可用。
      if (!closed)notice('图片 #'+task.id.slice(0,8)+' 无法读取：'+(error.message||error),true);
    }finally{fetching.delete(task.id);}
  }
  function updateQueueInfo() {
    const running=tasks.filter((t)=>t.status==='running').length;
    const queued=tasks.filter((t)=>t.status==='queued').length;
    const done=tasks.filter((t)=>t.status==='succeeded').length;
    const failed=tasks.filter((t)=>t.status==='failed'||t.status==='interrupted').length;
    const canceled=tasks.filter((t)=>t.status==='canceled').length;
    fields.queueInfo.textContent=running+' 运行中 · '+queued+' 待启动 · '+done+' 完成 · '
      +failed+' 失败/中断 · '+canceled+' 取消';
    fields.empty.hidden=tasks.length>0;
    if (running)status('working','后端生成中');
    else if (queued)status('working',queueLabel());
    else if(done)status('success','已完成');
    else if(failed)status('error','部分任务失败');
    else status('',tasks.length?'暂无进行中的任务':'尚无任务');
    fields.clearFinished.disabled=!tasks.some((t)=>!['queued','running'].includes(t.status));
    fields.cancel.disabled=!(running||queued);
    count();
  }
  function refreshTasks(remoteJobs) {
    const items=Array.isArray(remoteJobs)?remoteJobs:[];
    const byId=new Map(tasks.map((task)=>[task.id,task]));
    const remoteIds=new Set(items.map((job)=>job.id));
    for(const task of tasks.slice()){
      if(!remoteIds.has(task.id)){
        forgetTask(task);
      }
    }
    for(const item of items){
      if (!item || typeof item.id!=='string' || hiddenFinished.has(item.id))continue;
      let task=byId.get(item.id);
      if(!task){
        task={...item,imageUrl:null,node:null};
        tasks.push(task);
        createTaskCard(task);
      }else{
        const savedUrl=task.imageUrl;
        const savedNode=task.node;
        const detectedMime=task.resultMime;
        Object.assign(task,item);
        if (!task.resultMime && savedUrl) task.resultMime=detectedMime;
        task.imageUrl=savedUrl;
        task.node=savedNode;
        renderTask(task);
      }
      if(task.status==='succeeded' && !task.imageUrl)void downloadImage(task);
    }
    updateQueueInfo();
  }
  async function pollJobs() {
    if (closed)return;
    if (polling)return;
    polling=true;
    try{
      const response=await invoke('GET','api/jobs');
      if(!closed){
        backendEnabled=!!response.backendEnabled;
        workerActive=response.workerActive===true;
        if (Number.isFinite(response.dispatchIntervalMs)) dispatchIntervalMs=response.dispatchIntervalMs;
        refreshTasks(response.jobs);
      }
    }catch(error){
      if(!closed)notice('读取后端队列失败：'+(error.message||error),true);
    }finally{
      polling=false;
      if(!closed)pollTimer=setTimeout(()=>{void pollJobs();},2000);
    }
  }
  function startPolling() {
    if(pollStarted)return;
    pollStarted=true;
    stopwatchTimer=setInterval(refreshStopwatches,100);
    void pollJobs();
  }

  async function queueBatch() {
    const n=Number(fields.batchCount.value);
    if(closed || !fields.clientKey.value || !fields.imageModel.value ||
      !fields.prompt.value.trim() || !Number.isInteger(n) || n<1 || n>10)return;
    if(!backendEnabled){
      notice('后端任务不可用：请检查维护回调与插件配置。',true);
      return;
    }
    fields.generate.disabled=true;
    try {
      const response=await invoke('POST','api/jobs',{
        clientKeyId:fields.clientKey.value,
        driverModel:fields.driver.value || undefined,
        imageModel:fields.imageModel.value,
        prompt:fields.prompt.value.trim(),
        size:fields.size.value,
        quality:fields.quality.value,
        count:n
      });
      notice('已向 CPR 后端提交 '+response.ids.length+' 张。页面关闭后任务仍将继续。');
      await pollJobs();
    }catch(error){
      notice('后端任务提交失败：'+(error.message||error),true);
    }finally{count();}
  }
  async function cancelTask(task) {
    if(!['queued','running'].includes(task.status))return;
    try{
      await invoke('POST','api/jobs/cancel',{id:task.id});
      task.status='canceled';
      renderTask(task);
      updateQueueInfo();
      notice('任务已标记取消。已发往上游的请求可能继续消耗额度。');
    }catch(error){notice('取消失败：'+(error.message||error),true);}
  }
  async function cancelAllTasks() {
    const ids=tasks.filter((t)=>['queued','running'].includes(t.status)).map((t)=>t.id);
    await Promise.all(ids.map((id)=>{
      const task=tasks.find((item)=>item.id===id);
      return cancelTask(task);
    }));
  }
  function clearFinishedTasks() {
    for(const task of tasks.slice()) {
      if(['queued','running'].includes(task.status))continue;
      hiddenFinished.add(task.id);
      forgetTask(task);
    }
    updateQueueInfo();
  }
  function closePage() {
    closed=true;
    scanEpoch++;
    if(pollTimer)clearTimeout(pollTimer);
    if(stopwatchTimer)clearInterval(stopwatchTimer);
    if(preferenceTimer!==null)clearTimeout(preferenceTimer);
    // 不取消后台生图；这里只清理浏览器内存与观察器。
    for(const task of tasks)if(task.imageUrl)releaseImageUrl(task.imageUrl);
  }

  fields.prompt.addEventListener('input',()=>{
    selected=-1;
    presetButtons();
    count();
  });
  fields.clientKey.addEventListener('change',()=>{
    selectedKeyTouched=true;
    applyModelsForKey();
    queuePreferenceSave();
  });
  fields.driver.addEventListener('change',()=>{queuePreferenceSave();count();});
  fields.imageModel.addEventListener('change',()=>{
    refreshImageOptions();queuePreferenceSave();count();
  });
  [fields.size,fields.quality,fields.batchCount].forEach((el)=>
    el.addEventListener('change',()=>{queuePreferenceSave();count();}));
  fields.parallelism.addEventListener('change',()=>{
    // 后端实际每账号并发由 concurrencyPerAccount 配置与 CPR Core 租约决定。
    queuePreferenceSave();
    count();
  });
  fields.generate.addEventListener('click',queueBatch);
  fields.cancel.addEventListener('click',cancelAllTasks);
  fields.clearFinished.addEventListener('click',clearFinishedTasks);
  fields.refresh.addEventListener('click',bootstrap);
  window.addEventListener('pagehide',closePage,{once:true});
  loadConfigBuilder({
    configuration:{timeoutMs:300000,bridgeMode:'auto',driverModel:'gpt-5.6-luna',
      imageModels:['gpt-image-2.6-sunburst','gpt-image-2.6-flare','gpt-image-2.5-sunburst'],
      imageGroupIds:[],imageAccountIds:[]},
    candidateImageModels:['gpt-image-2.6-sunburst','gpt-image-2.6-flare',
      'gpt-image-2.5-sunburst','gpt-image-2.5-flare','gpt-image-2']
  });
  fields.prompt.value=presets[0].prompt;
  presetButtons();
  updateQueueInfo();
  void bootstrap();
})();
