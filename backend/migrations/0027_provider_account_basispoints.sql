-- OpenAI OAuth 账号可选走 Basis Points 上游通道；默认关闭保持现有 Codex 路由。
alter table provider_accounts
    add column basispoints_enabled boolean not null default false;
