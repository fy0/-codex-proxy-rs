# 官方宿主生图合同回归

这些资产只向固定提交 `96e8443ede051abdd8f0bac69462a7cfe2945633` 的官方 CPR 源码测试目录添加一个测试模块和模块声明。它们复用官方 `provider-openai` 的 `RuntimeSnapshot` 路由、Provider 实现、协议解析、HTTP 传输和已有 wiremock / 账号 fixture。

## GitHub Actions 调用

在 Linux CI 中先将 `zyycn/codex-proxy-rs` 检出到固定提交，再执行：

```bash
python3 ci/host-contract-tests/install.py --host-root "$CPR_HOST_ROOT"
cargo test --locked --manifest-path "$CPR_HOST_ROOT/backend/Cargo.toml" \
  -p provider-openai --test main \
  provider::contract::image_bridge_callback_contract:: -- --nocapture
```

`CPR_HOST_ROOT` 是该独立官方宿主 checkout 的根目录。插件仓库为当前工作目录。安装器检查精确 HEAD，拒绝覆盖不同内容的同名文件，可重复运行；加 `--check` 可只做预检。不要在 Windows 机器运行 Cargo 构建。

## 覆盖范围

1. 对两种 sunburst 图片模型及三种尺寸，通过实际 `RuntimeSnapshot::plan` 创建模型目标，验证固定宿主原生 Images Provider 返回 `InvalidRequest` 和 `NotSent`，且 mock 上游收到零次请求。
2. 用实际 `plan_provider_endpoint` 对照，验证同一原生 Images JSON 能发送给 mock 上游，模型、尺寸、WebP、压缩率等原始字节保持不变。
3. 通过实际 Responses Provider 解析和 HTTP SSE 传输，验证两种图片工具模型 × 三种尺寸、输出格式、压缩率和强制图片 `tool_choice` 保留；验证 `response.output_item.done` 和 `response.completed` 中的图片工具结果均可读取。

## 证据边界

测试服务器只在本机运行，不使用真实账号凭据、不调用外部图像模型、不产生计费请求。mock 账号由官方 fixture 创建。

这些测试覆盖真实宿主的路由和 Provider 合同；它们不替代插件 RPC 生命周期、真实 OpenAI 模型权限、上游尺寸支持、实际图片格式 / 像素、1 MiB 事件预算以及预览下载的真实环境验收。三种尺寸出现在本地路由与传输用例中，并不表示真实上游已接受这些尺寸。

新增测试会在固定 pin 上断言其现有原生模型目标限制；若未来宿主支持该路径，应先升级 pin 并重新审查对应回归预期。
