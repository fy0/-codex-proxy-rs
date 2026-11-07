# Native Images 管理桥补丁

## 支持范围

此补丁为 CPR 管理页 v2 消息桥增加独立的原生 Images generation 调用：

```javascript
const response = await window.codexProxyPlugin.models.images.generate({
  clientKeyId: '选中的 Client Key ID',
  body: {
    model: 'gpt-image-2.5-sunburst',
    prompt: 'a tree',
    size: '1024x1024',
    quality: 'auto',
    output_format: 'webp',
    n: 1,
    stream: false,
  },
  signal: abortController.signal,
})
```

返回浏览器原生 `Response`，保留 HTTP 状态、响应头和正文。公开桥版本仍为 `2`；插件用 `typeof bridge.models?.images?.generate === 'function'` 检测新增能力。原来的 `models.responses` 保持原有行为。

- 基线仓库：https://github.com/zyycn/codex-proxy-rs
- 唯一支持基线：`ca5901ba568ab398948dcf32239854337c6928dc`
- 官方基线发布版本：`3.42.0`
- 配套派生测试版本：`3.42.0+imagebridge.0.7.8`
- Image Bridge 插件继续使用官方 SDK pin `96e8443ede051abdd8f0bac69462a7cfe2945633`；本补丁不修改 SDK。

派生标识使用 SemVer build metadata，保留现有插件 `engines` 对稳定版 `3.42.0` 的兼容判断；不使用会被普通稳定版本范围排除的 prerelease 后缀。

旧宿主不会因为只升级插件而获得新增方法。宿主后端和前端资源须配套更新。此目录与 CI 产生的派生测试包不是官方完整发行版；生产宿主实际版本尚未确认，不能据此向较新宿主回退，也不能直接覆盖生产配置、数据库或插件目录。

## 执行合同

管理 HTTP 路径：

```text
POST /api/admin/plugins/extensions/{instance_id}/{artifact_sha256}/{revision}/models/images/generations?clientKeyId=...
```

目标数据面合同为 `/v1/images/generations`。它不是实际 Provider 的最终 URL；Provider 仍按账号认证类型选择自身地址和认证。

入口依次执行管理员认证、精确插件实例/制品摘要/revision 校验和显式 Client Key 准备，再进入现有原生 Images adapter。管理桥复用 `invoke_request`、`Operation::GenerateImage`、OpenAI Provider endpoint、Core 账号调度/租约、`PendingExecution` 及原有交付结算。页面目标撤销、停用或版本失效时，沿现有授权复核和取消机制终止等待及响应交付。

这条路径直接进入原生 Images 请求边界；不会经过 Image Bridge 外部 HTTP 的 Responses 转换分支。插件在 request/attempt/调度边界的白名单及 Quota Guard 策略仍可生效。它不读取或向页面暴露 Client Key 明文、OAuth 凭据或账号密钥，也不创建另外的记账流程。

`body` 从页面 JSON 序列化后，后端不重建、不裁剪字段、不改写模型/尺寸/质量/格式，不把 `stream: false` 改成 `true`。Core 原生 JSON adapter 当前不保证 SSE Images 支持，手工填写 `stream: true` 会原样进入该 adapter；真实错误原样返回，页面不会自动改走 Responses。`models.images.generate` 仅覆盖 generations，未新增 edits。

若管理请求没有显式 `x-codex-image-turn-id`，使用本次 Core 服务器请求 ID 填充原生 image turn context。已有请求头或请求中间件给出的 turn ID 优先；Provider 按原合同生成真实上游对应头。补丁不伪造 originator 或认证信息，也不改变 JSON 业务字段。

## 参数与图像验收

此路径保证进入 Core 原生端点时的请求保真，不能保证任意账号/模型/上游支持自定义分辨率或质量档位。需要同时记录：

1. 所选桥方法和提交 JSON；
2. HTTP 状态、关联请求 ID、服务端参数回报；
3. 图片真实 MIME、解码宽高和可下载内容。

当请求 `2160x3840` 而真实返回 `1536x1024` 时，插件仍必须显示尺寸不匹配。不能只凭选择值或服务端 `size` 标签判定 2K/4K 成功。Responses 的响应模型与 image tool 的模型分别展示，不能合并为同一个模型证据。

## 应用与构建

在独立 Core checkout 中应用，不在现有生产目录或用户未提交的源码工作树上操作：

```bash
git clone --filter=blob:none --no-checkout https://github.com/zyycn/codex-proxy-rs.git native-host
git -C native-host fetch --depth=1 origin ca5901ba568ab398948dcf32239854337c6928dc
git -C native-host checkout --detach ca5901ba568ab398948dcf32239854337c6928dc

python core-patches/native-images/apply.py native-host --check
python core-patches/native-images/apply.py native-host
```

`apply.py` 要求精确 Git HEAD、干净的已跟踪源码和 `manifest.json` 中每个文件的 SHA-256；先执行 `git apply --check`。不支持其他基线，不猜测兼容，也不自动解决冲突。重复应用已完整一致的补丁返回 `already_applied`。`native-images.patch` 是可直接审阅的标准 unified diff。

固定基线工具链为 Rust `1.99.1`、Node `26.13.0`、pnpm `12.6.0`。Windows 开发机磁盘限制下仅生成补丁、执行 Node 合同测试及 rustfmt；Cargo 编译和测试由 GitHub Actions 完成。

```bash
cargo fmt --manifest-path native-host/backend/Cargo.toml -p gateway-api -- --check

RUST_MIN_STACK=16777216 cargo test --manifest-path native-host/backend/Cargo.toml \
  -p gateway-api --test main admin::plugins::management --locked
RUST_MIN_STACK=16777216 cargo test --manifest-path native-host/backend/Cargo.toml \
  -p gateway-api --test main openai::images --locked

pnpm --dir native-host/frontend install --frozen-lockfile
pnpm --dir native-host/frontend typecheck
pnpm --dir native-host/frontend build
node core-patches/native-images/verify-bridge.mjs native-host

CPR_VERSION=3.42.0+imagebridge.0.7.8 \
CPR_GIT_SHA=ca5901ba568ab398948dcf32239854337c6928dc \
CPR_BUILD_TYPE=development \
cargo build --manifest-path native-host/backend/Cargo.toml \
  --release --locked --bin codex-proxy-rs
```

真实版本来源还应包含 Image Bridge 仓库提交及补丁摘要，避免将带补丁的二进制标成未修改的官方发行版。

后端二进制是 `backend/target/release/codex-proxy-rs`。前端资源不嵌入二进制：官方部署把 `frontend/dist` 放入运行目录 `web/dist`。完整官方发行还包含封口插件和部署资源；CI 的本次测试 bundle 仅作为明确基线的配套验证资源，不替代官方安装器。

## 验证覆盖和边界

新增 6 个 Rust 管理集成测试覆盖：

- 独立 Images Provider endpoint、JSON 原始字节和 2 MiB 返回、单次交付结算；
- 手工 `stream` 与未知字段保持不变；
- request middleware 拒绝时不会开始 Provider 调用；
- 管理员身份、页面目标版本及显式 Key 权限；
- 等待期间目标撤销和取消；
- 默认服务器 turn ID 与显式头优先。

`verify-bridge.mjs` 对实际前端桥源码运行 9 个合同场景：独立 URL、参数保留、64 KiB 背压分块、Responses 兼容、HTTP 错误不降级、未知 operation 拒绝及前后两个取消时机。它使用 Node 标准类型擦除，并模拟浏览器消息的异步交付，不要求真实账号或付费请求。

这些测试使用可控 Provider/HTTP fixture，不构成真实生产生图验收。需要在用户实际 CPR 版本与账号环境中继续确认两代图像模型的 1024 成功、预览/下载、2K/4K 参数支持、真实 Guard 调度及费用结算。未取得运行环境证据前，不宣称这些真实生图验收已完成。
