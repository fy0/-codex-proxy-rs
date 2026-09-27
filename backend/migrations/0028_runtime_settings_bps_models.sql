-- Basis Points 专用通道的公开模型别名映射；仅被列出的模型名走 bps.openai.com。
alter table runtime_settings
    add column bps_model_mappings_json jsonb not null default '{}'::jsonb;
