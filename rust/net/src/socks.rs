//! The SOCKS5 CONNECT handshake (RFC 1928, RFC 1929) as Go's dialer speaks it.
use crate::ConnectError;
use graphite_meter_proto::origin::Host;
use std::net::IpAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

type Login = (Vec<u8>, Vec<u8>);

/// Asks the proxy on `stream` to connect to `host`, offering username and password only with a login.
pub(crate) async fn connect(
    stream: &mut TcpStream,
    login: Option<&Login>,
    host: &Host,
    port: u16,
) -> Result<(), ConnectError> {
    let greeting: &[u8] = if login.is_some() { &[5, 2, 0, 2] } else { &[5, 1, 0] };
    stream.write_all(greeting).await?;
    let mut reply = [0; 2];
    stream.read_exact(&mut reply).await?;
    if reply[0] != 5 {
        return Err(failed(format!("unexpected protocol version {}", reply[0])));
    }
    match (reply[1], login) {
        (0, _) => {}
        (0xff, _) => return Err(failed("no acceptable authentication methods")),
        (2, Some(login)) => authenticate(stream, login).await?,
        (method, _) => return Err(failed(format!("unsupported authentication method {method}"))),
    }
    let address = match host {
        Host::Ip(ip) => match ip.to_canonical() {
            IpAddr::V4(ip) => [&[1][..], &ip.octets()].concat(),
            IpAddr::V6(ip) => [&[4][..], &ip.octets()].concat(),
        },
        Host::Name(name) => {
            let length = u8::try_from(name.len()).map_err(|_| failed("FQDN too long"))?;
            [&[3, length][..], name.as_bytes()].concat()
        }
    };
    stream
        .write_all(&[&[5, 1, 0][..], &address, &port.to_be_bytes()].concat())
        .await?;
    let mut head = [0; 4];
    stream.read_exact(&mut head).await?;
    match head {
        [5, 0, 0, _] => {}
        [5, 0, ..] => return Err(failed("non-zero reserved field")),
        [5, code, ..] => return Err(failed(format!("unknown error {}", reply_text(code)))),
        [version, ..] => return Err(failed(format!("unexpected protocol version {version}"))),
    }
    let bound = match head[3] {
        1 => 4,
        4 => 16,
        3 => usize::from(stream.read_u8().await?),
        other => return Err(failed(format!("unknown address type {other}"))),
    };
    stream.read_exact(&mut vec![0; bound + 2]).await?;
    Ok(())
}

async fn authenticate(stream: &mut TcpStream, (user, password): &Login) -> Result<(), ConnectError> {
    let (Ok(user_length @ 1..), Ok(password_length)) = (u8::try_from(user.len()), u8::try_from(password.len())) else {
        return Err(failed("invalid username/password"));
    };
    stream
        .write_all(&[&[1, user_length][..], user, &[password_length], password].concat())
        .await?;
    let mut reply = [0; 2];
    stream.read_exact(&mut reply).await?;
    match reply {
        [1, 0] => Ok(()),
        [1, _] => Err(failed("username/password authentication failed")),
        _ => Err(failed("invalid username/password version")),
    }
}

fn failed(reason: impl std::fmt::Display) -> ConnectError {
    ConnectError::Refused(format!("socks connect: {reason}"))
}

fn reply_text(code: u8) -> String {
    let text = match code {
        1 => "general SOCKS server failure",
        2 => "connection not allowed by ruleset",
        3 => "network unreachable",
        4 => "host unreachable",
        5 => "connection refused",
        6 => "TTL expired",
        7 => "command not supported",
        8 => "address type not supported",
        code => return format!("unknown code: {code}"),
    };
    text.to_owned()
}
