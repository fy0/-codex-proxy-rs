-- 账号内 Basis Points 子池上限；NULL 表示 BPS 请求只受账号总并发约束。
alter table provider_accounts
    add column bps_concurrency_limit bigint,
    add constraint provider_accounts_bps_concurrency_limit_ck check (
        bps_concurrency_limit is null or bps_concurrency_limit between 1 and 4294967295
    );
