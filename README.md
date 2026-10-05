# deskjet-linux

Get an **HP DeskJet 3750** (USB id 03f0:0653, "DeskJet 3700 series") onto
Wi-Fi from Linux over its USB cable, then print to it over Wi-Fi with CUPS.
One small Rust program: no HPLIP, no Python, no HP app.

Companion to [sn420b-linux](https://github.com/kenners22/sn420b-linux), which
does the same for the SN 420B label printer.

```
deskjet wifi [SSID]   join the printer to Wi-Fi over USB (default: the laptop's network)
deskjet usbip         ask the printer (over USB) what Wi-Fi IP it has
deskjet networks      list the Wi-Fi networks the printer can see (over USB)
deskjet find          look for the printer on the network (Bonjour)
deskjet setup         add/update the CUPS queue "DeskJet3750" (asks for sudo)
deskjet status        show where it is and whether it's reachable
deskjet test          print a test page over Wi-Fi and check it was sent
```

Typical first run: plug in USB, `deskjet wifi`, `deskjet setup`, `deskjet test`,
unplug. After that, switch the printer on and print to `DeskJet3750`.

## How it works

**Wi-Fi over USB.** The printer speaks HP's LEDM protocol, the same thing
HPLIP's `hp-wificonfig` uses: plain HTTP/1.1 requests written to the USB
interface with class ff/cc/00 (two bulk endpoints, no extra framing), against
`/IoMgmt/Adapters/...` resources. `usb.rs` does that with
[nusb](https://crates.io/crates/nusb); `ledm.rs` has the requests. The XML
payloads match HPLIP's `base/LedmWifi.py`; SSID and passphrase go hex-encoded.

**Printing.** `deskjet setup` adds a CUPS queue on the printer's driverless
URI (`ipps://<Bonjour service name>._ipps._tcp.local/`) with IPP Everywhere.
CUPS resolves the name on every job, so a new DHCP address doesn't matter, and
jobs sent while the printer is off wait (`printer-error-policy=retry-job`).

## Things learned on this printer

- Over USB it often answers "busy" just after power-on or a replug, and with
  HPLIP about half the replies came back out of step. Draining stale bytes
  before every request fixed that (5 of 5 clean reads); the open and each
  request are retried anyway.
- Once, the "radio on" command mid-session made it drop off USB and the
  laptop's port needed a replug. `deskjet wifi` scans first and only switches
  the radio on if the scan comes back empty.
- Its Bonjour host name (`HP` + MAC) differs from the hostname it reports over
  USB (`HP` + last six MAC digits).
- It also listens on raw port 9100, so tools that find printers by scanning
  for 9100 will see it.
- CUPS's own `testprint` is a banner file that cups-filters 2 rejects, yet
  CUPS still marks the job "completed" with 0 pages. `deskjet test` sends a
  real PDF (written by `pdf.rs`) and checks the page count in
  `/var/log/cups/page_log`.

## Install

Needs Rust (`cargo`), CUPS with cups-filters (`lp`, `lpstat`, `lpadmin`,
`driverless`) and Avahi (`avahi-browse`, plus `nss-mdns` so `.local` names
resolve). On Arch: `sudo pacman -S --needed rust cups cups-filters avahi nss-mdns`.

```bash
git clone https://github.com/kenners22/deskjet-linux
cd deskjet-linux
cargo build --release
install -m755 target/release/deskjet ~/.local/bin/
# optional: permanent USB access for your desktop user
sudo cp 70-deskjet.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger
```

Without the udev rule, `deskjet` asks for sudo once to run
`sudo setfacl -m u:$USER:rw /dev/bus/usb/BBB/DDD` (lasts until unplugged).

## Use

1. Plug the printer in by USB and switch it on.
2. `deskjet wifi` joins it to the network your laptop is on (or
   `deskjet wifi "Network name"`). It asks for the Wi-Fi password, hidden,
   and sends it straight to the printer; nothing is saved.
3. `deskjet setup` adds the `DeskJet3750` CUPS queue (asks for sudo).
4. `deskjet test` prints a test page and confirms CUPS sent it.
5. Unplug the USB cable. From now on, switch the printer on and print to
   `DeskJet3750` from any app.

State: `~/.config/deskjet.conf` (`HOST=`, the printer's Bonjour host name).

## Tested with

- HP DeskJet 3750 (reports itself as "DeskJet 3700 series", USB `03f0:0653`)
- Arch-based Linux, CUPS 2.4.19, cups-filters 2.0.1

Other DeskJet 3700-series models (3720, 3730, 3755, 3775, 3785) share the USB
id and LEDM interface, so they probably work too. PRs with test reports are
welcome.

## License

MIT
