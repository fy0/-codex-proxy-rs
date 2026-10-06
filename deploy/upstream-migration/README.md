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

## 获取过渡镜像

本 fork 的 `main` 推送在 CI 中构建并验证 `linux/amd64`、`linux/arm64`，全部所需 CI 任务成功后，
`publish-edge` 才把同一批已验证镜像发布到 `ghcr.io/fy0/codex-proxy-rs:edge`，同时保留
`sha-<完整提交号>` 标签。镜像的 `org.opencontainers.image.revision` 标签记录来源提交。
必须确认 `publish-edge` 成功；仅有构建成功不代表仓库中的 `edge` 已更新。

部署机直接拉取镜像，不需要本机构建或运行测试。自行构建时，在本仓库根目录、具备 Docker 的
构建机执行以下命令；此操作只构建镜像，不连接部署数据库：

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

PR、定时安全扫描和非本 fork 的工作流不发布 `edge`。不要用官方 release 工作流发布本 fork
的过渡镜像；已有版本标签和官方镜像不由此流程更新。

## 更新镜像并自动转换

过渡镜像**默认在启动服务前自动迁移**。更新镜像并重建容器即可，不需要设置迁移环境变量，
也不需要手动运行 `--migrate-only`。识别到已知 legacy 库后，先备份、再转换，成功后启动官方代码；
已转换的库直接启动，不重复生成备份。未知历史、备份失败或转换失败都会阻止服务启动。

保留现有 `deploy/config.yaml`、Compose 自定义设置以及全部 `.runtime` 数据。对比官方部署模板并合并
必要配置，不要用模板覆盖现有配置或重新运行初始化来代替迁移。数据库与 Redis 服务保持运行。
单实例更新时先停止旧容器再启动新容器；多副本部署必须先停止全部旧网关和其他数据库客户端，
不能使用新旧副本并行的滚动更新。下列命令从部署根目录执行：

```bash
export CPR_IMAGE=ghcr.io/fy0/codex-proxy-rs:edge
docker compose -f deploy/compose.yaml pull codex-proxy-rs &&
docker compose -f deploy/compose.yaml up -d --no-deps --no-build codex-proxy-rs
docker compose -f deploy/compose.yaml logs -f --tail=100 codex-proxy-rs
```

首次转换的日志应先显示完整备份路径，再显示 `数据库状态：migrated；固定目标：v3.19.0`，
随后服务正常启动。已经转换的库会显示 `数据库状态：official`。确认管理端、账号、Client Key
和一次真实 API 请求正常，保留备份后即可切换下文的官方镜像；不要仅凭容器已经创建判断成功。

备份和转换期间尚未提供 HTTP 服务，健康检查可能暂时失败。部署平台的启动等待、自动回滚或
自动重启策略必须允许首次迁移完成，不要在看到迁移成功和服务启动前再次更换镜像。

`CPR_MIGRATE_LEGACY_TO_UPSTREAM=0` 只用于显式关闭自动转换；部署中如保留了这个值，更新镜像前
应移除。默认值为 `1`，其他值会报错。排障时仍可使用只读 `--check`；需要只转换而不启动服务时，
停止所有网关后运行 `--migrate-only`，无需另外开启变量：

```bash
# 可选检查，不转换数据库。
docker compose -f deploy/compose.yaml run --rm --no-deps codex-proxy-rs --check

# 可选的一次性模式，须先停止全部网关及其他数据库客户端。
docker compose -f deploy/compose.yaml run --rm --no-deps codex-proxy-rs --migrate-only
```

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
docker compose -f deploy/compose.yaml pull codex-proxy-rs &&
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
