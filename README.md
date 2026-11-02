# CPR Image Bridge v0.3.0 — Codex 原生生图兼容插件

独立 Rust 插件，优先复用 CPR 原生 Images 路由；仅旧模型或显式选择兼容模式时，才转换到 Responses image_generation。不会自行读取 OAuth Token 或绕过宿主认证。

## 官方 codex-rs 对照

固定参照 openai/codex 提交 ba0d0e1f74ee0f41c27ef24b37ed5fc030b487e7：

- codex-rs/ext/image-generation/src/tool.rs：内置 image_gen 工具默认使用图片模型 gpt-image-2。
- codex-rs/ext/image-generation/src/backend.rs：CodexImagesBackend 为独立 Images 请求提供 x-codex-image-turn-id 和可选 originator；API Provider 和账号认证由当前会话决定。
- codex-rs/codex-api/src/endpoint/images.rs：JSON 发送至 images/generations 或 images/edits；提取响应头 x-codex-imagegen-request-id；保留独立图片的 generation_id。
- codex-rs/codex-api/src/endpoint/session.rs：Provider URL、授权头和重试由传输层实现。

插件实现：

1. 对原生 Images 请求保持原有 JSON 字节和客户端合法 x-codex-image-turn-id；普通客户端缺失时由宿主 HTTP call ID 生成相关 ID。
2. 不伪造 originator、Authorization、Cookie；CPR 为选定账号构建认证和请求画像。
3. 原生编辑接受官方 JSON 图片引用；传统 multipart 编辑先转换为同语义 JSON images 数组后转发。
4. 原生 JSON 成功响应不做非必要重写；Responses 兼容桥接在上游返回时保留 x-codex-imagegen-request-id。
5. 原生 Images 支持并保留 CodexImagesBackend 可选的 x-codex-turn-metadata；该 Header 由 CPR Provider 做安全过滤。Responses 兼容请求不携带 x-codex-image-turn-id。

实际 Headers 仍取决于 CPR 的 Provider 请求画像、部署代理及调用客户端，不能确保与官方工具的所有网络指纹字节相同。

## 插件配置

通过 CPR 插件管理 -> 设置填写（读取宿主 Handshake.configuration）：

| 参数 | 默认值 | 含义 |
| --- | --- | --- |
| driverModel | gpt-5.6-luna | 仅 Responses 兼容桥接使用的对话主控模型 |
| bridgeMode | auto | auto 优先原生 Images；responses 强制 Responses 工具桥接 |
| timeoutMs | 300000 | 图片桥接请求超时 |
| imageAccountIds | [] | OpenAI 账号 ID 白名单 |
| imageGroupIds | [] | OpenAI 分组 ID，和账号白名单取并集 |
| syntheticCatalog | false | 虚拟模型兼容目录开关（见下文警告） |

未注册的图片模型或非法 model 字段在插件入口直接返回 400，不能绕过图像账号策略转发给普通账号池。未指定账号或分组时，插件覆盖的生图请求直接拒绝。普通对话模型的请求委托 CPR 原生调度。生图模型的调度只在 CPR 已完成 Key/账号范围过滤后的候选账号内选取，先匹配指定账号与分组，失败拒绝，不回退给其它账号。

必须同时启用 middleware.http、middleware.request 和 scheduler 绑定，scheduler 的故障策略应为 reject（不能使用 delegate，否则无法保证账号限制）。将它们绑定到需要生图的 Key 范围。插件覆盖的原生 Images 与 Responses 桥接请求都由 HTTP 中间件添加受保护的内部意图头；在 HTTP 阶段清除客户端伪造值，认证后 request 阶段才登记并移除，不传给上游。原生 Images 的模型提示可能为空，因此不能仅依赖模型 ID 判定账号范围。

## /v1/models 的已知 SDK 边界

当前 CPR middleware v3 的 HTTP 挂载在 Key 鉴权之前，只能得到下游已认证的模型列表，不能取得 Key 对应的实际可用账号集合。仅依据 driverModel 是否出现在模型列表中增补虚拟绘图模型，可能误把绘图模型展示给不能访问所指定生图账号的 Key。

因此，syntheticCatalog 默认关闭，保留 CPR 原生普通和图像模型列表，不虚报图片权限。仅在管理员已经将 Key 独立绑定到指定生图账号分组并完成权限验证时，才可明确选择兼容开关 syntheticCatalog=true；这仍不是严格的按账号交集验证。

要严格实现同一 Key 的普通/生图模型按账号交集自动可见，必须扩展宿主认证后的 Key 账号事实查询或模型目录发布 SDK 合同，本插件版本没有声称完成该项。

## API

- POST /v1/images/generations：原生 Images 优先。
- POST /v1/images/edits：JSON / multipart 编辑，支持 data URI。
- GET /v1/models、GET /v1/models/{id}：默认原生目录；可选 legacy 虚拟图片目录。

示例：POST /v1/images/generations，Content-Type: application/json，客户端 Key Bearer 身份，JSON 正文为 {"model":"gpt-image-2","prompt":"a pelican riding a bicycle","size":"1024x1024","quality":"auto"}。

源代码固定 CPR SDK 提交 f9b9dfefe7a2706d625d6476c87bde0332290979（SDK 0.1.0、manifest v2、middleware v3、scheduler v1）。在独立插件仓库运行 cargo generate-lockfile、cargo test --locked、cargo build --release --target 对应平台；使用 CPR 官方 cpr-plugin package 工具构建 .tar.gz 安装包。CI 覆盖 Linux x86_64、Linux ARM64、macOS Apple Silicon；结果以实际执行状态为准。

目前没有完成真实账号的 OAuth 生图验收。不存在图片计费独立账本、异步任务、S3 图片存储或自动探测生图账号功能。