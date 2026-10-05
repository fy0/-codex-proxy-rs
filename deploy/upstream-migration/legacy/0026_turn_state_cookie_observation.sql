-- 固定指向某一条探测记录，而不是同一个 pod。
alter table account_turn_states add column cookie_override_observation_id bigint;

create or replace function clear_turn_state_on_identity_change() returns trigger language plpgsql as $$
begin
    update account_turn_states set upstream_account_id = new.upstream_account_id,
        upstream_user_id = new.upstream_user_id,
        turn_state_override = null, current_issued_at = null, current_length = null,
        candidate = null, candidate_issued_at = null, candidate_source = null,
        candidate_observed_at = null, candidate_attempts = null, candidate_hunt_started_at = null,
        hunt_attempts = 0, hunt_started_at = null, next_probe_at = null,
        manual_probe_requested_at = null, manual_override = false,
        attached_model = null, cookie_override_pod = null, cookie_override_issued_at = null,
        cookie_override_name = null, cookie_override_value = null, cookie_override_expires_at = null,
        cookie_override_observation_id = null
    where account_id = new.id;
    delete from account_turn_state_events where account_id = new.id;
    return new;
end;
$$;
