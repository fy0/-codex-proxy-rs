-- 位置配置由代理持有；现有代理保持全局继承，不回填账号或凭据。
alter table outbound_proxies
    add column location_country text,
    add column location_region text,
    add column location_city text,
    add column location_timezone text,
    add constraint outbound_proxies_location_complete check (
        num_nonnulls(location_country, location_region, location_city, location_timezone) = 0
        or (
            num_nonnulls(location_country, location_region, location_city, location_timezone) = 4
            and location_country ~ '^[A-Z]{2}$'
            and char_length(btrim(location_region)) between 1 and 128
            and char_length(btrim(location_city)) between 1 and 128
            and char_length(location_timezone) > 0
        )
    );

-- 全局位置属于运行设置；初始值仅在开启全局覆盖后生效，关闭时保留客户端字段。
alter table runtime_settings
    add column request_location_enabled boolean not null default false,
    add column request_location_json jsonb not null default
        '{"country":"US","region":"Ohio","city":"Piketon","timezone":"America/New_York"}'::jsonb,
    add constraint runtime_settings_request_location_valid check (
        jsonb_typeof(request_location_json) = 'object'
        and request_location_json ?& array['country', 'region', 'city', 'timezone']
        and request_location_json - array['country', 'region', 'city', 'timezone'] = '{}'::jsonb
        and jsonb_typeof(request_location_json->'country') = 'string'
        and request_location_json->>'country' ~ '^[A-Z]{2}$'
        and jsonb_typeof(request_location_json->'region') = 'string'
        and char_length(btrim(request_location_json->>'region')) between 1 and 128
        and jsonb_typeof(request_location_json->'city') = 'string'
        and char_length(btrim(request_location_json->>'city')) between 1 and 128
        and jsonb_typeof(request_location_json->'timezone') = 'string'
        and char_length(request_location_json->>'timezone') > 0
    );
