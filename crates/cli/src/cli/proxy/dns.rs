//! Talking to the proxy's DNS server directly, bypassing the system resolver.
//!
//! Two callers: `proxy up` waits for the server to answer at all, and `proxy
//! status` asks it what a given hostname resolves to. Both want to know what
//! the proxy itself thinks, which is exactly what the system resolver hides —
//! so this speaks the wire format rather than calling `getaddrinfo`.
//!
//! Only the sliver of DNS we need is implemented: one question, A or AAAA, no
//! EDNS.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use eyre::{Result, WrapErr};
use tokio::net::UdpSocket;

/// Host address the proxy's DNS port is published on.
pub(crate) const LISTEN_IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Name used to check that the proxy is answering; it resolves to nothing.
pub(crate) const PROBE_NAME: &str = "readiness-probe.devconcurrent.test";

pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(100);

const HEADER_LEN: usize = 12;
const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;

/// Which address family to ask about. The proxy answers a name with one
/// family only (whatever its container's IP is) and returns an empty answer
/// for the other, so asking for the wrong one looks like "no record".
#[derive(Debug, Clone, Copy)]
pub(crate) enum Family {
    V4,
    V6,
}

impl Family {
    fn of(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(_) => Family::V4,
            IpAddr::V6(_) => Family::V6,
        }
    }

    fn qtype(self) -> u16 {
        match self {
            Family::V4 => TYPE_A,
            Family::V6 => TYPE_AAAA,
        }
    }
}

/// What the proxy said about a name.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The proxy knows the name and returned this address.
    Address(IpAddr),
    /// The proxy answered, but with no address (`NXDOMAIN`, or a name it knows
    /// with no record of the family we asked for).
    Unknown,
}

/// Send one query and wait for the matching reply.
///
/// Errors mean "no usable answer arrived": the proxy is down, the port isn't
/// published, or the packet was malformed. A name the proxy doesn't know is
/// not an error — it's [`Answer::Unknown`].
pub(crate) async fn query(
    port: u16,
    name: &str,
    family: Family,
    timeout: Duration,
) -> Result<Answer> {
    let socket = UdpSocket::bind((LISTEN_IP, 0))
        .await
        .wrap_err("bind dns probe socket")?;
    socket
        .connect(SocketAddr::new(LISTEN_IP, port))
        .await
        .wrap_err("connect dns probe socket")?;

    let id = rand::random();
    let query = build_query(id, name, family);
    socket.send(&query).await.wrap_err("send dns query")?;

    let mut buf = [0u8; 512];
    let len = tokio::time::timeout(timeout, socket.recv(&mut buf))
        .await
        .map_err(|_| eyre::eyre!("no reply within {timeout:?}"))?
        .wrap_err("receive dns reply")?;

    parse_answer(&buf[..len], id)
}

/// True if the proxy answers a query at all. Used as a liveness signal, where
/// the content of the answer doesn't matter — an `NXDOMAIN` proves just as
/// much as an address.
pub(crate) async fn is_answering(port: u16) -> bool {
    query(port, PROBE_NAME, Family::V4, PROBE_TIMEOUT)
        .await
        .is_ok()
}

/// Ask about the family `expected` belongs to, so a v6-only container doesn't
/// look unregistered.
pub(crate) async fn query_for(
    port: u16,
    name: &str,
    expected: IpAddr,
    timeout: Duration,
) -> Result<Answer> {
    query(port, name, Family::of(expected), timeout).await
}

/// A minimal `<name> <family> IN` query packet. The name need not exist; an
/// NXDOMAIN is just as good an answer.
fn build_query(id: u16, name: &str, family: Family) -> Vec<u8> {
    let mut query = Vec::new();
    query.extend_from_slice(&id.to_be_bytes());
    query.extend_from_slice(&[0x01, 0x00]); // standard query, recursion desired
    query.extend_from_slice(&[0x00, 0x01]); // one question
    query.extend_from_slice(&[0x00; 6]); // no answer, authority, or additional records
    for label in name.split('.') {
        // Labels are length-prefixed with a single byte, so anything longer is
        // unrepresentable. Hostnames come from user templates, so this is a
        // real possibility rather than an invariant.
        let len = u8::try_from(label.len()).unwrap_or(u8::MAX).min(63);
        query.push(len);
        query.extend_from_slice(&label.as_bytes()[..len as usize]);
    }
    query.push(0); // root label
    query.extend_from_slice(&family.qtype().to_be_bytes());
    query.extend_from_slice(&[0x00, 0x01]); // qclass: IN
    query
}

/// Pull the first address out of a response's answer section.
fn parse_answer(packet: &[u8], id: u16) -> Result<Answer> {
    if packet.len() < HEADER_LEN {
        eyre::bail!("dns reply is too short to be a response");
    }
    if packet[..2] != id.to_be_bytes() {
        eyre::bail!("dns reply is for a different query");
    }

    let questions = u16::from_be_bytes([packet[4], packet[5]]);
    let answers = u16::from_be_bytes([packet[6], packet[7]]);

    let mut at = HEADER_LEN;
    for _ in 0..questions {
        at = skip_name(packet, at)?;
        // qtype + qclass
        at = at
            .checked_add(4)
            .filter(|end| *end <= packet.len())
            .ok_or_else(|| eyre::eyre!("dns reply is truncated in the question section"))?;
    }

    for _ in 0..answers {
        at = skip_name(packet, at)?;
        let header = packet
            .get(at..at + 10)
            .ok_or_else(|| eyre::eyre!("dns reply is truncated in the answer section"))?;
        let kind = u16::from_be_bytes([header[0], header[1]]);
        let rdlen = usize::from(u16::from_be_bytes([header[8], header[9]]));
        at += 10;
        let rdata = packet
            .get(at..at + rdlen)
            .ok_or_else(|| eyre::eyre!("dns reply has a truncated record"))?;
        at += rdlen;

        match (kind, rdata.len()) {
            (TYPE_A, 4) => {
                let octets: [u8; 4] = rdata.try_into().expect("just checked the length");
                return Ok(Answer::Address(IpAddr::from(octets)));
            }
            (TYPE_AAAA, 16) => {
                let octets: [u8; 16] = rdata.try_into().expect("just checked the length");
                return Ok(Answer::Address(IpAddr::from(octets)));
            }
            _ => {}
        }
    }

    Ok(Answer::Unknown)
}

/// Advance past a name, which is either a sequence of length-prefixed labels
/// or a two-byte pointer into an earlier one.
fn skip_name(packet: &[u8], mut at: usize) -> Result<usize> {
    loop {
        let len = *packet
            .get(at)
            .ok_or_else(|| eyre::eyre!("dns reply is truncated in a name"))?;
        if len & 0xc0 == 0xc0 {
            // A compression pointer always ends the name.
            return Ok(at + 2);
        }
        at += 1 + usize::from(len);
        if len == 0 {
            return Ok(at);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use super::*;

    #[test]
    fn query_is_a_well_formed_question() {
        let query = build_query(0xbeef, "foo.test", Family::V4);
        assert_eq!(
            query,
            [
                0xbe, 0xef, // id
                0x01, 0x00, // flags
                0x00, 0x01, // qdcount
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // an/ns/ar count
                3, b'f', b'o', b'o', 4, b't', b'e', b's', b't', 0, // qname
                0x00, 0x01, // qtype: A
                0x00, 0x01, // qclass: IN
            ]
        );
    }

    #[test]
    fn probe_name_labels_fit_in_a_length_byte() {
        build_query(0, PROBE_NAME, Family::V4);
    }

    #[test]
    fn over_long_labels_are_truncated_rather_than_panicking() {
        let name = format!("{}.test", "a".repeat(300));
        let query = build_query(0, &name, Family::V4);
        assert_eq!(query[HEADER_LEN], 63);
    }

    #[test]
    fn family_follows_the_expected_address() {
        let v6 = build_query(0, "foo.test", Family::of(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert_eq!(v6[v6.len() - 4..v6.len() - 2], TYPE_AAAA.to_be_bytes());
        let v4 = build_query(0, "foo.test", Family::of(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert_eq!(v4[v4.len() - 4..v4.len() - 2], TYPE_A.to_be_bytes());
    }

    /// A response to `build_query(id, "foo.test")` carrying `records`, with the
    /// answer name given as a compression pointer, the way a real server does.
    fn response(id: u16, answers: u16, records: &[u8]) -> Vec<u8> {
        let mut packet = build_query(id, "foo.test", Family::V4);
        packet[2] = 0x81; // response, recursion desired
        packet[3] = 0x80; // recursion available, rcode 0
        packet[6..8].copy_from_slice(&answers.to_be_bytes());
        packet.extend_from_slice(records);
        packet
    }

    fn record(kind: u16, rdata: &[u8]) -> Vec<u8> {
        let mut out = vec![0xc0, 0x0c]; // pointer to the question's name
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&[0x00, 0x01]); // class IN
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]); // ttl
        out.extend_from_slice(
            &u16::try_from(rdata.len())
                .expect("test rdata is short")
                .to_be_bytes(),
        );
        out.extend_from_slice(rdata);
        out
    }

    #[test]
    fn reads_an_a_record() {
        let packet = response(0x1234, 1, &record(TYPE_A, &[172, 18, 0, 4]));
        assert_eq!(
            parse_answer(&packet, 0x1234).unwrap(),
            Answer::Address(IpAddr::V4(Ipv4Addr::new(172, 18, 0, 4))),
        );
    }

    #[test]
    fn reads_an_aaaa_record() {
        let addr = Ipv6Addr::LOCALHOST;
        let packet = response(0x1234, 1, &record(TYPE_AAAA, &addr.octets()));
        assert_eq!(
            parse_answer(&packet, 0x1234).unwrap(),
            Answer::Address(IpAddr::V6(addr)),
        );
    }

    #[test]
    fn skips_records_of_other_types() {
        let mut records = record(15, &[0x00, 0x0a, 0x00]); // MX
        records.extend_from_slice(&record(TYPE_A, &[10, 0, 0, 1]));
        let packet = response(0x1234, 2, &records);
        assert_eq!(
            parse_answer(&packet, 0x1234).unwrap(),
            Answer::Address(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
        );
    }

    #[test]
    fn no_answers_is_unknown_not_an_error() {
        let packet = response(0x1234, 0, &[]);
        assert_eq!(parse_answer(&packet, 0x1234).unwrap(), Answer::Unknown);
    }

    #[test]
    fn rejects_a_reply_to_another_query() {
        let packet = response(0x1234, 0, &[]);
        assert!(parse_answer(&packet, 0x4321).is_err());
    }

    #[test]
    fn rejects_a_truncated_record() {
        let mut packet = response(0x1234, 1, &record(TYPE_A, &[172, 18, 0, 4]));
        packet.truncate(packet.len() - 2);
        assert!(parse_answer(&packet, 0x1234).is_err());
    }

    #[test]
    fn handles_an_uncompressed_answer_name() {
        let mut records = Vec::new();
        for label in ["foo", "test"] {
            records.push(u8::try_from(label.len()).unwrap());
            records.extend_from_slice(label.as_bytes());
        }
        records.push(0);
        records.extend_from_slice(&TYPE_A.to_be_bytes());
        records.extend_from_slice(&[0x00, 0x01]);
        records.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
        records.extend_from_slice(&[0x00, 0x04]);
        records.extend_from_slice(&[192, 168, 1, 1]);

        let packet = response(0x1234, 1, &records);
        assert_eq!(
            parse_answer(&packet, 0x1234).unwrap(),
            Answer::Address(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
        );
    }
}
