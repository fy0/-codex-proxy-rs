# 从 legacy 分支迁移到官方镜像

本过渡版本以官方 **v3.19.0**（`fef52b3717019513108733549508377a309f129d`）为基础。
迁移源固定为 `legacy` 分支备份 `1f099463c858bf97cf59ed6c60e9b1ef26ae47d4` 中的迁移文件。
目标镜像是 `ghcr.io/zyycn/codex-proxy-rs:3.19.0`，不要在转换时使用浮动的 `latest`。

仅支持共同 `0001–0015` 基线后，连续应用了本目录 `legacy/` 中 `0016–0030` 任意前缀的数据库。
文件必须与 `manifest.json` 的 SHA-256/SHA-384 相符，数据库中的描述、成功状态和 SHA-384 也必须匹配。
缺号、失败记录、未知迁移以及手动更改过的 checksum 都会被拒绝。不要改清单或数据库 checksum 来强行通过。
其他 fork、实验发行线、非 public schema、只读副本和更高版本数据库不在支持范围内。

## 数据处理与保护

账号及凭据、Client Key、额度与费用、分组、代理、请求记录、审计和共同运行设置保留。
以下 fork 专有能力不转换成官方功能：turn-state 配置、候选与事件、通知队列、routing-cookie 池、
BPS 开关与账号内并发上限、BPS 模型映射。它们会从活动数据库移除，但保存在完整数据库备份中。
官方 `0016` 也会移除 `disable_fast`，需要在官方客户端请求配置中重新核对相关设置。

程序只处理 PostgreSQL，不清空 Redis，不更换账号凭据，也不删除 `.runtime` 或部署配置。
Redis 与 `.runtime/data` 中的其他文件应按[常规备份说明](../README.md#备份与恢复)单独保留。

转换过程为：识别已知历史、检查其他客户端、获取迁移锁和业务表 SHARE 锁、生成完整 custom-format
`pg_dump` 备份并检查归档目录、写入备份校验回执，然后在**同一事务**中移除 fork 专有结构、
撤回已归档的 fork 迁移记录、真正执行官方 `0016–0021` 原始 SQL，并登记其实际 SHA-384。
最后检查迁移历史和共同业务表行数，成功才提交。不使用 `CASCADE` 强行删除未知依赖。

备份失败不会开始结构转换；补偿或官方 SQL 失败会回滚整个数据库事务，已经生成的备份继续保留。
重跑已转换数据库不会再删除数据或重复生成备份。提交结果不确定、连接中断或进程被终止时，先用
`--check` 重新识别状态，不要手动操作 `_sqlx_migrations`。

迁移必须停机运行。检测到其他数据库客户端会拒绝转换，包括遗留网关副本、管理工具和手动 SQL 会话；
迁移时不要重新启动旧网关，也不要允许其他维护程序更改数据库。

## 构建过渡镜像

在本仓库根目录、具备 Docker 的构建机执行。此操作只构建镜像，不连接部署数据库：

```bash
docker build -f deploy/Dockerfile --target runtime \
  -t codex-proxy-rs:upstream-migration .
```

`runtime` 和 `runtime-prebuilt` 都包含迁移入口。构建必须先通过隔离 PostgreSQL 18 测试，包括
全部 15 个 legacy 前缀、与全新官方库的 schema 对比、业务数据保留、幂等、失败回滚以及完整备份恢复。
测试不通过，构建不会产生可交付的最终镜像。仅验证迁移程序可使用：

```bash
docker build -f deploy/Dockerfile --target migration-tests .
```

仓库的普通 CI 构建不发布镜像。部署机与构建机不同时，先通过自己的镜像仓库或 `docker save/load`
传送构建结果；不要把官方 release 工作流误用于发布本 fork 的过渡镜像。

## 停机、检查和转换

保留现有 `deploy/config.yaml`、Compose 自定义设置以及全部 `.runtime` 数据。对比官方部署模板并合并
必要配置，不要用模板覆盖现有配置或重新运行初始化来代替迁移。下列命令在部署根目录执行；
数据库与 Redis 服务应保持运行，只停止网关及其他数据库客户端。

```bash
export CPR_IMAGE=codex-proxy-rs:upstream-migration
docker compose -f deploy/compose.yaml stop codex-proxy-rs

# 只识别历史，不执行转换；已知旧库应显示 legacy。
docker compose -f deploy/compose.yaml run --rm --no-deps \
  codex-proxy-rs --check

# 明确授权的一次性转换；成功后退出，不启动 HTTP 服务。
docker compose -f deploy/compose.yaml run --rm --no-deps \
  -e CPR_MIGRATE_LEGACY_TO_UPSTREAM=1 \
  codex-proxy-rs --migrate-only

# 再次检查应显示 official。
docker compose -f deploy/compose.yaml run --rm --no-deps \
  codex-proxy-rs --check

# 可先用过渡镜像运行官方代码，验证管理端、账号、Key 和一次真实 API 请求。
docker compose -f deploy/compose.yaml up -d --no-deps --no-build codex-proxy-rs
```

默认不开启转换。也支持在过渡容器的 `environment` 中显式设置
`CPR_MIGRATE_LEGACY_TO_UPSTREAM: '1'`，使入口在启动官方服务前转换；建议优先使用上述一次性方式，
避免较长的备份过程被外部健康检查或自动重启策略打断。`export` 一个变量不会自动把它传入已有
Compose 服务，必须使用 `run -e` 或服务的 `environment`。

程序读取 `/app/deploy/config.yaml` 的 `store.database`，遵循 `CPR_DATABASE_URL` 和
`CPR_DATABASE_PASSWORD` 的非空环境覆盖；Compose 已提供容器内 URL。密码只经子进程环境传给
`pg_dump`，不放入命令行或日志。特殊挂载位置可通过 `CPR_LEGACY_CONFIG_PATH` 指定。

默认备份目录是配置中 `host.runtime_data_dir` 下的 `upstream-migration/`，标准部署对应宿主机
`.runtime/data/upstream-migration/`。可用 `CPR_LEGACY_BACKUP_DIR` 指定容器内**绝对、持久化、可写**路径，
不要设为 `/tmp` 或容器的临时可写层。备份以流式方式写盘，每份归档和 JSON 回执权限为 `0600`，
不会覆盖旧备份；需要预留足够磁盘空间。文件含全部明文账号凭据，禁止提交 Git、公开上传或粘贴内容。

切换前，把 `.dump` 与同名 `.json` 复制到受控的另一存储位置，核对回执中的 SHA-256。
归档目录可读性检查不替代在空库中的实际恢复演练。

## 切换官方镜像

确认上面的转换与业务验证成功，保留备份后执行：

```bash
export CPR_IMAGE=ghcr.io/zyycn/codex-proxy-rs:3.19.0
docker compose -f deploy/compose.yaml pull codex-proxy-rs
docker compose -f deploy/compose.yaml up -d --no-deps --no-build codex-proxy-rs
```

如在 `.env`、Compose 覆盖文件或部署平台中固定了镜像，也要更新相应设置，避免下次启动恢复旧镜像。
移除迁移专用环境变量。先验证固定的 `3.19.0`，后续再按官方说明正常升级，不要同时升级数据库大版本。

## 恢复 legacy

不能只把镜像换回 legacy；旧版程序与转换后的数据库不兼容。需要回退时先停止网关，在独立空库中
用 PostgreSQL 18 `pg_restore --exit-on-error --no-owner --no-privileges` 恢复保存的 `.dump`，
检查其 `_sqlx_migrations` 回到原来的 legacy 前缀，并验证业务记录。随后将 legacy 部署指向恢复后的库，
使用保留的配置、密钥文件和原来的 Redis 状态启动。

不要在仍有写入的库上覆盖恢复，也不要把恢复工具接到生产库做测试。备份只包含转换前的数据，
转换后新增的请求、配置或额度变化不会自动反向合并回旧库。恢复所需的数据库角色和访问权限仍由
部署配置管理；备份使用 `--no-owner --no-privileges`，不负责恢复服务器角色。

## 本地检查

```bash
python -B -m unittest discover -s deploy/upstream-migration -p 'test_*.py' -v
```

不配置专用 `CPR_TEST_DATABASE_URL` 时仅运行纯 Python 测试，数据库测试明确跳过，不能算通过。
完整测试优先使用上面的隔离容器构建；需要自行连接测试服务时，该账号必须能够创建和删除临时数据库，
只使用专用测试实例，绝不能把 `CPR_DATABASE_URL` 作为测试 URL。
