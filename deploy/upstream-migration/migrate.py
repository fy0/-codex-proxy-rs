#!/usr/bin/env python3
"""将已知 legacy 数据库一次性转换为固定的官方迁移历史；默认不改库。"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field
from typing import Any
from uuid import uuid4

BASE = Path(__file__).resolve().parent
ENABLE_ENV = "CPR_MIGRATE_LEGACY_TO_UPSTREAM"
FORK_TABLES = frozenset({
    "account_turn_state_notifications", "account_turn_state_events",
    "account_turn_states", "openai_routing_cookies",
})
COMPENSATE_SQL = """
DROP TRIGGER IF EXISTS account_turn_state_identity_changed ON public.provider_accounts;
DROP FUNCTION IF EXISTS public.clear_turn_state_on_identity_change();
DROP TABLE IF EXISTS public.account_turn_state_notifications;
DROP TABLE IF EXISTS public.account_turn_state_events;
DROP TABLE IF EXISTS public.account_turn_states;
DROP TABLE IF EXISTS public.openai_routing_cookies;
ALTER TABLE public.provider_accounts
    DROP COLUMN IF EXISTS turn_state_override,
    DROP COLUMN IF EXISTS basispoints_enabled,
    DROP COLUMN IF EXISTS bps_concurrency_limit;
ALTER TABLE public.runtime_settings DROP COLUMN IF EXISTS bps_model_mappings_json;
"""


class MigrationError(RuntimeError):
    """仅包含可以安全输出的诊断信息，不携带数据库值或连接字符串。"""


@dataclass(frozen=True)
class Settings:
    dsn: str = field(repr=False)
    password: str = field(repr=False)
    backup_dir: Path


def load_bundle(base: Path = BASE) -> dict[str, Any]:
    metadata = json.loads((base / "manifest.json").read_text(encoding="utf-8"))
    official_dir = base / "official"
    if not official_dir.is_dir():
        official_dir = base.parent.parent / "backend" / "migrations"
    for kind, directory in (("legacy", base / "legacy"), ("official", official_dir)):
        entries = metadata[kind]
        if {p.name for p in directory.glob("*.sql")} != {e["file"] for e in entries}:
            raise MigrationError(f"{kind} 迁移文件集合与固定清单不一致")
        for version, entry in enumerate(entries, start=1):
            name = entry["file"]
            if not re.fullmatch(r"[0-9]{4}_[a-z0-9_]+\.sql", name):
                raise MigrationError("迁移清单包含非法文件名")
            if entry["version"] != version or int(name[:4]) != version:
                raise MigrationError("迁移清单不是连续前缀")
            if entry["description"] != name.split("_", 1)[1][:-4].replace("_", " "):
                raise MigrationError("迁移描述与文件名不一致")
            body = (directory / name).read_bytes()
            if (hashlib.sha384(body).hexdigest() != entry["sha384"]
                    or hashlib.sha256(body).hexdigest() != entry["sha256"]):
                raise MigrationError(f"{kind} 迁移文件校验失败：{name}")
            # 事务由本程序统一管理，不接受非事务迁移或改变事务边界的文件。
            text = body.decode("utf-8")
            if "-- no-transaction" in text or re.search(
                r"(?im)^\s*(?:begin|commit|rollback)\s*;", text
            ):
                raise MigrationError("迁移文件试图接管事务")
            entry["sql"] = text
    shared = metadata["shared_through"]
    if shared != 15 or len(metadata["legacy"]) != 30 or len(metadata["official"]) != 21:
        raise MigrationError("此程序仅支持固定的 legacy 0016–0030 到官方 3.19.0")
    if any(metadata["legacy"][i]["sha384"] != metadata["official"][i]["sha384"]
           for i in range(shared)):
        raise MigrationError("共同迁移基线不一致")
    return metadata


def classify_history(rows: list[tuple], bundle: dict[str, Any]) -> str:
    """只接受完整、成功且 checksum 匹配的已知前缀，绝不猜测未知历史。"""
    if not rows:
        return "empty"
    for kind in ("official", "legacy"):
        expected = bundle[kind]
        if len(rows) > len(expected):
            continue
        if all(version == entry["version"] and description == entry["description"]
               and success is True and checksum == entry["sha384"]
               for (version, description, success, checksum), entry
               in zip(rows, expected)):
            return "shared" if len(rows) <= bundle["shared_through"] else kind
    raise MigrationError("未知、缺号、失败或 checksum 不符的迁移历史；数据库未转换")


def read_settings() -> Settings:
    import yaml

    config_path = Path(os.environ.get("CPR_LEGACY_CONFIG_PATH", "/app/deploy/config.yaml"))
    config: dict[str, Any] = {}
    if config_path.is_file():
        try:
            config = yaml.safe_load(config_path.read_text(encoding="utf-8")) or {}
        except Exception:
            raise MigrationError("无法读取迁移所需的部署配置") from None
    try:
        database = config.get("store", {}).get("database", {})
        dsn = os.environ.get("CPR_DATABASE_URL") or database.get("url", "")
        password = os.environ.get("CPR_DATABASE_PASSWORD") or database.get("password", "")
        runtime_dir = config.get("host", {}).get("runtime_data_dir", "../.runtime/data")
        backup_dir = Path(os.environ.get("CPR_LEGACY_BACKUP_DIR") or
                          str((config_path.parent / runtime_dir).resolve() / "upstream-migration"))
        if not isinstance(dsn, str) or not dsn.strip() or not isinstance(password, str) or not password:
            raise MigrationError("必须配置 PostgreSQL URL 和密码；凭据不应写入命令行")
        if not backup_dir.is_absolute():
            raise MigrationError("CPR_LEGACY_BACKUP_DIR 必须是持久化目录的绝对路径")
        return Settings(dsn, password, backup_dir)
    except MigrationError:
        raise
    except Exception:
        raise MigrationError("迁移配置结构无效") from None


def connect_database(settings: Settings):
    import psycopg2

    return psycopg2.connect(settings.dsn, password=settings.password,
                            application_name="codex-proxy-rs:legacy-migration", connect_timeout=10)


def read_history(cursor) -> list[tuple]:
    cursor.execute("SELECT to_regclass('public._sqlx_migrations')")
    if cursor.fetchone()[0] is None:
        return []
    cursor.execute("""SELECT version, description, success, encode(checksum, 'hex')
                      FROM public._sqlx_migrations ORDER BY version""")
    return cursor.fetchall()


def public_tables(cursor) -> list[str]:
    cursor.execute("""SELECT c.relname FROM pg_catalog.pg_class c
                      JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                      WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p')
                      ORDER BY c.relname""")
    return [row[0] for row in cursor.fetchall()]


def verify_fork_shape(cursor, status: str, version: int) -> None:
    introduced = {
        "table:account_turn_states": 17, "table:account_turn_state_events": 17,
        "table:account_turn_state_notifications": 19, "table:openai_routing_cookies": 22,
        "column:provider_accounts.turn_state_override": 16,
        "column:provider_accounts.basispoints_enabled": 27,
        "column:provider_accounts.bps_concurrency_limit": 29,
        "column:runtime_settings.bps_model_mappings_json": 28,
        "function:clear_turn_state_on_identity_change": 17,
        "trigger:account_turn_state_identity_changed": 17,
    }
    cursor.execute("""
        SELECT 'table:' || table_name FROM information_schema.tables WHERE table_schema = 'public'
        UNION ALL
        SELECT 'column:' || table_name || '.' || column_name FROM information_schema.columns
            WHERE table_schema = 'public'
        UNION ALL
        SELECT 'function:' || p.proname FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
            WHERE n.nspname = 'public'
        UNION ALL
        SELECT 'trigger:' || t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
            JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public'
    """)
    actual = {row[0] for row in cursor.fetchall()} & introduced.keys()
    expected = {name for name, start in introduced.items() if status == "legacy" and version >= start}
    if actual != expected:
        raise MigrationError("fork 专有结构与迁移历史矛盾；拒绝转换或认定为官方数据库")


def require_stopped(cursor) -> None:
    # 活动视图在事务内缓存；备份前后的停机检查必须观察新的连接状态。
    cursor.execute("SELECT pg_catalog.pg_stat_clear_snapshot()")
    cursor.execute("""SELECT count(*) FROM pg_catalog.pg_stat_activity
                      WHERE datname = current_database() AND pid <> pg_backend_pid()
                      AND (backend_type = 'client backend' OR backend_type IS NULL)""")
    if cursor.fetchone()[0]:
        raise MigrationError("数据库仍有其他客户端连接；请停止所有网关和其他数据库客户端后重试")


def table_counts(cursor, names: list[str]) -> dict[str, int]:
    from psycopg2 import sql

    result = {}
    for name in names:
        cursor.execute(sql.SQL("SELECT count(*) FROM public.{}").format(sql.Identifier(name)))
        result[name] = cursor.fetchone()[0]
    return result


def backup_database(connection, settings: Settings, bundle: dict[str, Any], rows: list[tuple]) -> Path:
    from psycopg2.extensions import make_dsn

    directory = settings.backup_dir
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    identity = f"legacy-{uuid4().hex}"
    partial = directory / f"{identity}.partial"
    archive = directory / f"{identity}.dump"
    # 以实际已连接的 libpq 参数为准，避免备份和迁移指向两个不同的数据库。
    parameters = connection.get_dsn_parameters()
    parameters.pop("password", None)
    parameters["application_name"] = "codex-proxy-rs:legacy-backup"
    environment = {k: v for k, v in os.environ.items() if not k.startswith("PG")}
    environment.update(PGPASSWORD=settings.password, PGCONNECT_TIMEOUT="10")
    # libpq 不保证将环境变量 PGDATABASE 中的 conninfo 展开；显式传递不含密码的 DSN。
    backup_dsn = make_dsn(**parameters)
    try:
        descriptor = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as stream:
            completed = subprocess.run(
                ["pg_dump", "--format=custom", "--no-owner", "--no-privileges",
                 "--no-password", "--lock-wait-timeout=5s", "--dbname", backup_dsn],
                env=environment, stdin=subprocess.DEVNULL, stdout=stream, stderr=subprocess.DEVNULL,
                timeout=1800, check=False,
            )
            if completed.returncode:
                raise MigrationError("pg_dump 失败；数据库未转换")
            stream.flush()
            os.fsync(stream.fileno())
        with partial.open("rb") as stream:
            if stream.read(5) != b"PGDMP":
                raise MigrationError("备份文件不是有效的 PostgreSQL custom archive")
        checked = subprocess.run(["pg_restore", "--list", str(partial)],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, timeout=60, check=False)
        if checked.returncode:
            raise MigrationError("pg_restore 无法读取备份；数据库未转换")
        digest = hashlib.sha256()
        with partial.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        os.replace(partial, archive)
        receipt = {"source_commit": bundle["source_commit"], "target_commit": bundle["target_commit"],
                   "target_tag": bundle["target_tag"], "source_migrations": rows,
                   "archive": archive.name, "bytes": archive.stat().st_size,
                   "sha256": digest.hexdigest()}
        descriptor = os.open(directory / f"{identity}.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(receipt, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        # Linux 容器中连目录项一起落盘；备份及回执完成后才允许删除旧结构。
        if os.name == "posix":
            descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        print(f"完整数据库备份已保存：{archive}", flush=True)
        return archive
    except (OSError, subprocess.TimeoutExpired):
        raise MigrationError("无法完成持久化数据库备份；数据库未转换") from None
    finally:
        partial.unlink(missing_ok=True)


def migrate_database(connection, settings: Settings, bundle: dict[str, Any], *,
                     enabled: bool, check_only: bool = False) -> tuple[str, Path | None]:
    from psycopg2 import sql

    archive = None
    # 包含补偿、真正执行官方 SQL 和写入 sqlx 历史的单一事务。
    with connection:
        with connection.cursor() as cursor:
            cursor.execute("SELECT current_schema(), pg_is_in_recovery()")
            schema, recovering = cursor.fetchone()
            if schema != "public" or recovering:
                raise MigrationError("只支持主库的 public schema；请核对部署配置")
            cursor.execute("SET LOCAL search_path = public")
            cursor.execute("SET LOCAL lock_timeout = '5s'")
            cursor.execute("SET LOCAL statement_timeout = '15min'")
            cursor.execute("SET LOCAL idle_in_transaction_session_timeout = '35min'")
            cursor.execute("SELECT pg_try_advisory_xact_lock(7416613174123)")
            if not cursor.fetchone()[0]:
                raise MigrationError("已有另一个迁移程序运行；数据库未转换")
            rows = read_history(cursor)
            status = classify_history(rows, bundle)
            names = public_tables(cursor)
            if status == "empty" and any(name != "_sqlx_migrations" for name in names):
                raise MigrationError("数据库已有业务表但缺少迁移历史；拒绝猜测来源")
            verify_fork_shape(cursor, status, len(rows))
            if status != "legacy" or check_only:
                return status, None
            if not enabled:
                raise MigrationError(f"已识别 legacy 数据库；必须显式设置 {ENABLE_ENV}=1 才能转换")
            require_stopped(cursor)
            # SHARE 阻止写入，但允许另一个连接执行 pg_dump 的 ACCESS SHARE 读取。
            cursor.execute(sql.SQL("LOCK TABLE {} IN SHARE MODE").format(sql.SQL(", ").join(
                sql.Identifier("public", name) for name in names)))
            # 等待锁期间状态可能变化，因此所有破坏性动作都以锁内重新校验为准。
            locked_rows = read_history(cursor)
            if locked_rows != rows or classify_history(locked_rows, bundle) != "legacy":
                raise MigrationError("取得表锁前迁移历史发生变化；请重新检查")
            require_stopped(cursor)
            verify_fork_shape(cursor, "legacy", len(locked_rows))
            retained = [name for name in names if name not in FORK_TABLES and name != "_sqlx_migrations"]
            before = table_counts(cursor, retained)
            archive = backup_database(connection, settings, bundle, rows)
            require_stopped(cursor)
            cursor.execute(COMPENSATE_SQL)
            cursor.execute("DELETE FROM public._sqlx_migrations WHERE version > %s",
                           (bundle["shared_through"],))
            for migration in bundle["official"][bundle["shared_through"]:]:
                started = time.perf_counter_ns()
                cursor.execute(migration["sql"])
                elapsed = time.perf_counter_ns() - started
                # 只有实际成功执行的原始官方 SQL 才登记其 SHA-384，不伪造 checksum。
                cursor.execute("""INSERT INTO public._sqlx_migrations
                    (version, description, success, checksum, execution_time)
                    VALUES (%s, %s, true, %s, %s)""",
                    (migration["version"], migration["description"],
                     bytes.fromhex(migration["sha384"]), elapsed))
            final_rows = read_history(cursor)
            if classify_history(final_rows, bundle) != "official" or len(final_rows) != len(bundle["official"]):
                raise MigrationError("官方迁移历史验证失败；事务将回滚")
            verify_fork_shape(cursor, "official", len(final_rows))
            if table_counts(cursor, retained) != before:
                raise MigrationError("共同业务表行数发生变化；事务将回滚")
    return "migrated", archive


def main(arguments: list[str] | None = None) -> int:
    arguments = sys.argv[1:] if arguments is None else arguments
    check_only = arguments == ["--check"]
    migration_only = arguments == ["--migrate-only"]
    enabled = os.environ.get(ENABLE_ENV, "0")
    if enabled not in ("0", "1"):
        raise MigrationError(f"{ENABLE_ENV} 仅接受 0 或 1")
    if check_only or migration_only or enabled == "1":
        bundle = load_bundle()
        settings = read_settings()
        connection = connect_database(settings)
        try:
            status, _archive = migrate_database(connection, settings, bundle,
                                                enabled=enabled == "1", check_only=check_only)
        finally:
            connection.close()
        print(f"数据库状态：{status}；固定目标：{bundle['target_tag']}；" +
              ("检查模式，未执行转换" if check_only else "可交给该官方版本启动"), flush=True)
        if check_only or migration_only:
            return 0
    command = arguments or ["/app/bin/codex-proxy-rs"]
    os.execvp(command[0], command)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except MigrationError as error:
        print(f"迁移已停止：{error}", file=sys.stderr)
        sys.exit(1)
    except Exception as error:
        # 驱动异常可能含密码、SQL 或失败行，不能直接打印或输出 traceback。
        code = getattr(error, "pgcode", None)
        suffix = f"（SQLSTATE {code}）" if isinstance(code, str) and re.fullmatch(r"[0-9A-Z]{5}", code) else ""
        print(f"迁移未确认成功{suffix}；请先运行 --check，备份文件会保留。", file=sys.stderr)
        sys.exit(1)
