# CPR Image Bridge v0.6.2 (official SDK)

独立 Rust 插件，面向 zyycn/codex-proxy-rs 官方主线。SDK/打包工具固定源码提交：96e8443ede051abdd8f0bac69462a7cfe2945633。宿主要求 RPC v7、middleware v6、scheduler v3、management v2，MSRV Rust 1.99.1。旧 fork 的宿主与 SDK 不作为运行目标。

## 功能

- 原生 Images 接口：/v1/images/generations 和 /v1/images/edits。优先复用 CPR 自己的 Images Provider，不复制账号身份、不硬编码 originator。
- 兼容旧模型：支持用 Responses image_generation 工具桥接后返回 Images API 的 b64_json。
- 指定账号和分组：分别配置 imageAccountIds、imageGroupIds，两者取并集，仅在当前 Client Key 真正允许的候选中调度。
- 动态模型目录：使用官方 host.verify_client、host.check_access、host.data.accounts.list 做 Key、启用账号、分组及模型权限交集，保留原有对话模型并增补可调用的图片模型。
- 插件页面“画图测试”：选择 Key、对话主控模型、图片模型、尺寸和画质；六款预置提示词可编辑，显示 PNG、取消生成、下载图片、错误信息和请求 ID。页面直接使用宿主注入的 models.responses 安全桥，不获取密钥明文。

### 为什么没有为独立 Images 注册 model_catalog v2 AdaptedModel

官方 model_catalog v2 的 AdaptedModel 以 upstream_adapter 为执行器；当前官方 upstream_adapter v5 仅覆盖生成模型的 Responses 类路径，不替代独立 Images HTTP 端点。硬注册一个假 Responses 模型会造成列表显示可用、实际调用却错误的行为。

针对独立 Images 模型，本插件在 HTTP 中间件里完成**受官方 Key 身份与账号授权复核的** /v1/models 合并；实际 Images 路由仍由 CPR 原生 Provider 执行。浏览器访问带 client_version 的 Codex 专用目录时，不将图片模型冒充对话模型混入该格式。

## 插件设置

| 参数 | 默认 | 作用 |
| --- | --- | --- |
| driverModel | gpt-5.6-luna | Responses 桥接和管理测试页的对话主控模型 |
| bridgeMode | auto | auto：优先原生 Images；responses：通过 image_generation 工具桥接 |
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

页面通过官方 models.responses({clientKeyId, body, signal}) 以选定 Key 的身份调用完整 Responses 请求链，图像工具请求在 request middleware 被识别并由 scheduler 按账号范围过滤。选择 Key 的驱动模型来自官方 host.models.list。

注意：此页面验证的是 Responses 的 image_generation 工具链，不能宣称已测试独立 Images 原生 HTTP 端点；后者需要通过带 Client Key 的标准 Images API 请求实测。

## 官方 Codex 对齐

依据 openai/codex 的 codex-rs/ext/image-generation/src/backend.rs 和 codex-rs/codex-api/src/endpoint/images.rs：
- 原生 Images 保留或生成 x-codex-image-turn-id，并保留合法可选 x-codex-turn-metadata。
- originator、User-Agent、version 和 Authorization 仍由 CPR 当前 OpenAI Provider 请求画像生成，插件不覆盖。
- 图片编辑 multipart 会转换为 Codex Images JSON（图片 data URL）；JSON 直接透传。
- 不修改原生成功 JSON 的 generation_id；Responses 桥接将图片工具事件转回 b64_json。

## 安装与兼容

GitHub Actions 使用官方 SDK 提交和官方 cpr-plugin packaging CLI，运行 Cargo 单测、Release 编译，并将 web/index.html、web/app.js、web/app.css 作为资源打入 CPR 插件 tar.gz 包。

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


## v0.6.1：图片任务实时耗时与账号调度绑定兼容说明

- 每个进入“生成中”的图片任务，从 0.0s 开始显示每 0.1 秒更新的计时；用 performance.now() 单调时钟计时，任务完成/失败/取消时固定耗时并停止跳动。
- 多个并行任务共享一个 100ms setInterval，计时更新只修改任务状态标签文本，不重新绘制图片和卡片。所有运行任务结束或页面关闭后会清理 Interval。排队时间不计入生成中时长，未开始就取消的任务不显示伪造耗时。
- 浏览器读屏器不会被 100ms 状态更新持续打扰：任务网格 aria-live=off，只对汇总任务数播报。
- Edge headless 测试验证 0.0s 起跳、实时秒数推进、完成及单任务取消后耗时冻结和结束计时器；其它队列和模型配置逻辑保持 v0.6.0 行为。

### 与其他账号调度插件共存

官方 CPR runtime 的 PluginRequestPolicyPlan::compile 对所有启用 scheduler 绑定对比 BindingScope::overlaps；若两者有可能匹配同一请求，拒绝启动并报“账号调度绑定作用范围重叠”。这是宿主静态检验，调整执行顺序或失败策略无效。

静态证明互斥需双方的 **Client Key ID**、**Provider** 或 **模型 ID** 范围在同一维度上都明确收窄且完全不相交；账号分组不作为互斥证明（同一 Key 可命中多个分组）。绑定“应用于所有请求”的插件会与任何同条件调度插件重叠。

Image Bridge v0.6.1 不自动修改调度绑定，也不合并第三方调度器。若有独立 cpr-quota-guard 等全局额度保护 scheduler，应通过专用 Client Key 分割范围，并在另一插件中明确排除这些 Key；或将额度保护过滤与图片账号白名单合入同一个 scheduler 后停用旧的调度绑定。合并后仍由 CPR Core 执行租约和负载均衡，并保留 fail-closed 的账号权限限制。


## v0.6.2 CSP 图片预览修复

CPR 官方管理插件 iframe 的 CSP 允许 img-src data:，但不允许 img-src blob:。
因此 v0.6.0 / v0.6.1 将大型 Base64 图片转为 blob:null/... URL 的处理，会被浏览器拦截，导致画图成功但预览为空。v0.6.2 恢复使用 data:image/png;base64, 前缀，不再调用 URL.createObjectURL。保留 160 MiB Base64 大小上限、PNG 文件头检查、多张图片的清理与独立任务状态。测试将模拟与生产相同的 img-src data: CSP。
