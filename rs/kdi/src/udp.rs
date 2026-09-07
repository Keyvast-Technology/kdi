//! The UDP reference binding (`bindings.udp`, kdi/contract.yaml:145-153). Its peer is the reference
//! software device model — not shipped here, but documented because it is what every non-Python
//! implementation tests against with no hardware. Names are the wire: no address resolution at all.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use serde_json::{json, Value};

use crate::{io_err, reply_from, Arg, Error, Reply};
use std::io;

/// One request datagram -> one reply datagram, max 65535 B (`bindings.udp.rpc.framing`).
const MAX_DGRAM: usize = 65535;

/// Envelope `args` is an OBJECT KEYED BY NAME; only the vUART line is positional. As a JSON
/// ARRAY it validates with NO args, so every argumented command answered `bad_args`/`missing`.
/// An EMPTY name means there are none to key by, so the array is what the device must judge.
fn named(args: &[(&str, Arg<'_>)]) -> Value {
    if args.iter().any(|(k, _)| k.is_empty()) {
        return Value::Array(args.iter().map(|(_, v)| v.json()).collect());
    }
    // Fewer arguments than declared is legal and ordinary: a trailing optional is simply absent.
    Value::Object(
        args.iter()
            .map(|(k, v)| ((*k).to_string(), v.json()))
            .collect(),
    )
}

pub(crate) struct Udp {
    sock: UdpSocket,
    rx: Vec<u8>,
}

impl Udp {
    pub(crate) fn connect(peer: SocketAddr, timeout: Duration) -> Result<Udp, Error> {
        let sock = UdpSocket::bind(bind_any(peer)).map_err(Error::Io)?;
        sock.set_read_timeout(Some(timeout)).map_err(Error::Io)?;
        // `connect` so the kernel drops datagrams from anyone else: this socket is a point-to-point
        // link to one device, and an unrelated sender must not be able to answer for it.
        sock.connect(peer).map_err(Error::Io)?;
        Ok(Udp {
            sock,
            rx: vec![0u8; MAX_DGRAM],
        })
    }

    /// Bytes of the reply datagram, borrowed from the receive buffer.
    fn rpc(&mut self, req: &Value) -> Result<usize, Error> {
        let out = serde_json::to_vec(req)
            .map_err(|e| io_err(io::ErrorKind::InvalidData, e.to_string()))?;
        self.sock.send(&out).map_err(Error::Io)?;
        match self.sock.recv(&mut self.rx) {
            Ok(n) => Ok(n),
            // One rule, one definition. `io_or_timeout` maps a read deadline to `host_timeout` —
            // the closed set's token, never a minted one (`contract.yaml:36-37`) — and everything
            // else to `Io`. `simlink.rs` had its own copy of exactly this.
            Err(e) => Err(crate::io_or_timeout(e)),
        }
    }

    fn rpc_json(&mut self, req: &Value) -> Result<Value, Error> {
        let n = self.rpc(req)?;
        let v: Value = serde_json::from_slice(&self.rx[..n]).map_err(|e| {
            io_err(
                io::ErrorKind::InvalidData,
                format!("undecodable reply: {e}"),
            )
        })?;
        // ANY op may answer `{"err": token}` (kdi/contract.yaml:153-153). These register-drawer
        // tokens — `no_such_register`, `ro_register` — are host bugs, not device data: no `rc`, no
        // id, nothing to act on, so they surface as an error rather than as an `Ok`.
        if let Some(t) = v.get("err").and_then(Value::as_str) {
            return Err(io_err(
                io::ErrorKind::InvalidData,
                format!("device refused {}: {t}", req["op"]),
            ));
        }
        Ok(v)
    }

    pub(crate) fn discover(&mut self) -> Result<Value, Error> {
        self.rpc_json(&json!({"op": "discover"}))
    }

    pub(crate) fn reg_read(&mut self, name: &str) -> Result<u32, Error> {
        let v = self.rpc_json(&json!({"op": "reg_read", "name": name}))?;
        v.get("value")
            .and_then(Value::as_u64)
            .map(|x| x as u32)
            .ok_or_else(|| {
                io_err(
                    io::ErrorKind::InvalidData,
                    format!("reg_read {name}: reply carries no integer `value`"),
                )
            })
    }

    pub(crate) fn reg_write(&mut self, name: &str, value: u32) -> Result<(), Error> {
        self.rpc_json(&json!({"op": "reg_write", "name": name, "value": value}))?;
        Ok(())
    }

    pub(crate) fn stream_read(&mut self, name: &str, buf: &mut [u8]) -> Result<usize, Error> {
        // One datagram caps one reply, so ask for no more than can come back. The host's rule is
        // the same on every binding: read, decode what arrived, read again if you wanted more.
        let want = buf.len().min(MAX_DGRAM - 1);
        if want == 0 {
            return Ok(0);
        }
        let n = self.rpc(&json!({"op": "stream_read", "name": name, "max_bytes": want}))?;
        let data = &self.rx[..n];
        // 0x01 then raw bytes; an error comes back as JSON, which cannot start with 0x01
        // (`bindings.udp.rpc.errors`).
        match data.first() {
            Some(1) => {
                let body = &data[1..];
                if body.len() > buf.len() {
                    return Err(io_err(
                        io::ErrorKind::InvalidData,
                        format!(
                            "stream_read returned {} B for a {want} B request",
                            body.len()
                        ),
                    ));
                }
                buf[..body.len()].copy_from_slice(body);
                Ok(body.len())
            }
            _ => {
                let token = serde_json::from_slice::<Value>(data)
                    .ok()
                    .and_then(|v| v.get("err").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_else(|| "unframed stream reply".to_string());
                Err(io_err(
                    io::ErrorKind::InvalidData,
                    format!("stream_read {name}: {token}"),
                ))
            }
        }
    }

    /// The request envelope `{id, name, args}` (kdi/contract.yaml:108-108); `args` goes by NAME
    /// here (see `named`). `token` is an ENVELOPE KEY, never inside `args` (:499-506): inside
    /// it is undeclared; omitted, every non-ro command answers `not_claimed`. Always sent.
    pub(crate) fn message(
        &mut self,
        id: &str,
        name: &str,
        args: &[(&str, Arg<'_>)],
        token: &str,
    ) -> Result<Reply, Error> {
        let v = self.rpc_json(&json!({
            "op": "message",
            "req": {"id": id, "name": name, "args": named(args), "token": token},
        }))?;
        let resp = v.get("resp").cloned().ok_or_else(|| {
            io_err(
                io::ErrorKind::InvalidData,
                format!("message {name}: reply carries no `resp`"),
            )
        })?;
        let reply = reply_from(resp)?;
        // Correlate, always. A late reply from a previously timed-out command must never be
        // returned as this command's answer (`response.rules`, kdi/contract.yaml:204-208) — on a
        // datagram wire that is a stray reply the kernel queued, not a theoretical case.
        if reply.id != id {
            return Err(io_err(
                io::ErrorKind::InvalidData,
                format!("reply id {:?} does not match request {id:?}", reply.id),
            ));
        }
        Ok(reply)
    }
}

/// A one-shot discover datagram: the probe that turns an announcement file into evidence of a
/// live device (the Python reference host).
pub(crate) fn probe(addr: SocketAddr, timeout: Duration) -> Result<Value, Error> {
    let mut u = Udp::connect(addr, timeout)?;
    u.discover()
}

fn bind_any(peer: SocketAddr) -> SocketAddr {
    // Match the peer's family, or a v4 server is unreachable from a v6 socket and vice versa.
    if peer.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0u16; 8], 0))
    }
}
