#!/bin/sh
# 仅在镜像构建的隔离阶段创建临时 PostgreSQL，不读取部署配置或生产 URL。
set -eu
export PATH="/usr/lib/postgresql/18/bin:$PATH"
DATA=/tmp/cpr-migration-postgres
install -d -o postgres -g postgres "$DATA"
runuser -u postgres -- initdb -D "$DATA" --auth=trust --no-locale --encoding=UTF8 >/dev/null
cleanup() {
    runuser -u postgres -- pg_ctl -D "$DATA" -m immediate -w stop >/dev/null 2>&1 || true
    rm -rf "$DATA"
}
trap cleanup EXIT INT TERM
runuser -u postgres -- pg_ctl -D "$DATA" -o '-h 127.0.0.1 -p 55432' -w start >/dev/null
export CPR_TEST_DATABASE_URL='postgresql://postgres:fixture-only@127.0.0.1:55432/postgres'
export CPR_REQUIRE_MIGRATION_DB_TESTS=1
export PYTHONDONTWRITEBYTECODE=1
python3 -m unittest discover -s /opt/upstream-migration -p 'test_*.py' -v
printf 'PostgreSQL 18 migration and restore tests passed\n' >/opt/migration-tests-passed
