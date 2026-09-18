-- 自动覆盖按账号和模型隔离；签发时间水位保留，过期时只清除令牌正文。
create table account_turn_states (
    account_id text not null references provider_accounts(id) on delete cascade,
    upstream_account_id text,
    upstream_user_id text,
    model text not null check (char_length(model) between 1 and 256),
    config jsonb not null,
    turn_state_override text,
    current_issued_at bigint,
    current_length integer,
    candidate text,
    candidate_issued_at bigint,
    candidate_source text,
    candidate_observed_at bigint,
    candidate_attempts bigint,
    candidate_hunt_started_at bigint,
    hunt_attempts bigint not null default 0,
    hunt_started_at bigint,
    next_probe_at bigint,
    primary key (account_id, model)
);

create table account_turn_state_events (
    id bigserial primary key,
    account_id text not null,
    model text not null,
    event_kind text not null check (event_kind in ('observation', 'installation')),
    detail jsonb not null,
    foreign key (account_id, model) references account_turn_states(account_id, model) on delete cascade
);
create index account_turn_state_events_bucket on account_turn_state_events(account_id, model, event_kind, id desc);

-- 同一本地账号重新授权到新身份时保留配置，但绝不能继承原身份的令牌或观测。
create function clear_turn_state_on_identity_change() returns trigger language plpgsql as $$
begin
    update account_turn_states set upstream_account_id = new.upstream_account_id,
        upstream_user_id = new.upstream_user_id,
        turn_state_override = null, current_issued_at = null, current_length = null,
        candidate = null, candidate_issued_at = null, candidate_source = null,
        candidate_observed_at = null, candidate_attempts = null, candidate_hunt_started_at = null,
        hunt_attempts = 0, hunt_started_at = null, next_probe_at = null
    where account_id = new.id;
    delete from account_turn_state_events where account_id = new.id;
    return new;
end;
$$;
create trigger account_turn_state_identity_changed
after update of upstream_account_id, upstream_user_id, authentication_kind on provider_accounts
for each row when (old.upstream_account_id is distinct from new.upstream_account_id
    or old.upstream_user_id is distinct from new.upstream_user_id
    or old.authentication_kind is distinct from new.authentication_kind)
execute function clear_turn_state_on_identity_change();
