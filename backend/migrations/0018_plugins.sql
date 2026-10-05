-- 更新规则可变；制品保存安装时的来源事实，包体随 PostgreSQL 备份恢复。
create table plugin_update_sources (
    plugin_id text primary key check (
        octet_length(plugin_id) <= 64
        and plugin_id ~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?[.][a-z0-9]([a-z0-9-]*[a-z0-9])?$'
    ),
    source_json jsonb not null check (jsonb_typeof(source_json) = 'object'),
    policy_json jsonb not null default '{"kind":"manual"}'::jsonb
        check (jsonb_typeof(policy_json) = 'object'),
    outbound_proxy_id text references outbound_proxies(id) on delete restrict
);

create index plugin_update_sources_outbound_proxy_id_idx on plugin_update_sources(outbound_proxy_id);

create table plugin_artifacts (
    sha256 text primary key check (sha256 ~ '^[0-9a-f]{64}$'),
    plugin_id text not null references plugin_update_sources(plugin_id),
    version text not null check (length(version) between 1 and 128),
    metadata_json jsonb not null check (jsonb_typeof(metadata_json) = 'object'),
    source_json jsonb not null check (jsonb_typeof(source_json) = 'object'),
    -- 历史下载引用阻止代理被误删；地址与认证仍只存在受管代理记录中。
    outbound_proxy_id text references outbound_proxies(id) on delete restrict,
    archive bytea not null check (octet_length(archive) between 1 and 33554432),
    installed_at timestamptz not null default now(),
    -- 接受事实绑定不可变包；实例运行权限从包声明派生。
    accepted_at timestamptz
);

create index plugin_artifacts_plugin_id_idx on plugin_artifacts(plugin_id);
create index plugin_artifacts_outbound_proxy_id_idx on plugin_artifacts(outbound_proxy_id);

-- 平台逐项约束，避免通过增加另一平台条目绕过同版本不可变规则。
create table plugin_artifact_platforms (
    plugin_id text not null,
    version text not null,
    platform text not null,
    artifact_sha256 text not null references plugin_artifacts(sha256) on delete cascade,
    primary key (plugin_id, version, platform)
);

create index plugin_artifact_platforms_artifact_sha256_idx on plugin_artifact_platforms(artifact_sha256);

-- 下载鉴权单独持久化；普通列表和审计只返回 info_json，不读取 secret_json。
create table plugin_source_credentials (
    id uuid primary key,
    info_json jsonb not null check (jsonb_typeof(info_json) = 'object'),
    secret_json jsonb not null check (jsonb_typeof(secret_json) = 'object')
);

create table plugin_artifact_credentials (
    artifact_sha256 text not null references plugin_artifacts(sha256) on delete cascade,
    credential_id uuid not null references plugin_source_credentials(id),
    primary key (artifact_sha256, credential_id)
);

create index plugin_artifact_credentials_credential_id_idx on plugin_artifact_credentials(credential_id);

create table plugin_instances (
    id uuid primary key,
    artifact_sha256 text not null references plugin_artifacts(sha256),
    name text not null check (length(name) between 1 and 128),
    enabled boolean not null,
    configuration_json jsonb not null check (jsonb_typeof(configuration_json) = 'object' and octet_length(configuration_json::text) <= 65536),
    bindings_json jsonb not null check (jsonb_typeof(bindings_json) = 'array'),
    revision bigint not null check (revision > 0)
);

create index plugin_instances_artifact_sha256_idx on plugin_instances(artifact_sha256);

create table plugin_instance_secrets (
    instance_id uuid primary key references plugin_instances(id) on delete cascade,
    secrets_json jsonb not null check (jsonb_typeof(secrets_json) = 'object' and octet_length(secrets_json::text) <= 65536)
);

-- 配置恢复与插件私有状态迁移分离，删除实例或制品时同步清理对应快照。
create table plugin_version_configurations (
    instance_id uuid not null references plugin_instances(id) on delete cascade,
    artifact_sha256 text not null references plugin_artifacts(sha256) on delete cascade,
    configuration_json jsonb not null check (jsonb_typeof(configuration_json) = 'object'),
    secrets_json jsonb not null check (jsonb_typeof(secrets_json) = 'object'),
    bindings_json jsonb not null check (jsonb_typeof(bindings_json) = 'array'),
    primary key (instance_id, artifact_sha256)
);

-- 私有状态与配置 revision 分离；普通状态写入不得推进全局 revision。
create table plugin_state_generations (
    id uuid primary key,
    transition_id uuid,
    instance_id uuid not null references plugin_instances(id) on delete cascade,
    namespace text not null check (namespace ~ '^[a-z][a-z0-9_.-]{0,63}$'),
    artifact_sha256 text not null references plugin_artifacts(sha256),
    schema_version bigint not null check (schema_version between 1 and 4294967295),
    schema_sha256 text not null check (schema_sha256 ~ '^[0-9a-f]{64}$'),
    maximum_records bigint not null check (maximum_records between 1 and 10000),
    maximum_bytes bigint not null check (maximum_bytes between 1 and 16777216),
    maximum_value_bytes bigint not null check (
        maximum_value_bytes between 1 and 262144
        and maximum_value_bytes <= maximum_bytes
    ),
    instance_revision bigint not null check (instance_revision > 0),
    fence uuid not null,
    status text not null check (status in ('staging', 'active')),
    source_generation_id uuid references plugin_state_generations(id),
    next_record_version bigint not null default 1 check (next_record_version > 0),
    record_count bigint not null default 0 check (record_count between 0 and maximum_records),
    total_bytes bigint not null default 0 check (total_bytes between 0 and maximum_bytes),
    migration_cursor text,
    migration_complete boolean not null default false,
    created_at timestamptz not null default now(),
    promoted_at timestamptz,
    unique (instance_id, namespace, status),
    unique (transition_id, namespace),
    check (source_generation_id is null or source_generation_id <> id),
    check (
        (status = 'staging' and transition_id is not null)
        or (status = 'active' and transition_id is null and source_generation_id is null
            and migration_cursor is null and migration_complete and promoted_at is not null)
    )
);

create index plugin_state_generations_artifact_sha256_idx on plugin_state_generations(artifact_sha256);
create index plugin_state_generations_source_generation_id_idx on plugin_state_generations(source_generation_id);

create table plugin_state_records (
    generation_id uuid not null references plugin_state_generations(id) on delete cascade,
    -- 游标分页与 Rust 字符串比较都按 UTF-8 字节序，不受数据库 locale 影响。
    state_key text collate "C" not null check (
        length(state_key) between 1 and 256
        and octet_length(state_key) <= 512
        and state_key !~ '[[:cntrl:]]'
    ),
    value_json jsonb not null,
    value_bytes bigint not null check (value_bytes between 1 and 262144),
    record_version bigint not null check (record_version > 0),
    primary key (generation_id, state_key)
);

-- 授权回执与账号写入同事务提交；不保存 flow、回调地址或凭据文档。
create table authorization_receipts (
    provider_kind text not null,
    flow_digest text not null check (flow_digest ~ '^[0-9a-f]{64}$'),
    owner_digest text not null check (owner_digest ~ '^[0-9a-f]{64}$'),
    config_revision bigint not null check (config_revision > 0),
    accounts_json jsonb not null check (
        jsonb_typeof(accounts_json) = 'array'
        and jsonb_array_length(accounts_json) between 1 and 200
        and octet_length(accounts_json::text) <= 65536
    ),
    created_at timestamptz not null default now(),
    expires_at timestamptz not null default (now() + interval '24 hours'),
    primary key (provider_kind, flow_digest),
    check (expires_at > created_at)
);

create index authorization_receipts_expiry_idx on authorization_receipts (expires_at);

-- 手动位置与自动检测结果独立保存，关闭自动模式可恢复原有配置。
alter table outbound_proxies
    add column auto_location boolean not null default false,
    add column detected_location_json jsonb,
    add column last_location_detection_json jsonb not null default '{"status":"notRequested"}'::jsonb;

-- 定时账号预热默认关闭，启用时必须显式选择模型。
alter table runtime_settings
    add column account_warmup_enabled boolean not null default false,
    add column account_warmup_schedule_time text not null default '08:00'
        check (
            octet_length(account_warmup_schedule_time) between 5 and 255
            and account_warmup_schedule_time = btrim(account_warmup_schedule_time)
            and account_warmup_schedule_time !~ '[[:cntrl:]]'
            and account_warmup_schedule_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9](,([01][0-9]|2[0-3]):[0-5][0-9])*$'
        ),
    add column account_warmup_model text
        check (
            account_warmup_model is null
            or (
                octet_length(account_warmup_model) between 1 and 128
                and account_warmup_model = btrim(account_warmup_model)
                and account_warmup_model !~ '[[:cntrl:]]'
            )
        ),
    add constraint account_warmup_requires_model
        check (not account_warmup_enabled or account_warmup_model is not null);
