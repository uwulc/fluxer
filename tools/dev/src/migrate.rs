// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::cassandra::{CassandraConfig, Table, UserType, config_from_env, connect, target_schema};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use chrono::NaiveDate;
use scylla::client::session::Session;
use scylla::value::{CqlDate, CqlTimestamp, CqlTimeuuid, CqlValue, CqlVarint};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use uuid::Uuid;

pub struct MigrateConfig {
    pub pg_host: String,
    pub pg_port: u16,
    pub pg_database: String,
    pub pg_user: String,
    pub pg_password: String,
    pub pg_kv_table: String,
    pub cassandra: CassandraConfig,
    pub dry_run: bool,
    pub only_tables: Vec<String>,
}

// Compose files forward unset settings as empty strings, so treat "" as unset.
fn env_or(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

// A typo in a safety switch must not silently mean "off" and run the real migration.
fn parse_bool(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => bail!("invalid {name}={other:?}; use true or false"),
    }
}

impl MigrateConfig {
    pub fn from_env() -> Result<Self> {
        Ok(MigrateConfig {
            pg_host: env_or("FLUXER_POSTGRES_HOST", "postgres"),
            pg_port: env_or("FLUXER_POSTGRES_PORT", "5432")
                .parse()
                .context("invalid FLUXER_POSTGRES_PORT")?,
            pg_database: env_or("FLUXER_POSTGRES_DATABASE", "fluxer"),
            pg_user: env_or("FLUXER_POSTGRES_USERNAME", "fluxer"),
            pg_password: env::var("FLUXER_POSTGRES_PASSWORD").unwrap_or_default(),
            pg_kv_table: env_or("FLUXER_POSTGRES_KV_TABLE", "fluxer_kv"),
            cassandra: config_from_env()?,
            dry_run: parse_bool(
                "FLUXER_MIGRATE_DRY_RUN",
                &env_or("FLUXER_MIGRATE_DRY_RUN", "false"),
            )?,
            only_tables: env::var("FLUXER_MIGRATE_TABLES")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

pub async fn run_migrate(config: MigrateConfig) -> Result<()> {
    let mut pg_config = tokio_postgres::Config::new();
    pg_config
        .host(&config.pg_host)
        .port(config.pg_port)
        .dbname(&config.pg_database)
        .user(&config.pg_user)
        .password(&config.pg_password);

    println!(
        "Connecting to Postgres at {}:{}...",
        config.pg_host, config.pg_port
    );
    let (pg, pg_conn) = pg_config
        .connect(tokio_postgres::NoTls)
        .await
        .context("failed to connect to Postgres")?;
    tokio::spawn(async move {
        if let Err(e) = pg_conn.await {
            eprintln!("postgres connection error: {e}");
        }
    });

    println!(
        "Connecting to ScyllaDB at {}:{}...",
        config.cassandra.host, config.cassandra.port
    );
    let scylla = connect(&config.cassandra)
        .await
        .context("failed to connect to ScyllaDB")?;

    let schema = target_schema();
    let udts: HashMap<&str, &UserType> = schema
        .user_types
        .iter()
        .map(|ut| (ut.name.as_str(), ut))
        .collect();

    for name in &config.only_tables {
        if !schema.tables.iter().any(|t| &t.name == name) {
            eprintln!(
                "warning: FLUXER_MIGRATE_TABLES names {name:?}, which is not in the target schema; skipping it"
            );
        }
    }

    let tables: Vec<&Table> = if config.only_tables.is_empty() {
        schema.tables.iter().collect()
    } else {
        schema
            .tables
            .iter()
            .filter(|t| config.only_tables.contains(&t.name))
            .collect()
    };

    let keyspace = &config.cassandra.keyspace;
    let kv_table = &config.pg_kv_table;

    if config.dry_run {
        println!("DRY RUN: no data will be written to ScyllaDB");
    }

    let mut total_rows = 0usize;
    let mut total_errors = 0usize;

    for table in &tables {
        print!("  {} ... ", table.name);
        match migrate_table(
            &pg,
            &scylla,
            kv_table,
            keyspace,
            table,
            &udts,
            config.dry_run,
        )
        .await
        {
            Ok((rows, errors)) => {
                total_rows += rows;
                total_errors += errors;
                if errors > 0 {
                    println!("{rows} rows ({errors} errors)");
                } else {
                    println!("{rows} rows");
                }
            }
            Err(e) => {
                println!("FAILED");
                eprintln!("    {e:#}");
                total_errors += 1;
            }
        }
    }

    println!("\nDone: {total_rows} rows migrated, {total_errors} errors");

    if total_errors > 0 {
        bail!("{total_errors} errors during migration");
    }

    Ok(())
}

async fn migrate_table(
    pg: &tokio_postgres::Client,
    scylla: &Session,
    kv_table: &str,
    keyspace: &str,
    table: &Table,
    udts: &HashMap<&str, &UserType>,
    dry_run: bool,
) -> Result<(usize, usize)> {
    let col_names: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
    let placeholders = col_names.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let insert_cql = format!(
        "INSERT INTO {keyspace}.{} ({}) VALUES ({placeholders})",
        table.name,
        col_names.join(", "),
    );

    let insert_ttl_cql = format!("{insert_cql} USING TTL ?");

    let prepared = if !dry_run {
        let plain = scylla
            .prepare(insert_cql.as_str())
            .await
            .with_context(|| format!("failed to prepare INSERT for {}", table.name))?;
        let with_ttl = scylla
            .prepare(insert_ttl_cql.as_str())
            .await
            .with_context(|| {
                format!("failed to prepare INSERT ... USING TTL for {}", table.name)
            })?;
        Some((plain, with_ttl))
    } else {
        None
    };

    // Carry the remaining lifetime of expiring KV rows over as a CQL TTL so
    // short-lived rows (sessions, tickets, default-TTL tables) still expire on
    // schedule. The KV executor stores 'infinity' for `USING TTL 0` on a table
    // with a default_time_to_live, so it becomes `USING TTL 0` again; a plain
    // insert would give the row the table default. Rows with no expiry (NULL)
    // are inserted plainly and pick up the table's default_time_to_live, if any.
    let query = format!(
        "SELECT row_data, \
         CASE WHEN expires_at IS NULL THEN NULL \
         WHEN expires_at = 'infinity'::timestamptz THEN 0 \
         ELSE GREATEST(1, CEIL(EXTRACT(EPOCH FROM (expires_at - NOW()))))::int END AS ttl_secs \
         FROM {kv_table} WHERE table_name = $1 \
         AND (expires_at IS NULL OR expires_at > NOW())"
    );

    let rows = pg
        .query(query.as_str(), &[&table.name])
        .await
        .with_context(|| format!("failed to query Postgres KV for {}", table.name))?;

    let mut migrated = 0usize;
    let mut errors = 0usize;

    for pg_row in &rows {
        let raw: Value = pg_row.get(0);
        let ttl_secs: Option<i32> = pg_row.get(1);

        let decoded = match decode_kv_json(raw) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("\n    decode error in {}: {e:#}", table.name);
                errors += 1;
                continue;
            }
        };

        let Value::Object(mut obj) = decoded else {
            eprintln!("\n    row_data not an object in {}", table.name);
            errors += 1;
            continue;
        };
        timeuuids_from_millis(&mut obj, table);

        let mut values: Vec<Option<CqlValue>> = Vec::with_capacity(table.columns.len());
        let mut conversion_error = false;

        for col in &table.columns {
            let json_val = obj.get(&col.name).cloned().unwrap_or(Value::Null);
            match json_to_cql_value(&json_val, &col.field_type, keyspace, udts) {
                Ok(cql) => values.push(cql),
                Err(e) => {
                    eprintln!(
                        "\n    conversion error in {}.{} ({}): {e:#}",
                        table.name, col.name, col.field_type
                    );
                    conversion_error = true;
                    break;
                }
            }
        }

        if conversion_error {
            errors += 1;
            continue;
        }

        if let Some((plain, with_ttl)) = &prepared {
            let result = match ttl_secs {
                Some(ttl) => {
                    values.push(Some(CqlValue::Int(ttl)));
                    scylla.execute_unpaged(with_ttl, values).await
                }
                None => scylla.execute_unpaged(plain, values).await,
            };
            if let Err(e) = result {
                eprintln!("\n    insert error in {}: {e:#}", table.name);
                errors += 1;
                continue;
            }
        }

        migrated += 1;
    }

    Ok((migrated, errors))
}

// Mirrors postgres::decode_row_dates_as_millis from fluxer_svc.
// Strips __fluxer_type wrappers and converts dates to ms-since-epoch integers.
fn decode_kv_json(value: Value) -> Result<Value> {
    decode_value(value)
}

fn decode_value(value: Value) -> Result<Value> {
    match value {
        Value::Array(items) => items
            .into_iter()
            .map(decode_value)
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(mut obj) => {
            match obj.get("__fluxer_type").and_then(Value::as_str) {
                Some("bigint") => {
                    let s = obj
                        .remove("value")
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    Ok(s.parse::<i64>()
                        .map(|n| Value::Number(n.into()))
                        .unwrap_or(Value::String(s)))
                }
                Some("date") => {
                    // ISO8601 timestamp → milliseconds since epoch
                    let s = obj
                        .remove("value")
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    Ok(chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|dt| Value::Number(dt.timestamp_millis().into()))
                        .unwrap_or(Value::String(s)))
                }
                Some("buffer" | "local_date") => Ok(obj.remove("value").unwrap_or(Value::Null)),
                Some("set") => match obj.remove("value").unwrap_or(Value::Null) {
                    Value::Array(items) => items
                        .into_iter()
                        .map(decode_value)
                        .collect::<Result<Vec<_>>>()
                        .map(Value::Array),
                    _ => Ok(Value::Array(Vec::new())),
                },
                Some("map") => match obj.remove("value").unwrap_or(Value::Null) {
                    Value::Array(entries) => entries
                        .into_iter()
                        .map(decode_value)
                        .collect::<Result<Vec<_>>>()
                        .map(Value::Array),
                    _ => Ok(Value::Array(Vec::new())),
                },
                _ => obj
                    .into_iter()
                    .map(|(k, v)| decode_value(v).map(|v| (k, v)))
                    .collect::<Result<Map<_, _>>>()
                    .map(Value::Object),
            }
        }
        other => Ok(other),
    }
}

// Columns the API fills via `insertWithNow` (CQL `now()`) are timeuuids in
// ScyllaDB, but the Postgres KV executor stores them as a plain date, which
// decodes to milliseconds since the epoch. Turn those into a version-1
// timeuuid carrying the same timestamp so clustering order is preserved. The
// clock-sequence/node bits come from a hash of the row so re-running the
// migration is idempotent and rows sharing a millisecond do not collide.
fn timeuuids_from_millis(obj: &mut Map<String, Value>, table: &Table) {
    let millis_columns: Vec<&str> = table
        .columns
        .iter()
        .filter(|col| col.field_type.eq_ignore_ascii_case("timeuuid"))
        .filter(|col| obj.get(&col.name).is_some_and(Value::is_i64))
        .map(|col| col.name.as_str())
        .collect();
    if millis_columns.is_empty() {
        return;
    }
    let row_hash = Sha256::digest(Value::Object(obj.clone()).to_string().as_bytes());
    for name in millis_columns {
        if let Some(ms) = obj.get(name).and_then(Value::as_i64) {
            obj.insert(
                name.to_owned(),
                Value::String(timeuuid_from_millis(ms, &row_hash).to_string()),
            );
        }
    }
}

fn timeuuid_from_millis(ms: i64, seed: &[u8]) -> Uuid {
    // 100ns intervals between 1582-10-15 (UUID epoch) and 1970-01-01.
    const GREGORIAN_OFFSET: u64 = 0x01B2_1DD2_1381_4000;
    let ticks = (ms as u64)
        .wrapping_mul(10_000)
        .wrapping_add(GREGORIAN_OFFSET);
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&((ticks & 0xFFFF_FFFF) as u32).to_be_bytes());
    bytes[4..6].copy_from_slice(&(((ticks >> 32) & 0xFFFF) as u16).to_be_bytes());
    bytes[6..8].copy_from_slice(&((((ticks >> 48) & 0x0FFF) as u16) | 0x1000).to_be_bytes());
    bytes[8..16].copy_from_slice(&seed[0..8]);
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // RFC 4122 variant
    Uuid::from_bytes(bytes)
}

fn json_to_cql_value(
    value: &Value,
    type_str: &str,
    keyspace: &str,
    udts: &HashMap<&str, &UserType>,
) -> Result<Option<CqlValue>> {
    if value.is_null() {
        return Ok(None);
    }

    let t = type_str.replace(' ', "").to_ascii_lowercase();
    let t = t.as_str();

    // frozen<X> is serialized identically to X
    if let Some(inner) = t.strip_prefix("frozen<").and_then(|s| s.strip_suffix('>')) {
        return json_to_cql_value(value, inner, keyspace, udts);
    }

    Ok(Some(match t {
        "bigint" => {
            let n = value
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("expected i64, got {value}"))?;
            CqlValue::BigInt(n)
        }
        "int" => {
            let n = value
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("expected int, got {value}"))?;
            CqlValue::Int(n as i32)
        }
        "tinyint" => {
            let n = value
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("expected tinyint, got {value}"))?;
            CqlValue::TinyInt(n as i8)
        }
        "smallint" => {
            let n = value
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("expected smallint, got {value}"))?;
            CqlValue::SmallInt(n as i16)
        }
        "text" | "ascii" | "varchar" => {
            let s = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("expected string, got {value}"))?;
            CqlValue::Text(s.to_owned())
        }
        "boolean" => {
            let b = value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("expected bool, got {value}"))?;
            CqlValue::Boolean(b)
        }
        "timestamp" => {
            let ms = value
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("expected timestamp ms i64, got {value}"))?;
            CqlValue::Timestamp(CqlTimestamp(ms))
        }
        "date" => {
            let s = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("expected date string, got {value}"))?;
            let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .with_context(|| format!("invalid date: {s}"))?;
            let unix_epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
            let days = date.signed_duration_since(unix_epoch).num_days();
            // CqlDate(u32) = days since unix epoch + 2^31
            CqlValue::Date(CqlDate((days + (1i64 << 31)) as u32))
        }
        "uuid" => {
            let s = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("expected UUID string, got {value}"))?;
            let uuid = Uuid::parse_str(s).with_context(|| format!("invalid UUID: {s}"))?;
            CqlValue::Uuid(uuid)
        }
        "timeuuid" => {
            let s = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("expected timeuuid string, got {value}"))?;
            let uuid: CqlTimeuuid = s
                .parse()
                .with_context(|| format!("invalid timeuuid: {s}"))?;
            CqlValue::Timeuuid(uuid)
        }
        "double" => {
            let f = value
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("expected double, got {value}"))?;
            CqlValue::Double(f)
        }
        "float" => {
            let f = value
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("expected float, got {value}"))?;
            CqlValue::Float(f as f32)
        }
        "blob" => {
            let s = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("expected base64 string, got {value}"))?;
            let bytes = BASE64_STANDARD.decode(s).context("invalid base64")?;
            CqlValue::Blob(bytes)
        }
        "varint" => {
            // Stored as a JSON integer; convert to big-endian signed bytes
            let n = value
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("expected varint integer, got {value}"))?;
            CqlValue::Varint(CqlVarint::from_signed_bytes_be(n.to_be_bytes().to_vec()))
        }
        _ if t.starts_with("list<") => {
            let inner = &t[5..t.len() - 1];
            let arr = value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("expected array for list<{inner}>, got {value}"))?;
            let items = arr
                .iter()
                .filter_map(|v| json_to_cql_value(v, inner, keyspace, udts).transpose())
                .collect::<Result<Vec<_>>>()?;
            CqlValue::List(items)
        }
        _ if t.starts_with("set<") => {
            let inner = &t[4..t.len() - 1];
            let arr = value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("expected array for set<{inner}>, got {value}"))?;
            let items = arr
                .iter()
                .filter_map(|v| json_to_cql_value(v, inner, keyspace, udts).transpose())
                .collect::<Result<Vec<_>>>()?;
            CqlValue::Set(items)
        }
        _ if t.starts_with("map<") => {
            let inner = &t[4..t.len() - 1];
            let parts = split_top_level(inner);
            if parts.len() != 2 {
                bail!("malformed map type: {t}");
            }
            let (key_type, val_type) = (parts[0], parts[1]);
            let arr = value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("expected array for map<>, got {value}"))?;
            let mut map = Vec::with_capacity(arr.len());
            for entry in arr {
                let pair = entry
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("map entry is not [key, val]: {entry}"))?;
                if pair.len() != 2 {
                    bail!("map entry has {} elements, expected 2", pair.len());
                }
                let k = json_to_cql_value(&pair[0], key_type, keyspace, udts)?
                    .ok_or_else(|| anyhow::anyhow!("null map key"))?;
                let v = json_to_cql_value(&pair[1], val_type, keyspace, udts)?
                    .ok_or_else(|| anyhow::anyhow!("null map value"))?;
                map.push((k, v));
            }
            CqlValue::Map(map)
        }
        _ if t.starts_with("tuple<") => {
            let inner = &t[6..t.len() - 1];
            let types = split_top_level(inner);
            let arr = value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("expected array for tuple<>, got {value}"))?;
            let items = types
                .iter()
                .enumerate()
                .map(|(i, ty)| {
                    let v = arr.get(i).unwrap_or(&Value::Null);
                    json_to_cql_value(v, ty, keyspace, udts)
                })
                .collect::<Result<Vec<_>>>()?;
            CqlValue::Tuple(items)
        }
        _ => {
            // UDT — look up by type name
            let udt = udts
                .get(t)
                .ok_or_else(|| anyhow::anyhow!("unknown CQL type: {t}"))?;
            let obj = value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("expected object for UDT {t}, got {value}"))?;
            // Fields must be in UDT definition order for ScyllaDB serialization
            let fields = udt
                .fields
                .iter()
                .map(|f| {
                    let v = obj.get(&f.name).cloned().unwrap_or(Value::Null);
                    let cql = json_to_cql_value(&v, &f.field_type, keyspace, udts)?;
                    Ok((f.name.clone(), cql))
                })
                .collect::<Result<Vec<_>>>()?;
            CqlValue::UserDefinedType {
                keyspace: keyspace.to_owned(),
                name: udt.name.clone(),
                fields,
            }
        }
    }))
}

// Split a comma-separated type list at top-level commas (skipping those inside angle brackets).
fn split_top_level(s: &str) -> Vec<&str> {
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(s[start..].trim());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeuuid_from_millis_keeps_timestamp() {
        let ms = 1_787_875_200_123_i64;
        let uuid = timeuuid_from_millis(ms, &[0xAB; 32]);
        assert_eq!(uuid.get_version_num(), 1);
        let (secs, nanos) = uuid.get_timestamp().expect("v1 uuid").to_unix();
        assert_eq!(secs as i64 * 1000 + i64::from(nanos / 1_000_000), ms);
        assert!(uuid.to_string().parse::<CqlTimeuuid>().is_ok());
    }

    #[test]
    fn parse_bool_rejects_unknown_values() {
        assert!(parse_bool("X", "TRUE").unwrap());
        assert!(parse_bool("X", " yes ").unwrap());
        assert!(!parse_bool("X", "false").unwrap());
        assert!(parse_bool("X", "ture").is_err());
    }

    #[test]
    fn decodes_kv_wrappers_like_fluxer_svc() {
        let raw = serde_json::json!({
            "id": {"__fluxer_type": "bigint", "value": "42"},
            "at": {"__fluxer_type": "date", "value": "2026-08-28T00:00:00.000Z"},
            "tags": {"__fluxer_type": "set", "value": ["a"]},
        });
        let decoded = decode_kv_json(raw).unwrap();
        assert_eq!(decoded["id"], 42);
        assert_eq!(decoded["at"], 1_787_875_200_000_i64);
        assert_eq!(decoded["tags"], serde_json::json!(["a"]));
    }
}
