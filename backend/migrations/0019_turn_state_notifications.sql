-- 每桶只保留最近一次安装的通知任务，正文仍只存储在当前票中。
create table account_turn_state_notifications (
    account_id text not null,
    model text not null,
    issued_at bigint not null,
    installation jsonb not null,
    previous_installed_at bigint,
    attempts integer not null default 0,
    next_attempt_at bigint not null,
    sent_at bigint,
    primary key (account_id, model),
    foreign key (account_id, model) references account_turn_states(account_id, model) on delete cascade
);
