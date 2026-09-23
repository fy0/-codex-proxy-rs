-- 管理员可在共享 Cookie 池中固定某个网关，失效后不自动换到其它网关。
alter table account_turn_states add column cookie_override_pod text;
alter table account_turn_states add constraint account_turn_states_cookie_override_pod_length
    check (cookie_override_pod is null or char_length(cookie_override_pod) <= 256);

create or replace function clear_turn_state_on_identity_change() returns trigger language plpgsql as $$
begin
    update account_turn_states set upstream_account_id = new.upstream_account_id,
        upstream_user_id = new.upstream_user_id,
        turn_state_override = null, current_issued_at = null, current_length = null,
        candidate = null, candidate_issued_at = null, candidate_source = null,
        candidate_observed_at = null, candidate_attempts = null, candidate_hunt_started_at = null,
        hunt_attempts = 0, hunt_started_at = null, next_probe_at = null,
        manual_probe_requested_at = null, manual_override = false,
        attached_model = null, cookie_override_pod = null
    where account_id = new.id;
    delete from account_turn_state_events where account_id = new.id;
    return new;
end;
$$;
