# CPR Image Bridge v0.7.6 (official SDK)

面向官方 [zyycn/codex-proxy-rs](https://github.com/zyycn/codex-proxy-rs) 的独立 Rust 插件。SDK、宿主合同测试和打包工具固定提交 `96e8443ede051abdd8f0bac69462a7cfe2945633`；协议要求 RPC v7、middleware v6、scheduler v3、management v2，Rust 1.99.1。

## 本版修复：恢复即时生图，增加原始请求调试

用户在 v0.7.5 真实环境报告了空队列等待 30 秒以上，随后 `model_stream_read / Capacity`，而 v0.5.1 同时仍可生图。本版恢复 v0.5.1 已使用的浏览器管理桥调用方式，取消维护回调中的图片执行。

- 点击生成后，当前页面直接调用官方 `window.codexProxyPlugin.models.responses({ clientKeyId, body, signal })`。没有 `POST api/jobs`、维护轮询、指定账号 ID 或失败后重发。
- 普通画图默认请求恢复 v0.5.1 的完整模板：Responses 流、PNG、`tool_choice: "auto"`、`action: "auto"`、`parallel_tool_calls: false`。图片模型在 `tools[0].model`；顶层 `model` 是所选对话主控模型。不强制 WebP、压缩率、图片工具或新的 reasoning 参数。
- 管理桥仍进入 CPR 正常请求管线。Client Key 权限、图片权限、账号白名单、Core 调度和并发租约继续由宿主及已启用的插件绑定执行。页面不预选或绑定账号。
- 额度维护独立运行，不再与长时间生图共享一次维护回调。初次页面加载不读取全量账号目录、旧图片或历史任务；它们在展开相应区域时加载。
- 增加独立管理页面「原始请求调试」，与普通画图共用 `web/image-client.js`，便于比较同一管理桥上的参数与响应。
- 删除后台生图执行器、重复流解析、账号预绑定、下载上游图片及直接 `reqwest` 依赖；`src/jobs.rs` 仅保留旧任务归档兼容。

### 等待和 Capacity 的证据边界

旧生图队列由宿主维护轮询领取，约 30 秒的唤醒间隔会直接增加空队列首个任务的延迟。新路径不等待这次轮询，但 Core 的账号容量、上游处理时间和网络等待仍可能产生延迟。界面展示提交至响应头、首字节、首张图片和总耗时；这些是浏览器观察时间，不能单独证明上游何时开始生成。

固定官方宿主的模型回调将完整 JSON 事件与原始 SSE 一起投影。默认模型流 payload 预算 1 MiB，与 SDK 8 MiB 编码上限是不同限制；单个大图事件可能触发 `Capacity`。Core 的账号容量或排队错误也可映射为 `Capacity`，因此用户日志里的 `Capacity + NotSent` **不能单独确定是哪一个分支，更不能证明整个请求没有发送到上游**。管理桥直接读取 HTTP Response/SSE，不经过该模型回调的整事件编码预算。

官方源代码依据：

- [管理桥进入普通 Responses 请求](https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-api/src/admin/plugins/management.rs)
- [模型流读取和 Capacity 分支](https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-plugin/runtime/src/callback/model.rs)
- [事件事实投影](https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-plugin/runtime/src/callback/facts.rs)
- [v0.5.1 页面调用基线](https://github.com/fy0/-codex-proxy-rs/blob/e0d473d2aca0917e43c4da67d057e0fb75639dc0/web/app.js)

## 使用画图测试页

进入「插件管理 → Codex Image Bridge → 画图测试」。选择启用的 Client Key、对话主控模型、图片模型、尺寸、画质与提示词，再点击「立即生成」。只使用 Key ID，不读取密钥明文。

每次可提交 1～10 张，每张是一个独立请求；页面同时执行上限为 1～4，默认 2。仅超过本页并发上限的任务等待空位，完成一个就补入下一个。最多保留 12 张任务卡片，不自动丢弃尚未保存的图片；保存后可清理已结束任务。

**页面需要保持打开。刷新、关闭或切换离开页面会取消当前请求；本版新图片保存在当前页面内存中，不再承诺后台持久执行。** 这与 v0.5.1 的连接生命周期一致。取消会传递给宿主，但不能保证已经发生的上游用量被撤销。

图片按实际内容识别 PNG / WebP / JPEG，并检查真实像素。选择具体尺寸时，结果尺寸不符会明确标错，不把缩小图片算作成功 2K / 4K。尺寸与质量选项是否受某个上游账号支持，仍需真实请求验证；先验收 1024×1024，再测试大图。

官方插件 iframe 使用 `img-src data:`，预览采用 data URL。固定宿主 sandbox 只有 `allow-scripts`，自动下载可能被浏览器阻止；可在预览上右键另存图片。调试页另提供完整图片 data URL 文本。页面不会把点击下载按钮表述为已成功写入本地文件。

上次 Key、主控模型、图片模型、尺寸、画质、张数与页面并发使用官方私有状态保存。旧状态中的较大并发值在页面中收敛到 4，不改状态 schema、不清空配置；不保存提示词或新生成图片。

### 原始请求调试页

在同一个插件的管理页切换器中选择「原始请求调试」。

1. 选择 Client Key，加载 v0.5.1 基线模板。
2. 直接编辑完整 Responses JSON，例如 `tools[0].model`、`size`、`quality`、`output_format`、`output_compression`、`tool_choice`、`instructions` 等。
3. 点击一次发送一次。编辑后的 JSON 原样交给管理桥，不静默补字段、不自动修正、不在失败后更换参数重发。
4. 查看提交快照、HTTP 状态、经过脱敏的响应头、请求 ID、事件时间线、错误 code / kind / stage、阶段耗时与图片像素。
5. 完整事件与诊断可导出为当前页文本并全选复制；长 Base64 不塞满可视事件列表。响应采集上限为 256 MiB，超过时明确标记截断，绝不把截断采集称为完整原始响应。

面板显示的是**提交给 CPR 管理桥的 JSON 和管理桥返回的响应**。Core / Provider 仍可能转换协议或上游参数，因此面板不将这些内容称为上游网络抓包。此页不允许填写或导出 Authorization、Cookie、API Key 或 OAuth Token；通用错误文本与响应诊断会遮盖凭据。编辑器中的业务提示词和图片在页面内可见，分享导出前按需要删去。

调试页使用标准 Responses 请求，不能以此宣称已验证独立 Images 原生 HTTP 端点。自定义字段或上游不支持的参数会返回真实错误，这正是调试用途。

### 旧后台任务和图片

「旧后台任务历史」默认折叠，展开后才读取 `image-jobs`。不会重新领取或执行旧 `queued/running` 任务，不会重复收费请求。旧页面继续调用 `POST api/jobs` 时返回明确 410，提示刷新，不让任务继续进入已停用的队列。

旧记录、未知诊断字段和结果文件保持。只有既有 `.img` / `.png` 完整解码、格式与尺寸符合记录时，才将旧 `running` 恢复为已完成；缺失文件或不完整结果保留原状态。每次列表有界检查，按轮转游标逐批恢复。取消旧任务只更新该记录，不删除记录或文件。

旧图片在点击「读取旧图片」时按 256 KiB 分片读取。使用原 `imageStorageDir`；未设置时使用系统临时目录的 `cpr-image-bridge/images`。临时目录已清理的结果不能恢复。

## 账号额度保护与每日限额

独立页面「账号额度保护」保留五小时 / 每周进度条、账号卡片、筛选、刷新、阈值、状态提示、撤销与保存。浅色 / 深色样式具有完整默认值，宿主未注入主题时也能显示表单、卡片和状态。

`quotaGuard[]` 可增加 `dailyLimitUsd: "20"`：

- 省略或 `null` 关闭日费用保护；`"0"` 立即阻止该账号的新请求。
- 覆盖该账号全部 CPR 请求，按请求开始时间归属 UTC 自然日，在 UTC 00:00 换日。
- 费用来自宿主持久化用量账本，精确十进制累计；输入最多 18 位整数、12 位小数，不使用浮点金额累计。
- 达到阈值后排除账号。冷缓存、账本不完整、币种异常、已完成费用未知或观测过期时显示未知 / 过期并阻止受保护账号，不把未知视为 0。
- 重启后从宿主账本重建。单账号读取最多 100 页、每页 200 条、5 秒；维护批次最多 20 个账号、4 并发、8 秒预算。
- 这是已记录费用阈值，不能原子预留在途费用；并发和记账延迟可能让最终费用超过设定值。OAuth 套餐百分比与 CPR 的 USD 记账是不同口径。

保存额度规则通过实例 revision 合并，仅改变 Guard 字段，保留原有图片模型、白名单、主控模型、存储目录和兼容并发配置。画图页的配置生成器也保留未编辑的旧配置项。

额度保护与图片账号筛选合并在同一个 scheduler。原独立 `cpr-quota-guard` 若仍有启用的 scheduler 绑定，不会被本插件静默停用；需要在实际部署中核对并移除重叠绑定。建议插件故障策略为 reject。升级本插件不会自动修改其它插件实例。

原版 Quota Guard 代码来源为 [Mr-xiongyq/cpr-quota-guard](https://github.com/Mr-xiongyq/cpr-quota-guard)，Apache-2.0，详见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

## 配置和标准 API

| 参数 | 默认值 | 用途 |
| --- | --- | --- |
| `driverModel` | `gpt-5.6-luna` | 外部 Responses 桥接的主控模型；页面可另选当前 Key 可用主控 |
| `bridgeMode` | `auto` | 外部 Images HTTP 的策略；`auto` 使用原生 Images，`responses` 使用图片工具桥接 |
| `timeoutMs` | `300000` | 桥接和画图页单次超时，最终仍受宿主 / 反代限制 |
| `imageAccountIds` | `[]` | 生图账号 ID 白名单 |
| `imageGroupIds` | `[]` | 生图分组 ID 白名单，与账号白名单取并集 |
| `imageModels` | 内置图片模型列表 | 对外发布的 `gpt-image-*` 模型，显式空数组不发布 |
| `imageStorageDir` | `""` | 旧图片归档目录；新页面不使用它保存新图片 |
| `backgroundConcurrency` | `2` | 保留旧配置，已停用 |
| `concurrencyPerAccount` | `8` | 保留旧配置，已停用；实际账号并发在 CPR Core 设置 |
| `quotaGuard` | `[]` | 五小时 / 每周 / 每日费用规则 |
| `maxObservationAgeSeconds` | `300` | 额度观测有效期 |

未配置图片账号或分组时，不额外开放插件生图。动态目录根据官方 Key、账号启用、分组及模型权限复核；`check_access` 是政策检查，不能证明上游账号实际拥有某个图片模型。

插件提供 `/v1/images/generations` 和 `/v1/images/edits`，复用宿主原生 Images Provider，兼容模式则转 Responses。授权、originator、User-Agent、版本等由 CPR Provider 管理，不复制账号凭据。原生成功响应保留 `generation_id`；不注册伪造的普通 Responses 模型来冒充 Images endpoint。

启用 `middleware.http`、`middleware.request`、`scheduler`、`management` 与 `maintenance`。插件侧栏通常每个实例一个入口，三张页面在入口内切换，并非三个侧栏项目。

## 升级、构建与验收

保留现有插件实例，使用对应服务器架构的新包切换版本。不要删除实例或清空状态。本版 `image-workbench` schema v2 和 `image-jobs` schema v1 与 v0.7.5 完全相同，保留 v1 → v2 的选择状态迁移。

GitHub Actions 负责 Linux AMD64、Linux ARM64、macOS ARM64 编译与官方 CLI 打包。Windows 源码机不运行 Cargo 构建。每个包包含画图、调试、额度三个页面，共十项 HTML / JS / CSS 资源。

验证分层：

- 插件 Rust 回归：HTTP/配置/策略/状态迁移/额度保护，以及旧任务状态 RPC 和图片分片。
- 固定官方宿主回归：真实 Provider 路由、Responses HTTP SSE、模型事件双份投影和容量边界，详见 [ci/host-contract-tests](ci/host-contract-tests/README.md)。
- 浏览器回归：实际页面、共享客户端、`allow-scripts` 沙箱和 CSP；覆盖立即提交、原始请求不改写、超过 1 MiB 的图片事件、取消、失败状态、并发补位与配置保留。宿主桥和上游使用测试数据。
- 制品核验：源提交、资源逐字节一致、manifest/state、三种可执行架构、官方打包元数据与 SHA-256。

**这些验证不能替代真实 CPR 生图。** 最终须在用户已部署的 CPR 中分别完成 2.5 / 2.6 sunburst 的 1024 生图、像素 / 格式 / 保存、取消、额度读取保存与账号调度验证，然后再验收 2K / 4K。当前无宿主连接凭据时，不宣称生产闭环已完成。

## 版本背景

v0.5.1 使用即时 Responses 管理桥；v0.6.0 增加 2.6 模型、偏好和页面多图，仍使用该桥。后续后台任务架构引入维护领取与模型流回调。v0.7.3 修复状态 schema 切换，v0.7.5 修复原生 Images 回调路由但真实环境仍出现等待与 Capacity。v0.7.6 恢复即时路径，后台任务退为历史兼容。旧版实现细节可在 Git 历史查看，避免把已停用行为混入当前操作说明。
