-- Provider 默认选择仅在首次初始化时写入；后续以管理端保存的配置为准。
alter table runtime_settings
    add column provider_request_profiles_json jsonb not null default '{}'::jsonb,
    add constraint runtime_settings_request_profiles_object
        check (jsonb_typeof(provider_request_profiles_json) = 'object');

-- 空对象表示各 Provider 均跟随通用设置，只保存显式独立覆盖。
alter table client_api_keys
    add column provider_request_profiles_json jsonb not null default '{}'::jsonb,
    add constraint client_api_keys_request_profiles_object
        check (jsonb_typeof(provider_request_profiles_json) = 'object');

-- 覆盖配置与请求历史明细分离，改价不重算已发生的费用。
alter table runtime_settings add column pricing_overrides_json jsonb not null default '{}'
    check (jsonb_typeof(pricing_overrides_json) = 'object');
alter table runtime_settings add column pricing_synced_json jsonb not null default '{}'
    check (jsonb_typeof(pricing_synced_json) = 'object');
alter table runtime_settings add column pricing_synced_at timestamptz;
alter table model_requests add column billing_snapshot_json jsonb
    check (billing_snapshot_json is null or jsonb_typeof(billing_snapshot_json) = 'object');

alter table runtime_settings drop column disable_fast;
