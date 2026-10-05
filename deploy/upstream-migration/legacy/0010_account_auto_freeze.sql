-- 账号容量熔断：统计窗口内高频出现容量类上游错误时自动冻结账号一段时间；
-- 恢复前可选执行一次探测调用验证上游已恢复；冻结期间按观测到的在途并发峰值
-- 下调账号并发上限，让账号带着更安全的并发配置自愈。默认关闭，需在设置页显式启用。
alter table runtime_settings
    add column account_auto_freeze_enabled boolean not null default false,
    add column account_auto_freeze_threshold bigint not null default 12
        check (account_auto_freeze_threshold between 2 and 1000),
    add column account_auto_freeze_window_seconds bigint not null default 600
        check (account_auto_freeze_window_seconds between 60 and 3600),
    add column account_auto_freeze_duration_seconds bigint not null default 7200
        check (account_auto_freeze_duration_seconds between 300 and 604800),
    add column account_auto_freeze_probe_enabled boolean not null default true,
    add column account_auto_freeze_probe_model text
        check (
          account_auto_freeze_probe_model is null
          or (
            octet_length(account_auto_freeze_probe_model) between 1 and 128
            and account_auto_freeze_probe_model = btrim(account_auto_freeze_probe_model)
            and account_auto_freeze_probe_model !~ '[[:cntrl:]]'
          )
        ),
    add column account_auto_freeze_adaptive_concurrency boolean not null default true;
