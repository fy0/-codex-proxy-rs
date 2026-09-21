-- 旧的一小时默认寿命会在上游窗口收紧后继续注入已失效的 292。
-- 只改写仍为 3600/2100 的旧默认，自定义寿命和轮换龄保留。
update account_turn_states
set config = jsonb_set(
    jsonb_set(config, '{ttlSeconds}', '240'::jsonb),
    '{refreshAfterSeconds}', '120'::jsonb)
where config->>'ttlSeconds' = '3600'
  and config->>'refreshAfterSeconds' = '2100';
