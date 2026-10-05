-- 管理员配置的账号级 x-codex-turn-state 强制覆盖；空值表示不覆盖。
alter table provider_accounts
    add column turn_state_override text
    check (turn_state_override is null or char_length(turn_state_override) <= 1024);
