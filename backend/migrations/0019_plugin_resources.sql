-- 归属与稳定资源键属于宿主；升级沿用实例，删除实例只解除归属，不删除业务资源。
create table plugin_group_resources (
    instance_id uuid not null references plugin_instances(id) on delete cascade,
    resource_key text not null check (resource_key ~ '^[a-z0-9][a-z0-9_.-]{0,63}$'),
    group_id text not null unique references account_groups(id) on delete cascade,
    primary key (instance_id, resource_key)
);

create table plugin_key_resources (
    instance_id uuid not null references plugin_instances(id) on delete cascade,
    resource_key text not null check (resource_key ~ '^[a-z0-9][a-z0-9_.-]{0,63}$'),
    key_id text not null unique references client_api_keys(id) on delete cascade,
    primary key (instance_id, resource_key)
);

-- 保留默认调度行为；评分系数与回切偏好随运行设置原子发布。
alter table runtime_settings add column smart_scheduling_json jsonb not null default
    '{"loadWeight":1.0,"quotaWeight":0.8,"healthWeight":1.0,"latencyWeight":0.5,"resetWeight":0.0,"queueWeight":0.0,"preferHigherWeight":false}'::jsonb
    check (jsonb_typeof(smart_scheduling_json) = 'object');
