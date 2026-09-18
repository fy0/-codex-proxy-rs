-- 手动探测使用单槽队列，重复点击合并，worker 原子领取后只尝试一次。
alter table account_turn_states add column manual_probe_requested_at bigint;
alter table account_turn_states add column manual_override boolean not null default false;

-- 旧版默认的 45 分钟本地寿命统一改成一小时，保留其他自定义值。
update account_turn_states set config = jsonb_set(config, '{ttlSeconds}', '3600'::jsonb)
where config->>'ttlSeconds' = '2700';

-- 重新授权同时撤销旧身份的手动探测请求。
create or replace function clear_turn_state_on_identity_change() returns trigger language plpgsql as $$
begin
    update account_turn_states set upstream_account_id = new.upstream_account_id,
        upstream_user_id = new.upstream_user_id,
        turn_state_override = null, current_issued_at = null, current_length = null,
        candidate = null, candidate_issued_at = null, candidate_source = null,
        candidate_observed_at = null, candidate_attempts = null, candidate_hunt_started_at = null,
        hunt_attempts = 0, hunt_started_at = null, next_probe_at = null,
        manual_probe_requested_at = null, manual_override = false
    where account_id = new.id;
    delete from account_turn_state_events where account_id = new.id;
    return new;
end;
$$;
