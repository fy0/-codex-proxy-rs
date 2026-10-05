-- 实际模型检测需要记住本张票已经附着的上游模型；重新授权不能带走它。
alter table account_turn_states add column attached_model text;

create or replace function clear_turn_state_on_identity_change() returns trigger language plpgsql as $$
begin
    update account_turn_states set upstream_account_id = new.upstream_account_id,
        upstream_user_id = new.upstream_user_id,
        turn_state_override = null, current_issued_at = null, current_length = null,
        candidate = null, candidate_issued_at = null, candidate_source = null,
        candidate_observed_at = null, candidate_attempts = null, candidate_hunt_started_at = null,
        hunt_attempts = 0, hunt_started_at = null, next_probe_at = null,
        manual_probe_requested_at = null, manual_override = false,
        attached_model = null
    where account_id = new.id;
    delete from account_turn_state_events where account_id = new.id;
    return new;
end;
$$;
