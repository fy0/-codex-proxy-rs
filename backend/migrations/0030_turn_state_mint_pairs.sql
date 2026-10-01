-- 780 票据与路由 Cookie pair（__cflb + __oailb）持久化。
-- 旧 pool 行没有 cflb 半边：字段允许 NULL，缺半边的旧记录不可回放。
alter table openai_routing_cookies add column cflb_name text;
alter table openai_routing_cookies add column cflb_value text;
alter table openai_routing_cookies add column cflb_expires_at bigint;

alter table openai_routing_cookies add constraint openai_routing_cookies_cflb_name_length
    check (cflb_name is null or char_length(cflb_name) <= 64);
alter table openai_routing_cookies add constraint openai_routing_cookies_cflb_value_length
    check (cflb_value is null or char_length(cflb_value) <= 4096);

-- 已安装/候选票绑定的完整 pair 与各来源计算出的有效到期点。
-- pair 以 JSON 存放完整 RoutingCookie（含两个值），换票或身份变更时必须一并清除。
alter table account_turn_states add column installed_pair jsonb;
alter table account_turn_states add column candidate_pair jsonb;
alter table account_turn_states add column current_expires_at bigint;
alter table account_turn_states add column candidate_expires_at bigint;
alter table account_turn_states add column cookie_override_cflb_name text;
alter table account_turn_states add column cookie_override_cflb_value text;

alter table account_turn_states add constraint account_turn_states_cookie_override_cflb_name_length
    check (cookie_override_cflb_name is null or char_length(cookie_override_cflb_name) <= 64);
alter table account_turn_states add constraint account_turn_states_cookie_override_cflb_value_length
    check (cookie_override_cflb_value is null or char_length(cookie_override_cflb_value) <= 4096);

-- 目标长度从 292 升到 780：只迁移显式等于 292 的存量配置，其它显式长度原样保留。
update account_turn_states
    set config = jsonb_set(config, '{targetLength}', '780'::jsonb)
    where config->>'targetLength' = '292';

create or replace function clear_turn_state_on_identity_change() returns trigger language plpgsql as $$
begin
    update account_turn_states set upstream_account_id = new.upstream_account_id,
        upstream_user_id = new.upstream_user_id,
        turn_state_override = null, current_issued_at = null, current_length = null,
        current_expires_at = null, installed_pair = null,
        candidate = null, candidate_issued_at = null, candidate_source = null,
        candidate_observed_at = null, candidate_attempts = null, candidate_hunt_started_at = null,
        candidate_pair = null, candidate_expires_at = null,
        hunt_attempts = 0, hunt_started_at = null, next_probe_at = null,
        manual_probe_requested_at = null, manual_override = false,
        attached_model = null, cookie_override_pod = null, cookie_override_issued_at = null,
        cookie_override_name = null, cookie_override_value = null, cookie_override_expires_at = null,
        cookie_override_observation_id = null,
        cookie_override_cflb_name = null, cookie_override_cflb_value = null
    where account_id = new.id;
    delete from account_turn_state_events where account_id = new.id;
    return new;
end;
$$;
