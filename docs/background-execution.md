# 后台生图：已确认的宿主依赖

核对基线：官方 CPR / SDK `96e8443ede051abdd8f0bac69462a7cfe2945633`。

**本轮没有实现 Core JobRunner，也没有完成真实部署的后台生图验收。** 当前页面即时请求依赖页面继续读取响应；关闭页面后继续执行、重新打开后恢复结果，需要宿主承担任务生命周期。恢复旧 worker 或只增加开关不能解决这些问题。

## 现有合同的边界

| 能力 | 固定版本的实际行为 | 对本插件的影响 |
| --- | --- | --- |
| 管理桥 `models.responses` | 宿主用管理员会话、插件页面目标和显式 `clientKeyId` 验权；仅注册 Responses 路由。[1] | 不能把 Images 请求 JSON 原样发给它就认为访问了 `/v1/images/generations`。 |
| `host.model.execute_stream` | 按完整模型事件编码；批次上限 8 MiB，另受 RPC 载荷上限约束。[2] | 减少一次读取字节数不会把一个大图片事件自动分片。大图失败不能靠前端增加等待时间解决。 |
| `host.http` / `account_http` | 可分块读取 HTTP 正文；`account_http` 只选择账号代理和凭据版本，认证头仍由调用方提供。[3] | 不应读取 Client Key 明文或 OAuth 凭据来绕过模型执行合同。 |
| `upstream_http` | 需要当前调用已获得宿主的选定账号授权；缺少 `scope.upstream` 会被拒绝。[4] | 管理/维护调用不能任意升级为已授权的上游调用。 |
| `dispatch_http` | 进入完整 HTTP 请求链，继承父调用取消；请求合同只有 URI、请求头、正文和运行设置，没有 `clientKeyId` 身份入口。[5] | 不能直接替代管理桥的显式 Key 执行。 |
| 插件内 `tokio::spawn` | 父 RPC 结束时宿主移除 call ID、取消资源并关闭 HTTP/model streams；后续回调会被拒绝。[6] | 线程仍存活不代表原来的 `HostClient` 仍有合法调用寿命。 |
| `plugin.reconcile` | 宿主每轮等待通知或 30 秒定时器；内部快照更新可通知，没有已确认的插件公开入队唤醒接口。[7] | 旧队列即使为空也可能先等待维护周期；不能把修改配置当作唤醒机制。 |

## 最小工程顺序

### 1. 先补原生 Images 管理桥

在 `gateway-api/src/admin/plugins/management.rs` 增加明确的 Images generation 端点和桥能力声明，复用现有 `AdminAuth`、页面目标校验、`clientKeyId → prepare_plugin_execution`。从 `openai/endpoint.rs` 抽出接收已准备执行身份的公共入口，让 `openai/images/http.rs` 与新管理端点共用正常的 Core 请求链、账号选择、租约和结算。[8]

插件不获取密钥或拼接上游认证，不自行把模型绑定到单个账号。原生 Images 请求和 Responses `image_generation` 请求使用各自合同，不静默互换。

**接通原生端点仍不能保证上游支持 2K/4K。** 必须以真实返回文件的像素、方向、格式验证；尺寸不符时保留图片和诊断，但不标为所选尺寸成功。

### 2. 再增加宿主拥有的 Background Jobs

以下是待实现的能力要求，不是现有 API：

- **提交与身份：** 管理桥接收执行模式、`clientKeyId`、请求体和幂等标识；同步验证管理员、插件目标、Key 和输入。持久化后才返回任务 ID。执行前重新核实仍有效的身份与策略，继续经过 Core 的调度、租约和用量结算。
- **寿命与启动：** 任务由宿主服务持有独立取消令牌和截止时间；页面断开不取消任务。入队直接通知 worker，不等待 `reconcile` 的 30 秒周期。
- **正文与结果：** worker 消费现有 HTTP 响应的原始字节流，按块写入有大小上限的结果文件；不经完整 `ModelEventBatch`。保存必要的状态、响应类型、请求 ID 和结果引用，不落盘管理员 Cookie、Authorization 或账号凭据。
- **状态与恢复：** 保留排队、准备、执行、完成、失败、取消/中断的明确状态和时间。结果及任务元数据可在重开页面后读取。宿主重启后，已提交上游而未确认完成的任务标为中断，不自动重发；只有确认尚未发送的任务才可按既定策略恢复。
- **取消与访问：** 提供查询、取消、读取结果操作，均校验管理权限和所属插件实例。取消释放流及 Core 租约；插件被禁用、移除或授权失效时遵循明确终止规则。插件升级保留历史，不冒充已完成仍未知的请求。
- **前端模式：** 即时与后台模式并存，但仅在宿主明确声明并实现上述能力后启用后台模式。旧宿主显示明确的不可用原因，不能把页面 Promise 或旧维护队列标成可关闭页面的后台任务。

这涉及 API/桥、宿主 worker、持久存储及结果流，不能仅修改插件构建产物交付。实施时应作为独立 Core 改动编译和审查，不在插件里放空实现。

## 必须通过的集成验收

1. 空队列立即进入准备；排队耗时与生成耗时分开记录。
2. 提交后关闭页面，宿主继续执行；重开页面可查询、预览和下载原结果。
3. 大于 RPC 单载荷、8 MiB 的图片响应可分块交付；文件内容、MIME 和像素一致。
4. 取消、Key 撤销、插件禁用、宿主重启、磁盘写入失败均有明确终态，不产生重复上游提交或遗留租约。
5. 原生 Images 与 Responses 分别完成真实 1024 验证，再验 2K/4K；测试不同模型的尺寸和方向，不以 HTTP 200 代替像素验收。
6. 切回即时模式后使用现有正常 Core 路径；历史任务、图片及插件配置继续保留。

## 固定源码依据

[1]: https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-api/src/admin/plugins/management.rs#L73-L180
[2]: https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-plugin/sdk/src/call/host.rs#L280-L345
[3]: https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-plugin/sdk/src/client/http.rs#L22-L70
[4]: https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-plugin/runtime/src/callback/upstream/mod.rs#L134-L195
[5]: https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-plugin/runtime/src/callback/http_dispatch.rs#L16-L69
[6]: https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-plugin/runtime/src/callback/dispatch.rs#L44-L92
[7]: https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-plugin/runtime/src/generation/maintenance.rs#L193-L235
[8]: https://github.com/zyycn/codex-proxy-rs/blob/96e8443ede051abdd8f0bac69462a7cfe2945633/backend/crates/gateway-api/src/openai/endpoint.rs#L32-L90
