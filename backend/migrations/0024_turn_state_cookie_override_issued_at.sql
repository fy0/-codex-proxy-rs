-- Cookie 固定的粒度从 pod 收敛到具体一条 Cookie：续约或替换后旧固定自动失效。
alter table account_turn_states add column cookie_override_issued_at bigint;

-- 存量 pod 级固定回填为池内当前 Cookie 实例；池中已无对应 Cookie 的固定一并清除。
update account_turn_states s set cookie_override_issued_at = c.issued_at
    from openai_routing_cookies c
    where c.pod = s.cookie_override_pod and c.value is not null;
update account_turn_states set cookie_override_pod = null
    where cookie_override_pod is not null and cookie_override_issued_at is null;

create or replace function clear_turn_state_on_identity_change() returns trigger language plpgsql as $$
begin
    update account_turn_states set upstream_account_id = new.upstream_account_id,
        upstream_user_id = new.upstream_user_id,
        turn_state_override = null, current_issued_at = null, current_length = null,
        candidate = null, candidate_issued_at = null, candidate_source = null,
        candidate_observed_at = null, candidate_attempts = null, candidate_hunt_started_at = null,
        hunt_attempts = 0, hunt_started_at = null, next_probe_at = null,
        manual_probe_requested_at = null, manual_override = false,
        attached_model = null, cookie_override_pod = null, cookie_override_issued_at = null
    where account_id = new.id;
    delete from account_turn_state_events where account_id = new.id;
    return new;
end;
$$;
