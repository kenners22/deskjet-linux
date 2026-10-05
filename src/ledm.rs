//! The printer's LEDM Wi-Fi API (the /IoMgmt resources hp-wificonfig uses).

use std::net::Ipv4Addr;
use std::thread::sleep;
use std::time::Duration;

use crate::usb::Printer;

const BASE: &str = "/IoMgmt/Adapters";

const NS: &str = r#"xmlns:io="http://www.hp.com/schemas/imaging/con/ledm/iomgmt/2008/11/30" xmlns:dd="http://www.hp.com/schemas/imaging/con/dictionaries/1.0/" xmlns:wifi="http://www.hp.com/schemas/imaging/con/wifi/2009/06/26""#;

pub struct Network {
    pub ssid: String,
    pub encryption: String,
    pub mode: String,
    pub strength: u8,
}

fn ok(status: u16) -> bool {
    status == 200 || status == 204
}

fn parse(xml: &str) -> Result<roxmltree::Document<'_>, String> {
    roxmltree::Document::parse(xml).map_err(|e| format!("printer sent bad XML: {e}"))
}

/// Text of the first descendant of `node` with this local name.
fn text<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.descendants().find(|n| n.tag_name().name() == name).and_then(|n| n.text()).map(str::trim)
}

fn unhex(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) { return None; }
    let bytes: Option<Vec<u8>> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect();
    String::from_utf8(bytes?).ok()
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

/// Names of the printer's built-in Wi-Fi adapters ("Wifi0", "Wifi1"…).
pub fn wifi_adapters(p: &mut Printer) -> Result<Vec<String>, String> {
    let r = p.get(BASE)?;
    if r.status != 200 { return Err(format!("adapter list: HTTP {}", r.status)); }
    let doc = parse(&r.body)?;
    Ok(doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Adapter")
        .filter(|a| {
            let t = text(*a, "DeviceConnectivityPortType").unwrap_or("").to_ascii_lowercase();
            t == "wifiembedded" || t == "wifiaccessory"
        })
        .filter_map(|a| text(a, "Name").map(String::from))
        .collect())
}

/// Networks the printer can see. It answers 202 while a scan is still running.
pub fn scan(p: &mut Printer, adapter: &str) -> Result<Vec<Network>, String> {
    let path = format!("{BASE}/{adapter}/WifiNetworks");
    for _ in 0..10 {
        let r = p.get(&path)?;
        if r.status == 202 { sleep(Duration::from_secs(3)); continue; }
        if r.status != 200 { return Err(format!("scan: HTTP {}", r.status)); }
        let doc = parse(&r.body)?;
        return Ok(doc
            .descendants()
            .filter(|n| n.tag_name().name() == "WifiNetwork")
            .map(|n| {
                let raw = text(n, "SSID").unwrap_or("");
                Network {
                    ssid: unhex(raw).unwrap_or_else(|| raw.to_string()),
                    encryption: text(n, "EncryptionType").unwrap_or("none").to_string(),
                    mode: text(n, "CommunicationMode").unwrap_or("infrastructure").to_string(),
                    strength: text(n, "SignalStrength").and_then(|s| s.parse().ok()).unwrap_or(0),
                }
            })
            .collect());
    }
    Err("scan didn't finish".into())
}

/// Switch the Wi-Fi radio on (two payload shapes, as hp-wificonfig tries).
pub fn power_on(p: &mut Printer, adapter: &str) -> Result<(), String> {
    let path = format!("{BASE}/{adapter}");
    let a = format!(r#"<?xml version="1.0" encoding="UTF-8"?><io:Adapters {NS}><io:Adapter><io:HardwareConfig><dd:Power>on</dd:Power></io:HardwareConfig></io:Adapter></io:Adapters>"#);
    if ok(p.put(&path, &a)?.status) { return Ok(()); }
    let b = format!(r#"<?xml version="1.0" encoding="UTF-8" ?><io:Adapter {NS}><io:HardwareConfig><dd:Power>on</dd:Power></io:HardwareConfig></io:Adapter>"#);
    let s = p.put(&path, &b)?.status;
    if ok(s) { Ok(()) } else { Err(format!("couldn't turn on the printer's Wi-Fi (HTTP {s})")) }
}

/// Give the printer a network and passphrase. SSID and key go hex-encoded.
pub fn associate(p: &mut Printer, adapter: &str, net: &Network, key: &str) -> Result<(), String> {
    let open = net.encryption.eq_ignore_ascii_case("none");
    let auth = if open { "open" } else { net.encryption.as_str() };
    let key_info = if open {
        String::new()
    } else {
        format!(
            "<io:KeyInfo><io:WpaPassPhraseInfo><wifi:RsnEncryption>AESOrTKIP</wifi:RsnEncryption>\
             <wifi:RsnAuthorization>autoWPA</wifi:RsnAuthorization><wifi:PassPhrase>{}</wifi:PassPhrase>\
             </io:WpaPassPhraseInfo></io:KeyInfo>",
            hex(key)
        )
    };
    let xml = format!(
        r#"<io:Profile {NS} xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><io:AdapterProfile><io:WifiProfile><wifi:SSID>{}</wifi:SSID><wifi:CommunicationMode>{}</wifi:CommunicationMode><wifi:EncryptionType>{}</wifi:EncryptionType><wifi:AuthenticationMode>{}</wifi:AuthenticationMode>{key_info}</io:WifiProfile></io:AdapterProfile></io:Profile>"#,
        hex(&net.ssid), net.mode, net.encryption, auth
    );
    let s = p.put(&format!("{BASE}/{adapter}/Profiles/Active"), &xml)?.status;
    if ok(s) { Ok(()) } else { Err(format!("the printer didn't accept the Wi-Fi settings (HTTP {s})")) }
}

fn first_ip(xml: &str, names: &[&str]) -> Option<Ipv4Addr> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    doc.descendants()
        .filter(|n| names.contains(&n.tag_name().name()))
        .filter_map(|n| n.text()?.trim().parse::<Ipv4Addr>().ok())
        .find(|ip| !ip.is_unspecified())
}

/// The printer's Wi-Fi IPv4 address, if it has one yet.
pub fn ip(p: &mut Printer, adapter: &str) -> Result<Option<Ipv4Addr>, String> {
    let r = p.get(&format!("{BASE}/{adapter}/Protocols"))?;
    if let Some(ip) = first_ip(&r.body, &["IPv4Address"]) { return Ok(Some(ip)); }
    let r = p.get(&format!("{BASE}/{adapter}/Profiles/Active"))?;
    Ok(first_ip(&r.body, &["IPAddress"]))
}

pub fn hostname(p: &mut Printer) -> Result<Option<String>, String> {
    let r = p.get("/IoMgmt/IoConfig.xml")?;
    Ok(roxmltree::Document::parse(&r.body).ok().and_then(|d| text(d.root(), "Hostname").map(String::from)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        assert_eq!(hex("Hi !"), "48692021");
        assert_eq!(unhex(&hex("Home Wi-Fi 2.4")).unwrap(), "Home Wi-Fi 2.4");
    }

    #[test]
    fn ip_ignores_unset() {
        let xml = r#"<a xmlns:dd="x"><dd:IPv4Address>0.0.0.0</dd:IPv4Address><dd:IPv4Address>10.0.0.42</dd:IPv4Address></a>"#;
        assert_eq!(first_ip(xml, &["IPv4Address"]), Some("10.0.0.42".parse().unwrap()));
    }
}
