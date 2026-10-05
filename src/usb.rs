//! HTTP over the printer's LEDM USB interface (class ff/cc/00).
//!
//! This is what HPLIP's hpmud does for an "HP-LEDM" channel: claim the
//! interface and write plain HTTP/1.1 requests to its bulk OUT endpoint,
//! reading replies from bulk IN. No framing on top.

use std::io::{Read, Write};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use nusb::descriptors::TransferType;
use nusb::io::{EndpointRead, EndpointWrite};
use nusb::transfer::{Bulk, In, Out};
use nusb::{ErrorKind, MaybeFuture};

pub const VID: u16 = 0x03f0;
pub const PID: u16 = 0x0653;
const LEDM: (u8, u8, u8) = (0xff, 0xcc, 0x00);

pub struct Response {
    pub status: u16,
    pub body: String,
}

pub struct Printer {
    reader: EndpointRead<Bulk>,
    writer: EndpointWrite<Bulk>,
    _iface: nusb::Interface,
}

pub fn find() -> Option<nusb::DeviceInfo> {
    nusb::list_devices().wait().ok()?.find(|d| d.vendor_id() == VID && d.product_id() == PID)
}

/// Give this user read/write on the printer's USB node until it's unplugged.
fn grant_access(info: &nusb::DeviceInfo) -> Result<(), String> {
    let node = format!("/dev/bus/usb/{:03}/{:03}", info.busnum(), info.device_address());
    let user = std::env::var("USER").map_err(|_| "USER not set")?;
    eprintln!("Need USB access to the printer (sudo, until it's unplugged)");
    let ok = Command::new("sudo")
        .args(["setfacl", "-m", &format!("u:{user}:rw"), &node])
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if ok { Ok(()) } else { Err(format!("couldn't get access to {node}")) }
}

impl Printer {
    pub fn open() -> Result<Printer, String> {
        let info = find().ok_or("printer not found on USB — plug it in and switch it on")?;
        let ifnum = info
            .interfaces()
            .find(|i| (i.class(), i.subclass(), i.protocol()) == LEDM)
            .map(|i| i.interface_number())
            .ok_or("printer has no LEDM USB interface")?;

        let mut granted = false;
        let mut last = String::new();
        // Just after power-on or a replug the printer often answers "busy".
        for _ in 0..8 {
            let dev = match info.open().wait() {
                Ok(d) => d,
                Err(e) if e.kind() == ErrorKind::PermissionDenied && !granted => {
                    grant_access(&info)?;
                    granted = true;
                    continue;
                }
                Err(e) => { last = e.to_string(); sleep(Duration::from_secs(3)); continue; }
            };
            let (ep_in, ep_out) = endpoints(&dev, ifnum)?;
            match dev.detach_and_claim_interface(ifnum).wait() {
                Ok(iface) => {
                    let reader = iface
                        .endpoint::<Bulk, In>(ep_in)
                        .map_err(|e| e.to_string())?
                        .reader(16384)
                        .with_num_transfers(4);
                    let writer = iface
                        .endpoint::<Bulk, Out>(ep_out)
                        .map_err(|e| e.to_string())?
                        .writer(16384)
                        .with_write_timeout(Duration::from_secs(10));
                    return Ok(Printer { reader, writer, _iface: iface });
                }
                Err(e) => { last = e.to_string(); sleep(Duration::from_secs(3)); }
            }
        }
        Err(format!("printer didn't answer over USB ({last}) — is it still on? Replug and try again"))
    }

    /// Throw away anything left over from an earlier, interrupted exchange,
    /// so the next reply isn't read out of step (HPLIP's "flushThePort").
    fn drain(&mut self) {
        self.reader.set_read_timeout(Duration::from_millis(300));
        let mut buf = [0u8; 16384];
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            match self.reader.read(&mut buf) {
                Ok(n) if n > 0 => continue,
                _ => break,
            }
        }
    }

    fn exchange(&mut self, method: &str, path: &str, body: &str, timeout: Duration) -> Result<Response, String> {
        self.drain();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: deskjet\r\n\
             Content-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        self.writer.write_all(req.as_bytes()).map_err(|e| format!("USB write: {e}"))?;
        self.writer.flush_end().map_err(|e| format!("USB write: {e}"))?;
        self.read_response(timeout)
    }

    fn read_response(&mut self, timeout: Duration) -> Result<Response, String> {
        let deadline = Instant::now() + timeout;
        let mut data = Vec::new();
        let mut buf = [0u8; 16384];
        self.reader.set_read_timeout(Duration::from_millis(500));
        loop {
            if let Some(r) = parse_response(&data)? {
                return Ok(r);
            }
            if Instant::now() > deadline {
                return Err("printer took too long to answer".into());
            }
            match self.reader.read(&mut buf) {
                Ok(0) => return Err("USB channel closed".into()),
                Ok(n) => data.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("USB read: {e}")),
            }
        }
    }

    /// One request, retried with a clean channel when the printer garbles it.
    pub fn request(&mut self, method: &str, path: &str, body: &str, timeout: Duration) -> Result<Response, String> {
        let mut last = String::new();
        for attempt in 0..5 {
            if attempt > 0 { sleep(Duration::from_secs(2)); }
            match self.exchange(method, path, body, timeout) {
                Ok(r) => return Ok(r),
                Err(e) => last = e,
            }
        }
        Err(format!("{method} {path}: {last}"))
    }

    pub fn get(&mut self, path: &str) -> Result<Response, String> {
        self.request("GET", path, "", Duration::from_secs(30))
    }

    pub fn put(&mut self, path: &str, xml: &str) -> Result<Response, String> {
        self.request("PUT", path, xml, Duration::from_secs(60))
    }
}

fn endpoints(dev: &nusb::Device, ifnum: u8) -> Result<(u8, u8), String> {
    let cfg = dev.active_configuration().map_err(|e| e.to_string())?;
    let alt = cfg
        .interface_alt_settings()
        .find(|a| a.interface_number() == ifnum && a.alternate_setting() == 0)
        .ok_or("LEDM interface missing from USB descriptors")?;
    let bulk = |dir_in: bool| {
        alt.endpoints()
            .find(|e| e.transfer_type() == TransferType::Bulk && (e.address() & 0x80 != 0) == dir_in)
            .map(|e| e.address())
    };
    Ok((bulk(true).ok_or("no bulk IN endpoint")?, bulk(false).ok_or("no bulk OUT endpoint")?))
}

/// A complete HTTP response in `data`, or None if more bytes are needed.
/// Errors when what arrived can't be an HTTP response (out of step).
fn parse_response(data: &[u8]) -> Result<Option<Response>, String> {
    let Some(hdr_end) = find_bytes(data, b"\r\n\r\n") else {
        if data.len() > 8 && !data.starts_with(b"HTTP/") {
            return Err("garbled reply".into());
        }
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&data[..hdr_end]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .filter(|l| l.starts_with("HTTP/"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or("garbled reply")?;
    let mut length = None;
    let mut chunked = false;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "content-length" { length = v.parse::<usize>().ok(); }
        if k == "transfer-encoding" && v.eq_ignore_ascii_case("chunked") { chunked = true; }
    }
    let rest = &data[hdr_end + 4..];
    let body = if chunked {
        match dechunk(rest)? { Some(b) => b, None => return Ok(None) }
    } else {
        let n = length.unwrap_or(0);
        if rest.len() < n { return Ok(None); }
        rest[..n].to_vec()
    };
    Ok(Some(Response { status, body: String::from_utf8_lossy(&body).into_owned() }))
}

fn dechunk(mut data: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut out = Vec::new();
    loop {
        let Some(eol) = find_bytes(data, b"\r\n") else { return Ok(None) };
        let size_str = String::from_utf8_lossy(&data[..eol]);
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "garbled chunk")?;
        data = &data[eol + 2..];
        if size == 0 { return Ok(Some(out)); }
        if data.len() < size + 2 { return Ok(None); }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello").unwrap().unwrap();
        assert_eq!((r.status, r.body.as_str()), (200, "hello"));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhel").unwrap().is_none());
    }

    #[test]
    fn chunked() {
        let msg = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse_response(msg).unwrap().unwrap().body, "abcde");
        assert!(parse_response(&msg[..msg.len() - 6]).unwrap().is_none());
    }

    #[test]
    fn out_of_step() {
        assert!(parse_response(b"\">\r\n</io:Adapters>").is_err());
    }
}
