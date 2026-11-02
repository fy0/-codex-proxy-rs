# CPR Image Bridge（Codex Proxy RS 独立图片插件）

通过 `codex-proxy-rs` 的 **HTTP middleware v3** 提供接近 Sub2API 的生图体验：在标准 `/v1/models` 模型列表追加 `gpt-image-*`，并在请求 `/v1/images/generations`、`/v1/images/edits` 时按模型选择 Codex 原生 Images 或 `Responses + image_generation` 工具。

> **适用范围**：`codex-proxy-rs` **v3.14+ 且启用 middleware v3 / manifest v2** 的宿主；源码按 SDK 0.1.0 及 `f9b9dfefe7a2706d625d6476c87bde0332290979` 接口编写。这个插件**不是**通用 OpenAI API Key、ChatGPT Plus/Pro 生图权限的授予器。是否能生图由目标账号的实际能力决定。仅有 ChatGPT 订阅并不保证上游支持。安装包的目标平台必须与服务端相同。

## 路由行为

| 输入 | 插件处理 | 备注 |
| --- | --- | --- |
| `GET /v1/models` | 在宿主真实列表后追加图片模型 | **仅**当当前 Key 的列表中可见配置的 Responses 主控模型时添加；保留其他模型；不改 `?client_version=…` 的 Codex 专用目录 |
| `GET /v1/models/gpt-image-1` 等 | 根据当前 Key 可见模型目录合成单模型详情 | 无资格返回 404 |
| `POST /v1/images/generations` | `gpt-image-1` 默认桥接至 `/v1/responses` | 输出 OpenAI Images 风格 `data[].b64_json` |
| `POST /v1/images/edits` | 同上，支持 JSON `image`/`images` 和 multipart 图片上传 | 将图片转成 `data:image/...;base64,...`，转发给 Codex `input_image` |
| 新图片模型：`gpt-image-1.5`、`gpt-image-2`、`gpt-image-2.5-*` | 默认**原生 Images 透传** | 模仿 Sub2API 的 Codex Direct Images 逻辑；multipart 编辑转为原生 Images JSON |
| 插件设置中 `bridgeMode=responses` | 所有列出的图片模型均经 Responses 图片工具 | 用于某账号原生 Images 路径不可用而 Responses 工具可用的情形 |
| 未知图片模型、其他 HTTP API | 交回宿主，不改变已有默认路由 | 不拦截 Chat/Responses |

虚拟模型清单是 UI 发现辅助，**不等于每个账号都具备模型权限**。由于现有 SDK `model_catalog` 仅允许真实目标模型的别名，不能创建全新的图片能力，本插件把可见名称放在 `GET /v1/models` **HTTP 响应层**，而非声明 `model_catalog`。宿主对每个转换后的 `/v1/responses` 请求仍执行原有的 **Client Key 认证、账号组、调度、并发、路由和账本**；但宿主执行模型是 Responses 主控模型，**无法凭此虚拟图片模型 ID 自动实现额外的逐图片模型 Key 白名单及精确图片张数计费**。部署方应给插件适当访问范围并自行配置合理的限额与图片使用策略。

## 插件所需条件

- 受信任的 Rust 独立进程插件：`manifestVersion=2`、`runtime=trustedProcess`、`middleware.version=3`、`stages=["http"]`；没有额外声明账号、密钥、网络、管理页面等能力。
- 宿主具有可用的 OpenAI OAuth 或能够执行对应模型的 Key 账号，当前客户端 Key 能访问配置的 Responses 主控模型（默认 `gpt-5.6-luna`）。默认值可根据当前实际可用的 Codex 模型调整。
- **宿主必须已提供** `/v1/responses`、`/v1/images/generations`、`/v1/images/edits` 以及 `/v1/models` 路由。插件不会绕过授权路由独自向 ChatGPT 发请求，也不读取、保存 OAuth token。
- **模型切换在 CPR 管理端完成**：插件管理 → Codex Images API Bridge → 设置 → `Responses 对话主控模型`，输入当前客户端 Key 可见且支持 `image_generation` 的对话模型，例如 `gpt-5.6-luna` 或已实际验证的其他模型。保存并启用后，宿主会发布新实例配置，插件从新会话握手读取；不需要改环境变量或重新编译。
- 这些设置在安装清单 `configurationSchema` 中声明，默认自动填入，运行时读取 `Handshake.configuration`，不从请求参数中覆盖。

| 插件设置字段 | 默认 | 作用 |
| --- | --- | --- |
| `driverModel`（Responses 对话主控模型） | `gpt-5.6-luna` | 用来驱动 `image_generation` 工具的对话模型，不是客户端传入的 `gpt-image-*` 图片模型 |
| `bridgeMode`（生图路由策略） | `auto` | `auto`=原生/工具自动分流；`responses`=全部映射图片模型强制走 Responses 工具 |
| `timeoutMs`（图片请求超时） | `300000` | 30,000 到 600,000 毫秒；宿主/反代超时可能更短 |

注意：对话模型切换不会自动赋予账号图片权限。必须确保主控模型在当前 Key 可用的模型目录中，且实际支持 image_generation 调用。

## 编译和打包

**先使用支持目标 SDK 的 Rust 工具链（建议 1.97）。** 此源码是一份独立 Cargo package，不要加入 `codex-proxy-rs/backend` 的 workspace。源码 `Cargo.toml` 将 `gateway-plugin-sdk` 固定到一个 Git SHA，防止跟随上游 main 漂移。

```bash
cargo test --locked
cargo build --release --locked --target x86_64-unknown-linux-gnu
```

首次生成锁文件需要执行 `cargo generate-lockfile`，检查结果并提交，随后才使用 `--locked`。可以运行 `bash scripts/package-linux.sh` 一次性执行格式化、测试、构建及调用插件 CLI；脚本需要可用的 `cpr-plugin` 命令，并在本地已经完成锁文件准备。

```bash
cpr-plugin package \
  --manifest ./plugin.json \
  --binary ./target/x86_64-unknown-linux-gnu/release/cpr-image-bridge \
  --target x86_64-unknown-linux-gnu \
  --output-dir ./dist
```

`cpr-plugin` 由宿主 `backend/apps/plugin-cli` 编译安装，需与目标 SDK 兼容。官方 CLI 会生成可安装的 `.tar.gz` 和 `.sha256`，**不能仅用 `tar` 打包源码伪装成安装包**。在 CPR 管理端「插件管理 → 安装 → 上传包」上传 `.tar.gz`，审阅权限、版本与摘要后启用；HTTP 中间件默认自动绑定，但应检查范围和优先级。

仓库 `.github/workflows/check.yml` 使用 GitHub Actions 构建 Linux x86_64、Linux ARM64、macOS ARM64 并调用官方 `cpr-plugin` 打包工具生成真实可安装的插件包；CI 结果以实际运行日志为准。Windows 开发机如果没有足够磁盘，建议直接将这份独立插件工程放在 Linux 构建机或 CI 上编译，避免触碰宿主的大型 `backend/target`。

## 调用示例

文生图：

```bash
curl -sS https://your-cpr.example.com/v1/images/generations \
  -H 'Authorization: Bearer YOUR_CPR_CLIENT_KEY' \
  -H 'Content-Type: application/json' \
  --data '{"model":"gpt-image-1","prompt":"一只骑自行车的鹈鹕","size":"1024x1024"}'
```

图片编辑（标准 multipart，桥接时上游使用 Codex `input_image`）：

```bash
curl -sS https://your-cpr.example.com/v1/images/edits \
  -H 'Authorization: Bearer YOUR_CPR_CLIENT_KEY' \
  -F 'model=gpt-image-1' -F 'prompt=让背景变成晴天' \
  -F 'image=@input.png;type=image/png'
```

默认成功结果：`{"created":..., "data":[{"b64_json":"...","revised_prompt":"..."}]}`。请求 `stream=true` 时提供**生成完成后**的 SSE `image_generation.completed` + `[DONE]`，没有伪造实时 partial-image 进度；由于先聚合完整结果，这个选项不减少首包延迟。

## 已知限制

- 未实现 Sub2API 的 S3/R2 对象存储、异步 Images Tasks、批量图片排队、独立图片倍率/计费、账号能力探测与冷却；这些需要新的后端服务合同或独立状态/调度策略。
- 桥接返回 Base64（不支持 `response_format=url`）；支持 PNG/JPEG/WebP/GIF 的 multipart 图片编辑（每张最多 50 MiB）。JSON 编辑输入支持单个或数组形式的 HTTPS / data:image URL；不接受用户传入任意本地文件路径。正文最大 55 MiB，上游响应最大 160 MiB。对 Responses 上游输入另有 60 MiB 编码上限。
- `stream=true` 是**最终事件 SSE**，非逐图片增量生成；一次请求内部聚合上游 SSE，错误发生时在输出前返回 HTTP 错误，不能保证完全兼容使用 partial-image 的客户端。
- 当前采用 Responses `image_generation` 工具 `action`、`model` 等字段格式，实际可用性由上游接口决定。账号缺少权限、被限流、模型不存在等错误将沿用宿主转发错误或返回安全的标准错误信封；上游只返回文本或中途失败，不会冒充图片生成成功。
- HTTP middleware 在宿主原生认证**之前**运行，因此解析及大文件处理受插件本身大小上限保护；公网部署仍需使用宿主/反代请求体大小和频率限制。该插件属于完整信任进程，不构成安全沙箱。
- 目前进行了源码审查和独立 `rustfmt` Rust 2024 语法解析（通过），**没有完成 Cargo 编译、目标 CPR 插件安装与真实 OAuth 生图测试**。不能据此宣称插件已经可直接在生产环境使用。

## 主要源码

- `src/main.rs`：HTTP 挂载、模型列表/详情、上游请求/响应路由与限制。
- `src/bridge.rs`：Images JSON/multipart 参数校验和 Responses image_generation 调用构造。
- `src/response.rs`：Responses JSON/SSE 解析、图片结果去重、Images 标准响应。
- `src/models.rs`：公开图片模型增补与权限代理检查。
- `src/config.rs`：模型清单、直连/桥接策略和握手配置解析。

本项目是独立开发示例，不是官方 `codex-proxy-rs` / `Sub2API` 的组件或授权声明。遵守各上游服务条款。
