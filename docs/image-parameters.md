# 图片参数：公开 API、Codex 通路与实测边界

核对日期：2026-11-07。CPR SDK / Provider 基准固定为 `96e8443ede051abdd8f0bac69462a7cfe2945633`。本页记录已经核对的代码合同和验收方法；不代表该宿主已通过原生 2K / 4K 生图验收。

## 1. 尺寸字符串本身是否有效

OpenAI 的公开 API 指南为 GPT Image 2 / 2.5 列出自定义 `WIDTHxHEIGHT`。2.5 的常见尺寸包括 `2048x2048`、`3840x2160` 和 `2160x3840`；边长不超过 3840、均为 16 的倍数、长短边之比不超过 3、总像素在 655,360 到 8,294,400 之间。超过 `2560x1440` 的输出仍标为实验性。[官方参数指南](https://developers.openai.com/api/docs/guides/image-prompting?model=gpt-image-2.5)；[官方 Images API 参考](https://developers.openai.com/api/reference/resources/images/methods/generate)

因此用户提交的 `2048x2048`、`2160x3840` 符合上述公开 API 尺寸约束。公开 API 文档不能证明 ChatGPT / Codex OAuth 后端会遵守同样的参数，也不能仅从模型名称推断某个账号可获得原生 4K。

## 2. 本插件和固定 CPRS 有没有丢失参数

已经核对的路径如下。

| 路径 | 当前行为 | 证据 |
| --- | --- | --- |
| 管理页 `models.responses` | 提交 Responses JSON；正文里的图片参数位于 `tools[]` | `web/app.js`、`web/debug.js`、`web/image-client.js` |
| 配套 Core 的 `models.images.generate` | 独立 Images 正文，直接进入已授权的原生 provider-endpoint；不会退回 Responses | `core-patches/native-images/`、`web/debug.js`、`web/image-client.js` |
| 本插件 HTTP 中间件处理 `/v1/responses` | 不进入 Images 转换分支，直接交给下一层 | `src/main.rs` 的 `handle_http` / `image_kind` |
| 本插件模型中间件和账号策略 | 读取图片模型用于权限与账号选择，去掉内部意图头；不改写正文中的 `model`、`size`、`quality` | `src/main.rs` 的 `handle_request`；`src/policy.rs` 的 `image_intent` / `schedule` |
| Images → Responses 显式桥接 | 将 Images 的 `model`、`size`、`quality`、`output_format` 等复制到图片工具定义 | `src/bridge.rs` 的 `parse_json` |
| 原生 JSON Images 透传 | 保留原始请求字节；multipart 编辑另做结构转换并复制参数 | `src/main.rs`；`src/bridge.rs` 的 `direct_images_payload` |
| 固定 CPRS Responses Provider | 克隆协议正文、映射外层对话模型；没有删除或重写图片工具的上述字段 | [官方 request.rs](https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/providers/openai/src/transport/request.rs)、[兼容层 request.rs](https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/providers/openai/src/compat/request.rs) |

`ci/host-contract-tests/image_bridge_callback_contract.rs` 会在真实固定版本 Provider 和路由代码上，通过本地 HTTP fixture 验证 Responses 工具字段完整透传，以及原生 Images provider-endpoint 的字节保留。这个测试证明的是指定代码和测试条件下的传输合同；模拟图片不证明上游支持某个模型、尺寸或像素数量。生产中其他中间件和上游服务仍需通过真实响应排查。

## 3. 官方 Codex 实现与公开 API 的区别

核对官方 Codex 提交 `f9e7b19df8aa75490fc1d90f98d3d95ad716d08e`：

- 内置图片工具创建请求时使用 `gpt-image-2`，显式传 `quality: auto` 和 `size: auto`。这说明官方客户端当前选择了自动参数，不能据此断言服务端只支持自动参数。[tool.rs](https://github.com/openai/codex/blob/f9e7b19df8aa75490fc1d90f98d3d95ad716d08e/codex-rs/ext/image-generation/src/tool.rs)
- 独立 `ImagesClient` 请求 `images/generations` 或 `images/edits`；其请求类型具有可选字符串 `size`，并直接序列化这些字段。它与 Responses 中的托管 `image_generation` 工具是不同入口。[ImagesClient](https://github.com/openai/codex/blob/f9e7b19df8aa75490fc1d90f98d3d95ad716d08e/codex-rs/codex-api/src/endpoint/images.rs)；[请求类型](https://github.com/openai/codex/blob/f9e7b19df8aa75490fc1d90f98d3d95ad716d08e/codex-rs/codex-api/src/images.rs)
- 固定 CPR 管理桥公开的模型接口只有 `models.responses`，没有独立 Images 调用。给 JSON 增加 `endpoint` 等字段不会把这个方法变成 Images 入口。[bridge-contract.ts](https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/frontend/src/views/plugins/management/bridge-contract.ts)

固定版本的后台 `host.model` 是按公开模型执行的路径。已有宿主合同测试确认：原生 `GenerateImage` 通过这条模型路径会在发送前触发 `InvalidRequest / NotSent`，而原生 HTTP Images 使用的 provider-endpoint 路径可保留参数并发送。要在管理桥中增加独立 Images 调试入口，需要宿主提供相应合同；不能靠改变 Responses 正文绕过这一区别。

v0.7.8 已提供针对官方 Core `ca5901ba568ab398948dcf32239854337c6928dc` 的[前后端配套补丁](../core-patches/native-images/README.md)。调试页可切换真实的 `models.responses` / `models.images.generate`；后者复用原生 HTTP provider-endpoint 的授权与交付链。旧 Core 没有此方法时明确不可调用，不伪造路径或自动降级。当前固定原生适配器只支持非流式 JSON，默认模板 `stream: false`，不要把 Responses 的 `tools` 整体搬到原生正文。

## 4. 请求值、服务端回报与实际图片要分别核对

调试面板的提交快照和导出包含：

1. **请求正文**：实际提交给管理桥的完整 JSON，保留手写参数。切换 Key 不会改写手写正文；“载入模板”才会主动使用该 Key 目录里的模型。
2. **服务端参数回报**：Responses 返回的外层模型，以及 `response.tools`、`image_generation_call` 中实际出现的模型、尺寸、质量、格式等字段。每条记录保留事件类型和来源；没有回报的字段不由请求值补齐。
3. **实际图片**：从返回文件识别格式并解码得到宽高。预览和下载使用返回内容；不会通过插值放大冒充原生尺寸。

例如，请求 `2160x3840`，服务端工具回报 `size: auto`，解码图片为 `1536x1024`，这是三项独立事实。该图片仍可保留供检查，但不能把它标记为所选 4K 尺寸生成成功。反过来，即使服务端回报所选尺寸，也必须以解码后的像素验证。

这些观测来自管理桥返回的内容，不是上游网络抓包。如果上游工具回报缺失，面板明确显示“未返回”，不推断模型、质量或尺寸已被遵守。

## 5. 最小真实验收步骤

1. 在调试页选择一个 Client Key，先载入纯文本模板，确认使用该 Key 目录中存在的对话模型。若返回 `no_available_provider`，先核对模型授权、账号状态与额度规则；只凭这个 503 无法认定错误来自图片尺寸。
2. 载入图片模板，固定提示词、对话模型、图片模型、质量和输出格式，先运行一次 `1024x1024`。保留请求快照、HTTP 状态、请求 ID、服务端参数回报和实际图片。
3. 在相同条件下逐次将 `size` 改为 `2048x2048`、`2160x3840`。每次显式运行一次，核对原始事件与解码尺寸。不要把浏览器 fixture、返回 HTTP 200 或工具状态 `completed` 当作尺寸验收。
4. 部署配套 Core 后，在调试页切换「Images · 独立生图接口」，以顶层 `model`、`prompt`、`size`、`quality`、`output_format` 比较相同条件。检查实际桥是 `models.images.generate` 且 `bridgeInvoked: true`。旧 Core 无此能力时先补齐宿主，不能靠增加 JSON 字段模拟入口。切换入口本身不保证原生 4K。
5. 导出时检查 `completeCapture`，并记录插件版本、宿主版本、请求尺寸、返回格式和像素。完整导出保留被预览截断的长字段和图片内容；凭据不应出现在分享的诊断资料中。

在真实图片与尺寸检查通过之前，本版本只确认修复了可证明的客户端问题并补充参数观测，不能写成“2K / 4K 已修复”。
