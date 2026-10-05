//! deskjet — join an HP DeskJet 3750 to Wi-Fi over USB and print to it over Wi-Fi.
//!
//! Wi-Fi is set over USB the way HP's hp-wificonfig does it (LEDM: small HTTP
//! requests down the printer's USB interface), spoken directly — no HPLIP.
//! The CUPS queue uses the printer's Bonjour service name and the driverless
//! IPP Everywhere driver, so a new DHCP address needs no fix step.

mod ledm;
mod pdf;
mod usb;

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::thread::sleep;
use std::time::Duration;

use usb::Printer;

const QUEUE: &str = "DeskJet3750";

const HELP: &str = "\
deskjet — join the HP DeskJet 3750 to Wi-Fi over USB and print to it over Wi-Fi.

  deskjet wifi [SSID]   join the printer to Wi-Fi over USB (default: the laptop's network)
  deskjet usbip         ask the printer (over USB) what Wi-Fi IP it has
  deskjet networks      list the Wi-Fi networks the printer can see (over USB)
  deskjet find          look for the printer on the network (Bonjour)
  deskjet setup         add/update the CUPS queue \"DeskJet3750\" (asks for sudo)
  deskjet status        show where it is and whether it's reachable
  deskjet test          print a test page over Wi-Fi and check it was sent";

type Res<T = ()> = Result<T, String>;

/// println! that doesn't panic when stdout is a closed pipe (`deskjet status | head -1`).
macro_rules! out {
    ($($t:tt)*) => {{ use std::io::Write; let _ = writeln!(std::io::stdout(), $($t)*); }};
}

fn conf_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("deskjet.conf")
}

/// Saved Bonjour host name (same `HOST=` file the old shell script used).
fn load_host() -> Option<String> {
    let s = std::fs::read_to_string(conf_path()).ok()?;
    s.lines().find_map(|l| l.strip_prefix("HOST=")).map(str::trim).filter(|h| !h.is_empty()).map(String::from)
}

fn save_host(host: &str) -> Res {
    let p = conf_path();
    if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(&p, format!("HOST={host}\n")).map_err(|e| e.to_string())
}

fn output(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn run_cmd(cmd: &str, args: &[&str]) -> Res {
    let ok = Command::new(cmd).args(args).status().map_err(|e| format!("{cmd}: {e}"))?.success();
    if ok { Ok(()) } else { Err(format!("{cmd} failed")) }
}

/// Bonjour IPP printers on the LAN that look like HPs: (host.local, address).
fn browse() -> Vec<(String, String)> {
    let mut hits: Vec<(String, String)> = output("timeout", &["8", "avahi-browse", "-rtp", "_ipp._tcp"])
        .lines()
        .map(|l| l.split(';').collect::<Vec<_>>())
        .filter(|f| f.len() > 7 && f[0] == "=" && f[2] == "IPv4")
        .filter(|f| { let n = f[3].to_ascii_lowercase(); n.contains("hp") || n.contains("deskjet") })
        .map(|f| (f[6].to_string(), f[7].to_string()))
        .collect();
    hits.sort();
    hits.dedup();
    hits
}

/// The Wi-Fi network this laptop is on (NetworkManager or iwd), as the default.
fn laptop_ssid() -> Option<String> {
    let nm = output("nmcli", &["-t", "-f", "active,ssid", "dev", "wifi"]);
    if let Some(s) = nm.lines().find_map(|l| l.strip_prefix("yes:")) {
        return Some(s.replace("\\:", ":")).filter(|s| !s.is_empty());
    }
    output("iwgetid", &["-r"]).lines().next().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn wifi_adapter(p: &mut Printer) -> Res<String> {
    ledm::wifi_adapters(p)?.into_iter().next().ok_or_else(|| "printer has no Wi-Fi adapter".into())
}

fn cmd_wifi(ssid: Option<String>) -> Res {
    let ssid = ssid
        .or_else(laptop_ssid)
        .ok_or("not on Wi-Fi — name the network: deskjet wifi \"Name\"")?;
    eprintln!("Joining the printer to \"{ssid}\" (another network: deskjet wifi \"Name\")");
    let mut p = Printer::open()?;
    let adapter = wifi_adapter(&mut p)?;
    eprintln!("Scanning for \"{ssid}\"…");
    let mut nets = ledm::scan(&mut p, &adapter)?;
    if nets.is_empty() {
        // Radio off: switch it on the way hp-wificonfig does, then rescan.
        ledm::power_on(&mut p, &adapter)?;
        nets = ledm::scan(&mut p, &adapter)?;
    }
    let net = nets
        .into_iter()
        .filter(|n| n.ssid == ssid)
        .max_by_key(|n| n.strength)
        .ok_or_else(|| format!("the printer can't see \"{ssid}\" (it needs 2.4 GHz)"))?;
    eprintln!("Found it: signal {}/5, {}", net.strength, net.encryption);
    let key = if net.encryption.eq_ignore_ascii_case("none") {
        String::new()
    } else {
        let k = rpassword::prompt_password(format!("Password for \"{ssid}\" (hidden): ")).map_err(|e| e.to_string())?;
        if k.is_empty() { return Err("password can't be empty".into()); }
        k
    };
    ledm::associate(&mut p, &adapter, &net, &key)?;
    drop(key);
    eprintln!("Waiting for it to join (up to ~2 min)…");
    let mut ip = None;
    for _ in 0..24 {
        sleep(Duration::from_secs(5));
        if let Ok(Some(a)) = ledm::ip(&mut p, &adapter) { ip = Some(a); break; }
    }
    let Some(ip) = ip else {
        return Err("printer didn't get an IP. Check the password, then try again.".into());
    };
    let host = ledm::hostname(&mut p).ok().flatten().unwrap_or_default();
    eprintln!("Printer joined \"{ssid}\" at {ip} ({host})");
    // Its Bonjour name (HP + MAC) differs from the hostname it reports over USB.
    if let Some((name, _)) = browse().into_iter().find(|(_, a)| *a == ip.to_string()) {
        save_host(&name)?;
        eprintln!("Network name: {name}");
    }
    eprintln!("Next: deskjet setup");
    Ok(())
}

fn cmd_usbip() -> Res {
    let mut p = Printer::open()?;
    let adapter = wifi_adapter(&mut p)?;
    match ledm::ip(&mut p, &adapter)? {
        Some(ip) => out!("{ip}"),
        None => out!("0.0.0.0"),
    }
    Ok(())
}

fn cmd_networks() -> Res {
    let mut p = Printer::open()?;
    let adapter = wifi_adapter(&mut p)?;
    let mut nets = ledm::scan(&mut p, &adapter)?;
    nets.sort_by_key(|n| std::cmp::Reverse(n.strength));
    for n in nets { out!("{}/5  {:<5}  {}", n.strength, n.encryption, n.ssid); }
    if let Some(h) = ledm::hostname(&mut p)? { eprintln!("(printer hostname {h})"); }
    Ok(())
}

fn cmd_find() -> Res {
    let hits = browse();
    if hits.is_empty() { return Err("No HP printer found. Is it on and joined to Wi-Fi?".into()); }
    for (host, addr) in hits { out!("{host}  {addr}"); }
    Ok(())
}

fn cmd_setup() -> Res {
    // CUPS's driverless URI names the printer's Bonjour service, which CUPS
    // resolves on every job — so a new DHCP address doesn't matter.
    let uri = output("timeout", &["20", "driverless"])
        .lines()
        .find(|l| l.to_ascii_lowercase().contains("deskjet%203700"))
        .map(String::from)
        .ok_or("printer not found on the network — is it on and joined to Wi-Fi? (deskjet wifi)")?;
    eprintln!("Setting up queue {QUEUE} → {uri} (sudo needed)");
    run_cmd("sudo", &["lpadmin", "-p", QUEUE, "-E", "-v", &uri, "-m", "everywhere",
        "-D", "HP DeskJet 3750", "-L", "Wi-Fi", "-o", "printer-error-policy=retry-job"])?;
    eprintln!("Done. Print with: lp -d {QUEUE} file.pdf");
    Ok(())
}

fn cmd_status() -> Res {
    let host = load_host().ok_or("Not set up yet — run: deskjet wifi, then deskjet setup")?;
    let addr: Option<SocketAddr> =
        (host.as_str(), 631).to_socket_addrs().ok().and_then(|mut a| a.find(SocketAddr::is_ipv4));
    match addr {
        Some(a) => out!("Printer: {host} ({})", a.ip()),
        None => out!("Printer: {host}"),
    }
    let up = addr.is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_secs(2)).is_ok());
    out!("Wi-Fi:   {}", if up { "reachable" } else { "NOT reachable (is it switched on?)" });
    let q = output("lpstat", &["-p", QUEUE, "-v", QUEUE]);
    if q.is_empty() { out!("CUPS queue {QUEUE}: not installed"); } else { out!("{}", q.trim_end()); }
    Ok(())
}

/// CUPS calls a job "completed" even when its filters failed (0 pages), so
/// check how many pages it actually sent.
fn cmd_test() -> Res {
    if output("lpstat", &["-p", QUEUE]).is_empty() { return Err("no queue yet — run: deskjet setup".into()); }
    let stamp = output("date", &["+%F %H:%M"]);
    let file = std::env::temp_dir().join(format!("deskjet-test-{}.pdf", std::process::id()));
    std::fs::write(&file, pdf::test_page(stamp.trim())).map_err(|e| e.to_string())?;
    let out = output("lp", &["-d", QUEUE, "-t", "DeskJet Wi-Fi test", &file.to_string_lossy()]);
    let id = out
        .split_whitespace()
        .find(|w| w.starts_with(&format!("{QUEUE}-")))
        .map(String::from)
        .ok_or("lp didn't accept the job")?;
    eprintln!("Sent {id}; waiting for CUPS to hand it to the printer…");
    for _ in 0..60 {
        if !output("lpstat", &["-o", QUEUE]).lines().any(|l| l.split_whitespace().next() == Some(&id)) { break; }
        sleep(Duration::from_secs(2));
    }
    let _ = std::fs::remove_file(&file);
    let n = id.rsplit('-').next().unwrap_or("");
    let pages: u32 = std::fs::read_to_string("/var/log/cups/page_log")
        .unwrap_or_default()
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|f| f.len() > 6 && f[0] == QUEUE && f[2] == n)
        .filter_map(|f| f[6].parse().ok())
        .next_back()
        .unwrap_or(0);
    if pages > 0 {
        eprintln!("CUPS sent {pages} page(s) to the printer.");
        Ok(())
    } else {
        Err(format!("CUPS sent 0 pages — see: lpstat -l -o {QUEUE} and /var/log/cups/error_log"))
    }
}

/// Run the command line: `args` are the arguments after the program name.
pub fn run(args: Vec<String>) -> ExitCode {
    let mut args = args.into_iter();
    let cmd = args.next().unwrap_or_default();
    let res = match cmd.as_str() {
        "wifi" => cmd_wifi(args.next()),
        "usbip" => cmd_usbip(),
        "networks" => cmd_networks(),
        "find" => cmd_find(),
        "setup" => cmd_setup(),
        "status" => cmd_status(),
        "test" => cmd_test(),
        _ => { out!("{HELP}"); Ok(()) }
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("error: {e}"); ExitCode::FAILURE }
    }
}
