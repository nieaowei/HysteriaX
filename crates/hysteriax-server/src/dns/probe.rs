use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use hickory_proto::{
    op::{Message, MessageType, OpCode, Query},
    rr::{Name, RData, RecordType},
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    time::timeout,
};

use crate::db;

/// A DNS exchange validates source, transaction, question, truncation and authority.
async fn query(
    server: SocketAddr,
    name: &str,
    kind: RecordType,
    recursive: bool,
) -> Result<Message> {
    let mut request = Message::new();
    request
        .set_id(rand::random())
        .set_message_type(MessageType::Query)
        .set_op_code(OpCode::Query)
        .set_recursion_desired(recursive)
        .add_query(Query::query(Name::from_ascii(name)?, kind));
    let wire = request.to_vec()?;
    let socket = UdpSocket::bind(if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .await?;
    socket.connect(server).await?;
    socket.send(&wire).await?;
    let mut buffer = vec![0_u8; 65535];
    let size = timeout(Duration::from_secs(4), socket.recv(&mut buffer))
        .await
        .context("DNS query timed out")??;
    let mut response = Message::from_vec(&buffer[..size])?;
    if response.truncated() {
        let mut tcp = timeout(Duration::from_secs(4), TcpStream::connect(server)).await??;
        tcp.write_u16(wire.len() as u16).await?;
        tcp.write_all(&wire).await?;
        let size = timeout(Duration::from_secs(4), tcp.read_u16()).await?? as usize;
        buffer.resize(size, 0);
        timeout(Duration::from_secs(4), tcp.read_exact(&mut buffer)).await??;
        response = Message::from_vec(&buffer)?;
    }
    if response.id() != request.id()
        || response.message_type() != MessageType::Response
        || response.queries() != request.queries()
    {
        bail!("invalid DNS response");
    }
    if !recursive && !response.authoritative() {
        bail!("DNS response is not authoritative");
    }
    Ok(response)
}
fn resolvers() -> Vec<SocketAddr> {
    let data = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let mut result: Vec<_> = data
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != "nameserver" {
                return None;
            }
            fields
                .next()?
                .parse::<IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, 53))
        })
        .collect();
    if result.is_empty() {
        result.push("1.1.1.1:53".parse().unwrap());
    }
    result
}
async fn recursive(name: &str, kind: RecordType) -> Result<Message> {
    for resolver in resolvers() {
        if let Ok(response) = query(resolver, name, kind, true).await {
            return Ok(response);
        }
    }
    bail!("management DNS resolver is unavailable")
}
fn answers(message: &Message, kind: RecordType) -> Vec<String> {
    message
        .answers()
        .iter()
        .filter_map(|r| match (kind, r.data()?) {
            (RecordType::A, RData::A(value)) => Some(value.to_string()),
            (RecordType::AAAA, RData::AAAA(value)) => Some(value.to_string()),
            (RecordType::CNAME, RData::CNAME(value)) => {
                Some(value.to_string().trim_end_matches('.').to_ascii_lowercase())
            }
            (RecordType::NS, RData::NS(value)) => {
                Some(value.to_string().trim_end_matches('.').to_ascii_lowercase())
            }
            _ => None,
        })
        .collect()
}
async fn addresses(name: &str) -> Vec<String> {
    let mut values = Vec::new();
    for kind in [RecordType::A, RecordType::AAAA] {
        if let Ok(response) = recursive(name, kind).await {
            values.extend(answers(&response, kind));
        }
    }
    values
}
pub async fn check(pool: &PgPool, id: &str) -> Result<Value> {
    let row=sqlx::query("SELECT r.*,z.name AS zone_name FROM dns_records r JOIN dns_zones z ON z.id=r.zone_id WHERE r.id=$1").bind(id).fetch_one(pool).await?;
    let name: String = row.get("name");
    let expected: String = row.get("content");
    let kind = match row.get::<String, _>("record_type").as_str() {
        "A" => RecordType::A,
        "AAAA" => RecordType::AAAA,
        "CNAME" => RecordType::CNAME,
        _ => bail!("unsupported DNS record check"),
    };
    if row.get::<String, _>("state") != "synced" {
        bail!("DNS record must be written before checking resolution");
    }
    let management = recursive(&name, kind).await.map(|r| answers(&r, kind));
    // A flattened CNAME has address answers rather than a literal CNAME answer.
    let target_addresses = if kind == RecordType::CNAME {
        addresses(&expected).await
    } else {
        Vec::new()
    };
    let management_addresses = if kind == RecordType::CNAME {
        addresses(&name).await
    } else {
        Vec::new()
    };
    let zone_name: String = row.get("zone_name");
    let mut authoritative = Vec::<Value>::new();
    if let Ok(ns) = recursive(&zone_name, RecordType::NS).await {
        for host in answers(&ns, RecordType::NS).into_iter().take(4) {
            if let Ok(mut addresses) = tokio::net::lookup_host((host.as_str(), 53)).await
                && let Some(address) = addresses.next()
            {
                match query(address, &name, kind, false).await {
                    Ok(response) => {
                        let values = answers(&response, kind);
                        let mut flattened = Vec::new();
                        if kind == RecordType::CNAME && values.is_empty() {
                            for address_type in [RecordType::A, RecordType::AAAA] {
                                if let Ok(response) =
                                    query(address, &name, address_type, false).await
                                {
                                    flattened.extend(answers(&response, address_type));
                                }
                            }
                        }
                        authoritative.push(
                            json!({"server":host,"answers":values,"flattened_addresses":flattened}),
                        );
                    }
                    Err(_) => authoritative
                        .push(json!({"server":host,"error":"authority query unavailable"})),
                }
            }
        }
    }
    let expected = expected.trim_end_matches('.').to_ascii_lowercase();
    let local_matches = management
        .as_ref()
        .is_ok_and(|values| values.contains(&expected))
        || (!target_addresses.is_empty()
            && management_addresses
                .iter()
                .any(|ip| target_addresses.contains(ip)));
    let local_matches = local_matches
        && (kind != RecordType::CNAME
            || (!target_addresses.is_empty()
                && management_addresses
                    .iter()
                    .any(|ip| target_addresses.contains(ip))));
    let authoritative_matches = !authoritative.is_empty()
        && authoritative.iter().all(|v| {
            v["answers"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&expected)))
                || v["flattened_addresses"].as_array().is_some_and(|values| {
                    values.iter().any(|ip| {
                        ip.as_str()
                            .is_some_and(|ip| target_addresses.iter().any(|target| target == ip))
                    })
                })
        });
    let known_proxied_target = if kind == RecordType::CNAME {
        sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM dns_records WHERE name=$1 AND proxied AND deleted_at IS NULL)").bind(&expected).fetch_one(pool).await?
    } else {
        false
    };
    let status = if row.get::<bool, _>("proxied") || known_proxied_target {
        "proxied"
    } else if local_matches && authoritative_matches {
        "verified"
    } else {
        "pending"
    };
    let detail = json!({"record_id":id,"status":status,"expected":expected,"management_answers":management.ok(),"authoritative":authoritative,"target_addresses":target_addresses,"management_addresses":management_addresses});
    let mut tx = db::begin_write(pool).await?;
    // An in-flight result must never validate a newer edit.
    sqlx::query("UPDATE dns_records SET resolution_status=$1,resolution_detail=$2,checked_at=now() WHERE id=$3 AND revision=$4 AND state='synced'")
        .bind(status).bind(&detail).bind(id).bind(row.get::<i64,_>("revision")).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(detail)
}
