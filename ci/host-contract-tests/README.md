# 官方宿主生图合同回归

这些资产只向固定提交 `96e8443ede051abdd8f0bac69462a7cfe2945633` 的官方 CPR 源码添加测试模块及声明，不改变宿主生产实现。Provider 用例复用官方 `provider-openai` 的路由、协议解析、HTTP 传输与已有账号 fixture；Runtime 用例直接调用官方私有 `ModelStream::read`、事件投影与 SDK 编码器。

## GitHub Actions 调用

在 Linux CI 中先将 `zyycn/codex-proxy-rs` 检出到固定提交，再执行：

```bash
python3 ci/host-contract-tests/install.py --host-root "$CPR_HOST_ROOT"
cargo test --locked --manifest-path "$CPR_HOST_ROOT/backend/Cargo.toml" \
  -p provider-openai --test main \
  provider::contract::image_bridge_callback_contract:: -- --nocapture
cargo test --locked --manifest-path "$CPR_HOST_ROOT/backend/Cargo.toml" \
  -p gateway-plugin-runtime --lib \
  callback::model::image_bridge_capacity_contract:: -- --nocapture
```

`CPR_HOST_ROOT` 是该独立官方宿主 checkout 的根目录。插件仓库为当前工作目录。安装器检查精确 HEAD，先完成所有目标文件预检，拒绝覆盖不同内容的同名文件，可重复运行；加 `--check` 可只做预检。不要在 Windows 机器运行 Cargo 构建。

Runtime 测试通过 `#[cfg(test)]` 子模块访问原有实现。测试只注入一个实现官方 `ExecutionSession` 的事件源，不需要数据库、外部插件进程或真实凭据，也没有跳过测试的分支。

## 覆盖范围

1. 对两种 sunburst 图片模型及三种尺寸，通过实际 `RuntimeSnapshot::plan` 创建模型目标，验证固定宿主原生 Images Provider 返回 `InvalidRequest` 和 `NotSent`，且 mock 上游收到零次请求。
2. 用实际 `plan_provider_endpoint` 对照，验证同一原生 Images JSON 能发送给 mock 上游，模型、尺寸、WebP、压缩率等原始字节保持不变。
3. 通过实际 Responses Provider 解析和 HTTP SSE 传输，验证两种图片工具模型 × 三种尺寸、输出格式、压缩率和强制图片 `tool_choice` 保留；验证图片工具结果可读取。
4. 小图片事件经过真实 `ModelStream::read` 和 SDK 解码，正常提交并结束。
5. 单个原始 SSE 帧小于 1 MiB，但 JSON 数据和原帧共同编码后超过默认运行时预算、仍小于 SDK 8 MiB 上限：读取返回明确 `Capacity`，且取消并清理执行会话。
6. 即使把测试运行时预算设为 16 MiB，超过 SDK 8 MiB 的事件仍由真实编码器拒绝；调用方关闭原流后完成清理。

## 证据边界

测试服务器只在本机运行，不使用真实账号凭据、不调用外部图像模型、不产生计费请求。mock 账号由官方 fixture 创建。Runtime 事件中的合成 Base64 只用于传输容量验证，不声明它是可解码的真实图片。

这些测试覆盖真实宿主的路由、Provider 合同，以及模型流读取和编码的明确容量边界。它们不能单凭执行耗时证明某次生产 `Capacity` 的确切来源；账号并发排队、预算等 Core 错误也会映射为相同错误码。真实故障仍需脱敏的具体错误类别佐证。

测试不替代插件 RPC 调度、管理桥浏览器交付、真实上游权限与尺寸支持、实际图片格式 / 像素以及预览下载的环境验收。三种尺寸出现在本地合同用例中，并不表示真实上游已接受这些尺寸。

新增测试会在固定 pin 上断言其现有路由与容量限制；未来若升级宿主，应重新审查这些回归预期。
