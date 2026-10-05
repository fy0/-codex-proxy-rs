-- 按上游端点和 pod 共享路由凭证；失效行保留短期水位，阻止旧响应复活。
create table openai_routing_cookies (
    origin text not null,
    pod text not null,
    name text not null,
    value text,
    issued_at bigint not null,
    expires_at bigint not null,
    observed_at bigint not null,
    reported_model text not null,
    primary key (origin, pod)
);
create index openai_routing_cookies_expiry on openai_routing_cookies(expires_at);
