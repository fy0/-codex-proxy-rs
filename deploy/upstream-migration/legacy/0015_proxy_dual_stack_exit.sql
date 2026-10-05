-- 代理记录双栈出口（IPv4 与 IPv6）探测结果。
alter table outbound_proxies
    add column last_test_ipv4 text,
    add column last_test_ipv6 text;
