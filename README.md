# CPR Image Bridge v0.7.5 (official SDK)

独立 Rust 插件，面向 zyycn/codex-proxy-rs 官方主线。SDK/打包工具固定源码提交：96e8443ede051abdd8f0bac69462a7cfe2945633。宿主要求 RPC v7、middleware v6、scheduler v3、management v2，MSRV Rust 1.99.1。旧 fork 的宿主与 SDK 不作为运行目标。

## v0.7.5：后台执行合同、账号日费用与管理界面修复

后台画图任务在发送前选择 **Responses `image_generation`** 路径，顶层模型使用本次选择的对话驱动模型，图片模型精确放在 `tools[0].model`。固定官方宿主的 `host.model` 原生 `GenerateImage` 路径会构造带 `upstream_model` 的普通模型候选，原生 Images Provider 会在发送前拒绝；修改尺寸或压缩参数不能修复这个路由矛盾。外部原生 Images HTTP 仍使用宿主正确的 Provider endpoint 路由。插件不在失败后自动重发图片请求。

本版保存每个任务选择的驱动模型，并在提交和执行时检查同一账号的权限交集；Core 仍执行 Key 分组的图片权限检查。后台请求强制图片工具、使用流式 Responses、关闭并行工具调用，不强制驱动模型提供推理功能。事件解析支持 JSON、WebSocket 与原始 SSE，并等待完整完成事件后才校验图片格式、尺寸及写盘。

任务按完成顺序立即持久化，额度维护与生图分支并行推进；生图尚未结束时，在同一维护父调用内每轮有界额度刷新结束后等待 15 秒继续刷新，防止长生图令日费用观测自然过期。界面区分“待后台唤醒”和“等待后台分派”，等待时间与后台处理时间分别显示。固定宿主在每次维护结束后约 30 秒再次唤醒，尚无公开插件唤醒 API；本版不把这个宿主调度空窗表述为账号队列满或全时立即执行。每轮限制 16 个不同 Client Key，避免超出宿主父调用身份缓存的合同；本版后台总并发上限为 16，仍受每账号配置和 Core 租约约束，并为额度维护、状态与管理回调保留余量。

诊断增加插件版本、SDK 本地／宿主远端错误来源，以及严格白名单的错误原因。读取回调输入错误与 Core／Provider 拒绝分开记录，原始错误正文、完整 details、提示词、账号和凭据不写入诊断。

### 账号每日 USD 费用保护

`quotaGuard[]` 规则可增加 `dailyLimitUsd: "20"`；省略或 `null` 表示关闭，`"0"` 表示立即停止为该账号分配新请求。它覆盖该账号的**全部 CPR 请求**，不限制为图片或某个 Client Key。

金额来自宿主 `usage.records.list` 持久账本，按请求开始时间归属 UTC 自然日；跨午夜完成的请求仍计入开始那天。每天 UTC 00:00 换日。限额输入使用精确十进制字符串（最多 18 位整数、12 位小数），账本成本精确保留最多 18 位小数，不使用浮点累计。

达到费用阈值后 Guard 排除账号。冷缓存、过期、完整分页失败、币种异常或已结束请求费用未知时也保持阻止，界面显示未知或过期，不把未知费用显示成零。重启后从宿主账本重建，不能通过重启清零。每次读取最多 100 页、每页 200 条，单账号 5 秒；维护批次最多 20 个账号、4 并发、8 秒预算。超过预算会显示未知并停止该账号分配。

这是已记录费用的**阈值保护**，不是原子预占预算：在途请求与记账延迟可能使最终金额超过阈值。在途数量单独显示。CPR 记录的 USD 成本与 OAuth 套餐的官方额度百分比是不同的计量口径，五小时／每周保护继续按宿主的原生额度事实运行。

### 额度界面与验证

额度管理页使用完整的浅色／深色样式默认值，并优先继承宿主主题变量；缺少主题注入时仍有卡片、输入、开关、进度条和按钮。保留搜索、账号筛选、刷新、撤销、版本冲突与精确保存。保存仅合并 Guard 配置，保留原有图片模型、账号白名单、驱动模型、并发和存储目录。

CI 除插件 SDK/RPC 回归外，还在精确固定的官方源码中运行真实 Provider / 路由 / HTTP SSE 合同测试；只有两组测试及三平台构建均成功才发布安装包。浏览器测试使用实际打包资源与沙箱，账号和费用为测试数据。以上仍不替代真实 CPR 实例中的上游生图和实际额度调度验收。

## 功能

- 原生 Images 接口：/v1/images/generations 和 /v1/images/edits。优先复用 CPR 自己的 Images Provider，不复制账号身份、不硬编码 originator。
- 兼容旧模型：支持用 Responses image_generation 工具桥接后返回 Images API 的 b64_json。
- 指定账号和分组：分别配置 imageAccountIds、imageGroupIds，两者取并集，仅在当前 Client Key 真正允许的候选中调度。
- 动态模型目录：使用官方 host.verify_client、host.check_access、host.data.accounts.list 做 Key、启用账号、分组及模型权限交集，保留原有对话模型并增补可调用的图片模型。
- 插件页面“画图测试”：选择 Key、对话主控模型、图片模型、尺寸和画质；六款预置提示词可编辑，支持后台任务、预览、下载、取消、耗时和结构化错误信息。页面通过管理桥提交任务，只使用 Key ID，不获取密钥明文。

### 为什么没有为独立 Images 注册 model_catalog v2 AdaptedModel

官方 model_catalog v2 的 AdaptedModel 以 upstream_adapter 为执行器；当前官方 upstream_adapter v5 仅覆盖生成模型的 Responses 类路径，不替代独立 Images HTTP 端点。硬注册一个假 Responses 模型会造成列表显示可用、实际调用却错误的行为。

针对独立 Images 模型，本插件在 HTTP 中间件里完成**受官方 Key 身份与账号授权复核的** /v1/models 合并；实际 Images 路由仍由 CPR 原生 Provider 执行。浏览器访问带 client_version 的 Codex 专用目录时，不将图片模型冒充对话模型混入该格式。

## 插件设置

| 参数 | 默认 | 作用 |
| --- | --- | --- |
| driverModel | gpt-5.6-luna | Responses 桥接和管理测试页的对话主控模型 |
| bridgeMode | auto | 外部 Images HTTP 的路由策略：auto 优先原生 Images，responses 使用工具桥接；后台画图任务固定走 Responses |
| timeoutMs | 300000 | Responses 桥接请求超时（毫秒） |
| imageAccountIds | 空数组 | 可执行生图的 OpenAI 账号 ID |
| imageGroupIds | 空数组 | 可执行生图的 OpenAI 账号分组 ID |
| imageModels | 内置 gpt-image 候选列表 | 允许发布和生成的 gpt-image-* 模型列表 |

未指定账号和分组时不开放插件生图。配置项由 CPR 的插件管理接口持久化并注入 Handshake，不使用环境变量伪造配置状态。

务必启用 middleware.http、middleware.request 和 scheduler 三个挂载，并把 scheduler 的插件故障策略设成 reject，防止插件异常时直接回退普通账号池。

## 画图测试页

安装并启用 management 扩展后，进入“插件管理 → Codex Image Bridge → 画图测试”。页面包含以下预设：

1. 骑车鹈鹕（卡通）
2. 微缩城市（等距 3D）
3. 雨夜未来（电影风）
4. 产品摄影（商业）
5. 日系田野（绘本）
6. 橙色图标（UI 设计）

页面通过管理桥的 api/jobs 提交任务，后台在维护回调中以选定 Key 的身份调用 host.model.execute_stream。后台自行复核白名单、所选驱动模型和额度保护；宿主继续执行账号授权、图片权限和并发租约。选择 Key 的驱动模型来自官方 host.models.list。

注意：此页面验证的是 Responses 的 image_generation 工具链，不能宣称已测试独立 Images 原生 HTTP 端点；后者需要通过带 Client Key 的标准 Images API 请求实测。

## 官方 Codex 对齐

依据 openai/codex 的 codex-rs/ext/image-generation/src/backend.rs 和 codex-rs/codex-api/src/endpoint/images.rs：
- 原生 Images 保留或生成 x-codex-image-turn-id，并保留合法可选 x-codex-turn-metadata。
- originator、User-Agent、version 和 Authorization 仍由 CPR 当前 OpenAI Provider 请求画像生成，插件不覆盖。
- 图片编辑 multipart 会转换为 Codex Images JSON（图片 data URL）；JSON 直接透传。
- 不修改原生成功 JSON 的 generation_id；Responses 桥接将图片工具事件转回 b64_json。

## 安装与兼容

GitHub Actions 使用固定官方 SDK 和官方 cpr-plugin packaging CLI，运行插件与宿主合同回归、三平台 Release 编译，并将画图页及 Quota Guard 页的六项 HTML/JS/CSS 资源打入 CPR 插件 tar.gz 包。

目标平台：Linux AMD64、Linux ARM64、macOS Apple Silicon。请根据实际服务器部署架构选包；当前不支持将该包安装到旧 RPC v2 的 CPR 宿主。

真实性边界：check_access 判断 Key/账号/模型政策的允许性，不是上游图像权限探针；真实 OAuth 生图、上游限流、图像尺寸兼容性仍需在授权测试环境验收。没有提供 S3/R2 存储、独立图片计费或异步批量任务。

## v0.5.0 联动修复与可视化配置生成器

已修复 v0.4.0 绘图测试列表为空的一个原因：之前把原生 Images 的 gpt-image-* 当作普通对话模型传入 host.check_access(upstream_model=...)，账号未在普通模型访问清单列出图片模型时被错误过滤。v0.5.0 对原生 Images 采用 check_access(upstream_model=None) 校验 Client Key 与账号/分组的有效交集；只有走 Responses 桥接的模型额外检查同一账号是否允许配置的 driverModel。模型展示不能代替上游真实生图授权检测，分组的 allowImageGeneration 仍由宿主在调用时执行。

新增 imageModels：数组指定可公开和执行的图片模型，最多 32 个不同合法 gpt-image-*。省略时沿用之前内置列表，明确设置 [] 时不公开图片模型。

新增页面顶部「插件配置生成器」：通过宿主只读账号事实查询可选账号名称、ID 和所属分组 ID，允许勾选与手动输入账号/分组 ID，图片模型多选、添加自定义模型、设置 driverModel、bridgeMode、timeoutMs，实时生成合法 JSON 并复制。由于管理页面当前缺少直接改写插件自身配置的官方 SDK 能力，此处**仅生成，不自动保存**：复制后到 CPR「插件管理 → 插件设置 → 插件参数」粘贴 JSON 并保存，再刷新画图测试页。

典型配置：

    {
      "timeoutMs": 300000,
      "bridgeMode": "auto",
      "driverModel": "gpt-5.6-luna",
      "imageModels": ["gpt-image-2", "gpt-image-1.5"],
      "imageGroupIds": ["替换为真实分组ID"],
      "imageAccountIds": []
    }

后台仍复核 Key、账号启用、分组范围和桥接主控模型权限；图片模型的官方服务器真实授权须调用实测才能确定。


## v0.5.1 配置生成器易用性

- 账号选择项现在优先使用 CPR 官方只读 AccountFacts.email 展示邮箱，下面仍保留账户 ID、分组；缺失邮箱时自动展示原有名称或 ID。该数据仅在已登录管理员的插件页面 / 管理 API 范围可见，不会进入 /v1/models 响应。
- 配置生成器改为可点击标题展开的原生 details 组件，默认折叠。首次打开画图测试页直接看到生成设置和预览；展开后所有账号、分组和模型选择、JSON 生成及复制功能不变。
- 不新增配置字段、不修改插件权限或账号池；升级 v0.5.0 至 v0.5.1 会沿用原配置。仍须将配置生成器导出的 JSON 复制到官方 CPR 插件参数并保存。


## v0.6.0 · Images 2.6 与异步多图测试台

2026-11-04 OpenAI 发布 Images 2.6。新增内置候选 gpt-image-2.6-sunburst、gpt-image-2.6-flare，并更新新安装时 imageModels 默认值；上游 OAuth 账号能否调用由实际权限决定。旧插件实例显式保存的 imageModels 保持原样：需要在“插件配置生成器”中勾选 2.6 并复制 JSON 到 CPR「插件参数」保存。已退役 gpt-image-1 不再属于默认候选。

### 上次选择记忆

管理测试页仅列出经官方 host.models.list / Key 账号权限复核、同时具有对话主控模型和绘图模型的启用 Key。已保存且仍有权限的 Key 优先恢复。
上次选择将保存在插件私有状态：Client Key ID、Responses 主控模型、图片模型、尺寸、画质、单次张数、页面并发上限。默认主控优先 gpt-6-luna，不存在则 gpt-5.6-luna；图片模型默认 gpt-image-2.5-sunburst。若原选择不再允许，自动回退到可用项。

支持 1024×1024、1536×1024、1024×1536、2048×2048、2048×1152、1152×2048、3840×2160 和 2160×3840，以及 auto。画质包含 auto、low、medium、high，较新的 2.5/2.6 型号还可选择 xhigh、max。旧款型号没有高分辨率或高画质权限时，界面自动禁用不可用选项。

官方 CPR 插件页面使用仅 allow-scripts 的隔离 iframe，浏览器 localStorage 不可靠，因此使用官方 host.state.get/put 存储选择。保存和恢复使用 image-workbench 私有命名空间及 CAS 版本，**不保存 API Key 明文、Prompt、生成图片**。存储属于同一个插件实例，管理员之间共享最后选择；状态存储失败时页面提示，绘图仍可继续。

### 多张与异步任务队列

每次点击可提交 1～10 张，每张通过一个独立的官方 models.responses 生图工具请求执行。页面允许上一批未完成时继续提交下一批，默认同时执行 2 个，用户可以选择 1～3。每张有独立的排队、生成、完成、取消和失败状态及请求 ID，可取消单张或停止全部，完成后分别保存 PNG。

前端在最多 30 个任务卡片和 12 张结果 PNG 之间回收内存，4K 大图分片 Base64 解码成 Blob URL；清理任务或关闭页面时回收资源。

这里的异步是“页面打开期间多请求并发”，不是后台队列：关闭或刷新页面会中止正在请求的任务，图片和任务历史不持久化。使用标准 Images API 时，原生 n 参数支持 1～10，但本管理测试页为便于独立取消和按账号分配，每张单独调用一次 Responses。

### 账号并发、负载均衡

此插件 SDK 固定的官方 CPR 提交 96e8443 的 docs/api.md 将 maxConcurrentPerAccount 初始值列为 3，单账号 concurrencyLimit 可以覆盖全局默认值。Core 对 Key 和账号进行配额/并发租约管理；插件仅缩小允许生图的账号候选范围并 DelegateFiltered，最终由原生选号策略平衡负载，不会强行绑定某一个账号。

会话亲和的“优先”或“严格”模式可能让同一会话坚持使用主账号。管理测试页每张独立创建请求且不提供 previous_response_id，通常可以分配到不同账号。账号池没有空位时按 CPR 的 maxWaitingPerKey、maxWaitingPerAccount 和超时设置排队，或返回明确并发失败；上游还可能施加独立的速率限制。建议只有一个并发额度为 1 的账号时，页面并发设为 1；多个生图账号时再选 2 或 3。

资料：https://openai.com/index/introducing-images-2-6/ 、 https://developers.openai.com/api/reference/resources/images/methods/generate 和 https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/docs/api.md

### 验收覆盖

Node --check、rustfmt、真实 Edge headless 模拟 host bridge 验证偏好恢复、Key 过滤、6 Luna 不存在时回退 5.6、4K、两批各 2 张的独立并发结果、总取消不会启动后续任务；并用 GitHub Actions 官方 SDK 完成 Cargo 单测、三平台 Release 构建及可安装包验证。此处不宣称已通过真实 OAuth 上游的生产生成验收。


## v0.7.0 — 后端持久任务与额度保护合并

### 后端任务如何运行

这一版**不再由浏览器直接发起 models.responses 并维持生图连接**。
管理页 POST api/jobs 把当前 Key ID、图片模型、Prompt、尺寸、画质和张数保存到 CPR 官方插件私有状态 image-jobs；
宿主对该插件的 maintenance 定时维护回调与浏览器访问无关，大约每 30 秒领取一次队列。
领取后由 host.model.execute_stream 通过 CPR Core 执行并按原生 Key、账号、租约、配额和计费规则结算。
每个任务通过管理接口 GET api/jobs 轮询状态（queued/running/succeeded/failed/interrupted/canceled），前端每 0.1 秒根据服务器 startedMs 更新计时。PNG 使用文件写入到服务器持久目录，浏览器通过 POST api/jobs/image 按 256 KiB 分块下载并预览。

**必须先在插件配置中设置 imageStorageDir 为服务器绝对路径，保证该路径存在或可被插件运行用户创建。Docker/容器部署必须绑定持久卷。** 若为空，后端任务不接受提交。
例：imageStorageDir=/var/lib/cpr-image-bridge/images，backgroundConcurrency=2。
请勿使用 /tmp、容器未挂载路径或可能被清理的缓存目录作为永久结果存储。服务器关闭或插件进程替换期间，排队中的任务仍保存在 CPR 私有状态；先前已经在上游执行的 running 任务如果 PNG 已经完成原子落盘，会自动恢复为 succeeded；如果没有有效完成文件，则标为 interrupted，**不自动重试**，以避免上游已扣费但下游还没存储的情况下重复执行。
网页关闭、断网或刷新只影响轮询和预览，不会取消已经交给后端的维护任务。显式点“取消”会标记任务不再交付图片；若上游已受理，取消仍可能产生计费。
状态由同一 CPR 实例的管理员共享，不是 per-browser。插件状态包括 Prompt（属于业务敏感信息），只有具有插件管理权限的管理员可访问；API Key 明文不写入状态或 PNG 文件。

本版本保留 900 秒 maintenance 父调用预算、每轮最多 backgroundConcurrency 个并发任务。后台任务不是无限期守护 daemon：维护被取消、插件升级或服务器关闭会导致当前 in-flight 请求中断，重启后明确标记 interrupted 而不是重试；任务完成后保存在 imageStorageDir 的 PNG 可以在后续管理会话中继续读取。任务队列最多 30 项，保存时会回收最旧已结束记录及对应图片。

**协议边界**：固定官方 SDK 对应宿主的默认 `maximum_buffered_body_bytes` 是 **1 MiB**，插件的 `host.model.stream_read.maximum_bytes` 使用该预算。SDK 的 GPE2／HME1 编解码上限 **8 MiB** 是另一层限制，不能作为宿主允许的读取额度。原生 Images 的完整 JSON/Base64 事件仍必须装入宿主缓冲预算；超过时会返回 `Capacity`，改用更大的读取参数或 buffered execute 不能解除限制。先验收 1024 WebP，再单独验证 2K／4K；大型结果需要宿主提供相应预算或官方大对象交付能力。

任务的 `diagnostic` 保存失败阶段、操作、模型、尺寸、输出格式、SDK 错误类型、请求 ID、HTTP 状态和上游发送三态；同一组安全字段尽力提交到 `host.log`。`upstreamSent: null` 表示无法确认，得到执行句柄不等于证明上游已发送，流读取失败的 `NotSent` 也不等于整次生图未发送。诊断不保存提示词、账号、密钥、OAuth Token、完整请求体或原始错误正文。失败后只关闭原流，不重新发起生图；已有旧任务没有此字段时仍可读取。

### 合并 cpr-quota-guard

基于 Mr-xiongyq/cpr-quota-guard v0.2.3、Apache-2.0（详见 THIRD_PARTY_NOTICES.md），将 5 小时、周额度证据检查加入 Image Bridge 的同一个 scheduler 回调，并提供以下新插件配置：

- quotaGuard: 数组，每项 {accountId, fiveHourLimitPercent, weeklyLimitPercent}；例如两个百分比均为 95。数组为空时不启用额度保护。
- maxObservationAgeSeconds: 默认 300；达到阈值、缺少/过期额度证据或窗口不可用时排除对应受保护账号。
- Image Bridge 自身维护任务还会定期刷新配额观测，额度是否恢复仍以 CPR 宿主新事实为准。

额度过滤对该 scheduler 绑定范围内的所有 OpenAI 请求有效，与生图账号白名单使用集合交集；宿主为后台生图的模型子调用屏蔽发起者的 scheduler 时，后台工作器会独立调用同一份额度筛选，选定 Key 当前允许、可生图且未触发保护的账号后，显式把账号交给 Core 执行。

**这并不会自动消除两个仍在启用的 scheduler 绑定的冲突。** 升级后必须在原 cpr-quota-guard 插件的高级设置中**禁用其账号调度绑定**或停用原插件调度，确保只保留 Image Bridge 的调度。新的单 scheduler 才能兼容两种账号过滤行为。若用户不希望合并，也可以保持 quotaGuard=[]，并通过不同 Client Key 的作用范围分区避免重叠。

### 典型参数

{
  "timeoutMs": 300000,
  "bridgeMode": "auto",
  "driverModel": "gpt-5.6-luna",
  "imageModels": ["gpt-image-2.6-sunburst", "gpt-image-2.5-sunburst"],
  "imageGroupIds": ["实际分组ID"],
  "imageAccountIds": [],
  "imageStorageDir": "/var/lib/cpr-image-bridge/images",
  "backgroundConcurrency": 2,
  "quotaGuard": [
    {"accountId": "实际账号ID", "fiveHourLimitPercent": 95, "weeklyLimitPercent": 95}
  ],
  "maxObservationAgeSeconds": 300
}

后端状态经 host.state.get/put 的 CAS 版本控制，文件按 UUID 路径管理并做 PNG 签名校验，下载每次至多传回 256 KiB，不通过巨型数据 URL 返回图片。当前未在生产账号实测 OAuth 生成/大图片，CI 编译与浏览器模拟只能证明代码路径，不能替代生产验收。

### 多副本部署边界

当前宿主的插件维护工作器在**每个实例的本地进程**内串行运行，并通过实例私有状态做 CAS 持久化。后台任务目前按“一个插件实例只有一个有效维护工作器”设计；如果高可用部署让两个宿主副本同时维护相同的插件实例，它们可能观察到另一副本已经领取的 running 任务并将其误判为 interrupted。因此正式生产建议单实例执行该 worker，或在 CPR 宿主增加跨副本租约/owner 与 fencing token 后再启用多副本后台执行。

服务端单张图像最长受 maintenance 父调用预算 900 秒限制；等待最多约 30 秒才会由维护轮询领取，若需要立即响应、长于预算或可跨宿主副本自动恢复，应升级到具备专用持久任务 worker 和租约队列的宿主 API/独立服务。


## v0.7.1 CSP 图像显示修复

CPR 官方插件 iframe 的 CSP 为 img-src data:，不允许 blob:。v0.7.0 从后台持久队列以二进制分片读取 PNG、用 URL.createObjectURL 显示，导致 blob:null/... 被拦截。v0.7.1 直接将 PNG 分片按 3 字节对齐编码为 data:image/png;base64 URL，保证跨任意大小分片边界时 Base64 不破坏 PNG；保留 120 MiB 最大图片约束和文件头检查。原 Responses 直接返回的 Base64 图片同样使用 data URL。

不更改合并后的 Quota Guard 五小时/每周额度保护、宿主维护/持久队列、账号路由或图片模型配置。测试覆盖真实的 img-src data: CSP、后端二进制分片模拟、图片的自然宽高加载与多张图片独立显示。


## v0.7.2：4K、WebP 优先、临时图片与 Quota Guard 原生管理页

### 图片格式与 4K

依据 OpenAI 的 Images API 和 Responses image_generation 官方协议，两条路径都支持 webp、png、jpeg 和 output_compression（压缩参数只对 WebP/JPEG 有效）。插件发起请求前按模型 ID 选择格式：明确列出的 GPT Image 1.5、2、2.5、2.6 默认 WebP、output_compression=85；自定义/未知模型预先选择 PNG 且不发送 output_compression。**不会在失败后重试另一次收费生图**。不再默认优先 JPEG。

对于官方支持的自定义尺寸，原样传递 3840x2160 或 2160x3840 等 WIDTHxHEIGHT，并在返回时验证实际图片格式、宽高；若返回 1024x1024 则报告请求尺寸与实际尺寸不一致，不计为成功 4K。超过单个宿主模型事件容量的返回仍可能失败；WebP 优先是减少此风险，不是伪造无大小限制的传输。

参考：
- https://developers.openai.com/api/reference/resources/images/methods/generate
- https://developers.openai.com/api/reference/resources/responses/methods/create
- https://developers.openai.com/api/docs/guides/image-generation

### 临时文件

不再要求设置 imageStorageDir。新安装使用系统临时目录下的 cpr-image-bridge/images（例如 /tmp/cpr-image-bridge/images）；文件扩展名 .img 与任务 UUID 关联，客户端通过管理分片接口加载图片，不通过公共 URL 暴露。宿主设置 CPR_DATA_DIR 不强制改变临时存储；容器重启或临时目录清理后，旧结果可能丢失。旧配置中的 imageStorageDir 字段保留兼容用法，若用户明确设置了绝对路径会继续使用。

后台图片可以是 PNG、WebP、JPEG：文件格式由内容识别，元数据记录 resultMime、actualWidth、actualHeight，页面按真实 MIME 构造 CSP 允许的 data URL 和带正确扩展名的保存链接。兼容已有 .png 后缀的结果文件恢复。

### 账号额度和并发

新的 concurrencyPerAccount 默认 8（可配置 1 到 32），不再以 backgroundConcurrency=3 限制整池并发。执行任务以账号 ID 使用独立信号量，候选账号池容量按当前可用账号个数累加；最终仍以 CPR Core 的账号并发租约、Key 配额和原生调度允许容量为准，若宿主账号并发上限低于 8，请在 CPR 自身设置中调大后再并行执行。

保留 Quota Guard 的 5 小时和每周额度保护。新增独立管理页「账号额度保护」，**直接移植 Mr-xiongyq/cpr-quota-guard 的 Vue 账号卡片、额度进度条、账号选择和保存交互**（Apache License 2.0）；仅对 API 路径和保存的宿主配置合并做适配，不会用保存额度规则覆盖图片模型、账号白名单等其它 Image Bridge 配置。源代码位于 quota-ui/，构建资源位于 web/quota/。本地模拟测试不能代替真实 CPR 宿主管理员授权、真实 OAuth 生图和额度交互验收。

官方 CPR 的侧栏按插件实例显示一个入口。打开「画图测试」后，在页面顶部的分段切换器选择「账号额度保护」；第二页不会另外生成一个侧栏项。若顶部也没有切换器，应核对实际宿主版本及该实例管理目录返回的 pages，并确认加载的是预期制品。页面使用管理桥访问 `quota/overview`、`quota/accounts` 和 `quota/configuration`；保存只合并额度字段，并通过实例 revision 防止覆盖并发配置。

仍须停用原 cpr-quota-guard 的 scheduler 绑定，保留 Image Bridge 的单个 scheduler；使用失败时拒绝策略，防止额度保护筛选失效。


## v0.7.3：修复插件状态版本升级冲突

症状：在 CPR 插件管理里从 v0.7.1 切换 v0.7.2 时，switch-version 端点报「插件状态版本不兼容或已发生并发变更」。

根因：v0.7.2 把 image-workbench 状态中 parallelism.maximum 从 3 改成 32，却仍声明 schemaVersion=1、migratesFrom=[]。官方 CPR PostgreSQL State Store 同时比较 schemaVersion 和 Schema SHA-256；内容变更但未声明迁移时，transition_required 会拒绝更新，而不是静默改写旧记录。

本版修复：

- image-workbench 的 schemaVersion 升为 **2**，声明 migratesFrom=[1]。
- 注册官方 plugin.state.migrate 处理器，按宿主提供的批次逐项返回 Keep，完整保留以前的 last-selection 设置（Client Key ID、对话模型、绘图模型、尺寸、画质、数量、并发数）；v1 原有并发范围 1..3 完全包含在新版 1..32 中，无须转换。
- image-jobs 私有状态（任务记录）Schema 与版本 **保持 1 不变**，不进行图片队列迁移、不删除结果文件。
- CPR 版本切换可能在迁移时短暂停用实例，迁移失败时由宿主保护原数据，不能在调度任务进行中强制重启。
- 可以从 v0.7.1 **直接切换到 v0.7.3**，不需要先切到 v0.7.2；安装并接受目标制品后通过插件管理「切换版本」。若提示配置 revision 发生并发变化，刷新插件列表后核对最新状态再重试，不删除插件实例、不清空状态。

宿主源码依据：gateway-store/src/postgres/plugins/state.rs 的 transition_required / begin_transition；gateway-plugin/runtime/src/generation/state.rs 的受管迁移；SDK call/host.rs 中 StateMigrationRequest 与 StateMigrationChange::Keep。修复没有改变 Quota Guard、WebP/PNG/4K、默认临时目录、每账号并发或真实调度策略。

验证包含：Rust 迁移处理器单元测试、manifest 目标 schema 声明比对、官方 GitHub Actions 三平台编译和打包校验。尚未在用户实际 CPR 宿主中执行带真实 PostgreSQL 的升级迁移，生产中的并发变更仍可导致另一类 409。
