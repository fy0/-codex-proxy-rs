# CPR Image Bridge v0.4.0 (official SDK)

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
