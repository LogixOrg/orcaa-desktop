//! Silent ESC/POS receipt printing — the thing a browser can never do.
//!
//! The web POS builds a receipt as a list of semantic line ops (text / pair /
//! rule / cut / drawer…); this module turns them into raw ESC/POS bytes and
//! pushes them straight at the printer — a Windows print queue in RAW mode,
//! TCP port 9100, or a serial COM port — with no page, no scaling, and above
//! all **no print dialog**. The cash drawer kick rides the same wire (drawers
//! plug into the printer's RJ11).
//!
//! "printer" (the RAW spooler path, see `spooler.rs`) is the DEFAULT worth
//! reaching for: a USB thermal printer — which is most of them — exposes
//! neither a socket nor a COM port, only a Windows queue. It is also the only
//! transport that needs no setup at all: the queue is already installed by the
//! vendor driver, so `shell_pos_printer_autodetect` can find it and save the
//! config itself.
//!
//! The byte protocol is hand-rolled on purpose: ESC/POS is a dozen constant
//! sequences, and owning them beats guessing at a wrapper crate's API. Only
//! transport (`serialport`) and text encoding (`encoding_rs`) are dependencies.
//!
//! Arabic: text is encoded to the configured codepage (default windows-1256).
//! Whether the glyphs shape correctly is FIRMWARE-dependent — many thermal
//! printers ship an Arabic codepage that shapes and reorders on-printer, some
//! don't. That is why `shell_pos_test_print` prints an Arabic line: one test
//! page answers the question for the hardware in front of the user.

use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

use crate::STORE_FILE;

const STORE_KEY_PRINTER: &str = "pos_printer";

/// Wire timeout for both transports. Receipts are a few KB; anything slower
/// than this is a wrong address, not a slow printer.
const IO_TIMEOUT: Duration = Duration::from_secs(4);

/// Hard ceiling on ops per receipt — a runaway payload from the page must not
/// be able to spool paper for minutes.
const MAX_OPS: usize = 400;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub struct PrinterConfig {
    /// "printer" (Windows spooler, RAW), "network" (TCP:9100) or "serial"
    /// (COM port / tty).
    pub interface: String,
    /// The Windows queue name for "printer" (as shown in Settings > Printers),
    /// `host:port` for network (port defaults to 9100 if omitted), device path
    /// for serial (`COM3`, `/dev/ttyUSB0`).
    pub address: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    /// Characters per line: 32 (58mm paper) or 48 (80mm).
    #[serde(default = "default_width")]
    pub width: u8,
    /// The `ESC t n` codepage slot — printer-specific. Left at the printer's
    /// default when `None`.
    #[serde(default)]
    pub codepage: Option<u8>,
    /// How text bytes are produced: "cp1256" (Arabic), "cp437", or "ascii".
    #[serde(default = "default_encoding")]
    pub encoding: String,
    /// Fire the drawer kick after a cash sale's receipt.
    #[serde(default)]
    pub drawer_kick: bool,
    /// How a line of text reaches the paper.
    ///
    /// "codepage" (default) sends CHARACTERS and lets the printer draw them —
    /// fast, crisp, and correct only when the firmware knows how to SHAPE
    /// Arabic. Cheap Xprinter/Rongta clones don't: they map cp1256 byte by
    /// byte, so every Arabic word prints as disconnected isolated letters in
    /// the wrong order.
    ///
    /// "raster" sends the receipt as a monochrome IMAGE the web side already
    /// laid out with the browser's own Arabic shaping (see the web side's
    /// `pos/utils/rasterReceipt.ts`). Slower and a little heavier on the wire,
    /// but the paper matches the screen on ANY ESC/POS printer.
    #[serde(default = "default_text_mode")]
    pub text_mode: String,
}

fn default_baud() -> u32 {
    9600
}
fn default_width() -> u8 {
    48
}
fn default_encoding() -> String {
    "cp1256".to_string()
}
pub(crate) fn default_text_mode() -> String {
    "codepage".to_string()
}

impl PrinterConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !matches!(self.interface.as_str(), "printer" | "network" | "serial") {
            return Err("interface must be \"printer\", \"network\" or \"serial\"".into());
        }
        if self.address.trim().is_empty() {
            return Err("printer address is empty".into());
        }
        if !matches!(self.width, 32 | 48) {
            return Err("width must be 32 or 48 characters".into());
        }
        if !matches!(self.encoding.as_str(), "cp1256" | "cp437" | "ascii") {
            return Err("encoding must be cp1256, cp437 or ascii".into());
        }
        if !matches!(self.text_mode.as_str(), "codepage" | "raster") {
            return Err("text_mode must be \"codepage\" or \"raster\"".into());
        }
        Ok(())
    }
}

pub(crate) fn load_config(app: &AppHandle) -> Option<PrinterConfig> {
    let store = app.store(STORE_FILE).ok()?;
    let value = store.get(STORE_KEY_PRINTER)?;
    serde_json::from_value(value).ok()
}

// ---------------------------------------------------------------------------
// Receipt ops
// ---------------------------------------------------------------------------

/// One semantic line of a receipt. The page describes WHAT to print; layout
/// within the shell stays dumb enough that any POS change is web-only.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ReceiptOp {
    /// A text line. `align`: "left" | "center" | "right".
    Text {
        value: String,
        #[serde(default)]
        align: Option<String>,
        #[serde(default)]
        bold: bool,
        /// Double width+height — totals, the business name.
        #[serde(default)]
        big: bool,
    },
    /// Left and right text on one line, padded apart (item ↔ price).
    Pair { left: String, right: String },
    /// A full-width rule of dashes.
    Hr,
    /// Blank feed lines.
    Feed { lines: u8 },
    /// QR code (e.g. a receipt or review link).
    Qr { value: String },
    /// Partial cut with feed.
    Cut,
    /// Cash drawer pulse — honored only when the config enables it.
    Drawer,
}

// ---------------------------------------------------------------------------
// ESC/POS encoding
// ---------------------------------------------------------------------------

fn encode_text(value: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "cp1256" => encoding_rs::WINDOWS_1256.encode(value).0.into_owned(),
        "cp437" => encoding_rs::IBM866.encode(value).0.into_owned(),
        _ => value
            .chars()
            .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
            .collect(),
    }
}

/// Pads `left` and `right` apart to the line width (in bytes of the encoded
/// text, which is one column per byte on these codepages).
fn pair_line(left: &[u8], right: &[u8], width: usize) -> Vec<u8> {
    let used = left.len() + right.len();
    let mut line = Vec::with_capacity(width.max(used) + 1);
    line.extend_from_slice(left);
    if used < width {
        line.extend(std::iter::repeat_n(b' ', width - used));
    } else {
        line.push(b' ');
    }
    line.extend_from_slice(right);
    line
}

/// `pub(crate)` for `kitchen.rs`: a KOT is the same byte protocol, and two
/// encoders would drift.
pub(crate) fn encode_receipt(ops: &[ReceiptOp], config: &PrinterConfig) -> Result<Vec<u8>, String> {
    if ops.len() > MAX_OPS {
        return Err(format!(
            "receipt too long ({} ops, max {MAX_OPS})",
            ops.len()
        ));
    }

    let width = config.width as usize;
    let mut out: Vec<u8> = Vec::with_capacity(1024);

    out.extend_from_slice(&[0x1B, 0x40]); // ESC @  init
    if let Some(page) = config.codepage {
        out.extend_from_slice(&[0x1B, 0x74, page]); // ESC t  codepage
    }

    for op in ops {
        match op {
            ReceiptOp::Text {
                value,
                align,
                bold,
                big,
            } => {
                let align_byte = match align.as_deref() {
                    Some("center") => 1,
                    Some("right") => 2,
                    _ => 0,
                };
                out.extend_from_slice(&[0x1B, 0x61, align_byte]); // ESC a
                if *bold {
                    out.extend_from_slice(&[0x1B, 0x45, 1]); // ESC E
                }
                if *big {
                    out.extend_from_slice(&[0x1D, 0x21, 0x11]); // GS !  2x2
                }
                out.extend(encode_text(value, &config.encoding));
                out.push(b'\n');
                if *big {
                    out.extend_from_slice(&[0x1D, 0x21, 0x00]);
                }
                if *bold {
                    out.extend_from_slice(&[0x1B, 0x45, 0]);
                }
                out.extend_from_slice(&[0x1B, 0x61, 0]);
            }
            ReceiptOp::Pair { left, right } => {
                let line = pair_line(
                    &encode_text(left, &config.encoding),
                    &encode_text(right, &config.encoding),
                    width,
                );
                out.extend(line);
                out.push(b'\n');
            }
            ReceiptOp::Hr => {
                out.extend(std::iter::repeat_n(b'-', width));
                out.push(b'\n');
            }
            ReceiptOp::Feed { lines } => {
                out.extend_from_slice(&[0x1B, 0x64, (*lines).min(10)]); // ESC d
            }
            ReceiptOp::Qr { value } => {
                let data = value.as_bytes();
                if data.is_empty() || data.len() > 700 {
                    continue;
                }
                out.extend_from_slice(&[0x1B, 0x61, 1]); // centered
                out.extend_from_slice(&[0x1D, 0x28, 0x6B, 4, 0, 0x31, 0x41, 0x32, 0]); // model 2
                out.extend_from_slice(&[0x1D, 0x28, 0x6B, 3, 0, 0x31, 0x43, 6]); // module size
                out.extend_from_slice(&[0x1D, 0x28, 0x6B, 3, 0, 0x31, 0x45, 48]); // EC level L
                let len = data.len() + 3;
                out.extend_from_slice(&[
                    0x1D,
                    0x28,
                    0x6B,
                    (len & 0xFF) as u8,
                    (len >> 8) as u8,
                    0x31,
                    0x50,
                    0x30,
                ]);
                out.extend_from_slice(data);
                out.extend_from_slice(&[0x1D, 0x28, 0x6B, 3, 0, 0x31, 0x51, 0x30]); // print
                out.extend_from_slice(&[0x1B, 0x61, 0]);
            }
            ReceiptOp::Cut => {
                out.extend_from_slice(&[0x1B, 0x64, 4]); // feed clear of the blade
                out.extend_from_slice(&[0x1D, 0x56, 0x42, 0]); // GS V  partial cut
            }
            ReceiptOp::Drawer => {
                if config.drawer_kick {
                    out.extend_from_slice(&[0x1B, 0x70, 0, 0x19, 0xFA]); // ESC p
                }
            }
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// One wire, three transports — shared by receipts (ESC/POS) and labels
/// (TSPL): the transport neither knows nor cares which byte protocol rides it.
pub fn send_bytes(
    interface: &str,
    address: &str,
    baud: u32,
    bytes: &[u8],
    document: &str,
) -> Result<(), String> {
    match interface {
        "printer" => {
            #[cfg(windows)]
            {
                crate::spooler::send_raw(address, bytes, document)
            }
            #[cfg(not(windows))]
            {
                let _ = document;
                Err("printing through a system print queue is Windows-only;                      use a network or serial printer here"
                    .to_string())
            }
        }
        "network" => {
            let address = if address.contains(':') {
                address.to_string()
            } else {
                format!("{address}:9100")
            };
            let target = address
                .to_socket_addrs()
                .map_err(|e| format!("bad printer address {address}: {e}"))?
                .next()
                .ok_or_else(|| format!("printer address {address} did not resolve"))?;

            let mut stream = TcpStream::connect_timeout(&target, IO_TIMEOUT)
                .map_err(|e| format!("can't reach the printer at {address}: {e}"))?;
            stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
            stream
                .write_all(bytes)
                .map_err(|e| format!("printing failed mid-receipt: {e}"))?;
            stream.flush().ok();
            Ok(())
        }
        "serial" => {
            let mut port = serialport::new(address, baud)
                .timeout(IO_TIMEOUT)
                .open()
                .map_err(|e| format!("can't open {address}: {e}"))?;
            port.write_all(bytes)
                .map_err(|e| format!("printing failed mid-receipt: {e}"))?;
            port.flush().ok();
            Ok(())
        }
        other => Err(format!("unknown printer interface {other}")),
    }
}

fn send_to_printer(config: &PrinterConfig, bytes: &[u8]) -> Result<(), String> {
    send_bytes(
        &config.interface,
        &config.address,
        config.baud,
        bytes,
        "Orcaa receipt",
    )
}

async fn print_ops(app: AppHandle, ops: Vec<ReceiptOp>) -> Result<(), String> {
    let config = load_config(&app).ok_or("no printer configured")?;
    config.validate()?;
    let bytes = encode_receipt(&ops, &config)?;

    // Blocking socket/serial I/O has no business on the async runtime's core.
    tauri::async_runtime::spawn_blocking(move || send_to_printer(&config, &bytes))
        .await
        .map_err(|e| format!("print task failed: {e}"))?
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Prints a receipt described by the page. Errors come back as strings the
/// POS surfaces in its own (localized) toast.
#[tauri::command]
pub async fn shell_pos_print(app: AppHandle, ops: Vec<ReceiptOp>) -> Result<(), String> {
    print_ops(app, ops).await
}

// NOTE: there is deliberately no standalone drawer-kick command. The drawer
// opens ONLY as a `ReceiptOp::Drawer` inside a cash sale/refund receipt print
// (still gated on the config's `drawer_kick`) — a manual "no sale" pop is the
// classic skim window, so the capability does not exist at any layer.

/// One page that answers "is this printer wired right": layout, both scripts,
/// a QR — everything the real receipts use. Deliberately does NOT kick the
/// drawer; a test page popping the till would be a nasty surprise.
#[tauri::command]
pub async fn shell_pos_test_print(app: AppHandle) -> Result<(), String> {
    let config = load_config(&app).ok_or("no printer configured")?;
    let ops = vec![
        ReceiptOp::Text {
            value: "Orcaa".into(),
            align: Some("center".into()),
            bold: true,
            big: true,
        },
        ReceiptOp::Text {
            value: "Printer test".into(),
            align: Some("center".into()),
            bold: false,
            big: false,
        },
        ReceiptOp::Hr,
        ReceiptOp::Pair {
            left: "Width".into(),
            right: format!("{} cols", config.width),
        },
        ReceiptOp::Pair {
            left: "Encoding".into(),
            right: config.encoding.clone(),
        },
        ReceiptOp::Text {
            value: "اختبار الطباعة بالعربية".into(),
            align: Some("center".into()),
            bold: false,
            big: false,
        },
        ReceiptOp::Hr,
        ReceiptOp::Qr {
            value: "https://orcaa.cloud".into(),
        },
        ReceiptOp::Feed { lines: 2 },
        ReceiptOp::Cut,
    ];
    print_ops(app, ops).await
}

#[tauri::command]
pub fn shell_pos_printer_get(app: AppHandle) -> Option<PrinterConfig> {
    load_config(&app)
}

/// Saves (or clears, with `None`) the printer config. Validated here so a
/// broken config can never be persisted and then fail every sale.
#[tauri::command]
pub fn shell_pos_printer_set(app: AppHandle, config: Option<PrinterConfig>) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    match config {
        Some(config) => {
            config.validate()?;
            store.set(
                STORE_KEY_PRINTER,
                serde_json::to_value(&config).map_err(|e| e.to_string())?,
            );
        }
        None => {
            store.delete(STORE_KEY_PRINTER);
        }
    }
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// The printers installed on this machine, for the settings page's picker —
/// typing a queue name by hand is how "Microsoft XPS Document Writer" ends up
/// in the address field of a serial printer.
#[tauri::command]
pub fn shell_pos_printers_list() -> Result<Vec<PrinterCandidate>, String> {
    #[cfg(windows)]
    {
        Ok(crate::spooler::list_printers()?
            .into_iter()
            .map(PrinterCandidate::from)
            .collect())
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

/// Finds the counter's receipt printer and SAVES it, so a fresh station prints
/// silently without anyone opening settings. Returns the config it stored, or
/// `None` when no thermal printer is installed (then the page falls back to the
/// PDF receipt and the settings page can still be used by hand).
///
/// Never overwrites an existing config: once someone has chosen a printer, that
/// choice outranks detection.
#[tauri::command]
pub fn shell_pos_printer_autodetect(app: AppHandle) -> Result<Option<PrinterConfig>, String> {
    if let Some(existing) = load_config(&app) {
        return Ok(Some(existing));
    }

    #[cfg(windows)]
    {
        let Some(found) = crate::spooler::detect_receipt_printer() else {
            return Ok(None);
        };

        let config = PrinterConfig {
            interface: "printer".to_string(),
            address: found.name,
            baud: default_baud(),
            // The model name usually states the roll (XP-80C -> 80mm -> 48
            // columns); 80mm is the safer guess when it does not, since a
            // 32-column receipt on 80mm paper is merely narrow, while the
            // reverse wraps every line.
            width: if found.roll_width_mm == Some(58) {
                32
            } else {
                48
            },
            codepage: None,
            encoding: default_encoding(),
            drawer_kick: true,
            text_mode: "codepage".to_string(),
        };

        config.validate()?;
        shell_pos_printer_set(app, Some(config.clone()))?;

        Ok(Some(config))
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

/// What the settings picker shows for one installed printer. Mirrors
/// `spooler::InstalledPrinter` so the page never sees a platform type.
#[derive(Clone, Serialize)]
pub struct PrinterCandidate {
    pub name: String,
    pub driver: String,
    pub is_default: bool,
    pub is_virtual: bool,
    pub is_thermal: bool,
    pub is_label: bool,
    pub roll_width_mm: Option<u32>,
}

#[cfg(windows)]
impl From<crate::spooler::InstalledPrinter> for PrinterCandidate {
    fn from(printer: crate::spooler::InstalledPrinter) -> Self {
        Self {
            name: printer.name,
            driver: printer.driver,
            is_default: printer.is_default,
            is_virtual: printer.is_virtual,
            is_thermal: printer.is_thermal,
            is_label: printer.is_label,
            roll_width_mm: printer.roll_width_mm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PrinterConfig {
        PrinterConfig {
            interface: "network".into(),
            address: "192.168.1.50".into(),
            baud: 9600,
            width: 32,
            codepage: Some(50),
            encoding: "cp1256".into(),
            drawer_kick: true,
            text_mode: "codepage".to_string(),
        }
    }

    #[test]
    fn a_receipt_begins_with_init_and_codepage() {
        let bytes = encode_receipt(&[ReceiptOp::Hr], &config()).unwrap();
        assert_eq!(&bytes[..5], &[0x1B, 0x40, 0x1B, 0x74, 50]);
    }

    #[test]
    fn pairs_pad_to_the_configured_width() {
        let bytes = encode_receipt(
            &[ReceiptOp::Pair {
                left: "Total".into(),
                right: "150.00".into(),
            }],
            &config(),
        )
        .unwrap();
        // Skip init+codepage (5 bytes), drop the trailing newline.
        let line = &bytes[5..bytes.len() - 1];
        assert_eq!(line.len(), 32);
        assert!(line.starts_with(b"Total"));
        assert!(line.ends_with(b"150.00"));
    }

    #[test]
    fn arabic_encodes_to_single_byte_cp1256_columns() {
        let bytes = encode_text("اختبار", "cp1256");
        assert_eq!(
            bytes.len(),
            6,
            "one byte per letter, or padding math breaks"
        );
        assert!(bytes.iter().all(|b| *b >= 0x80), "must not be ASCII '?'");
    }

    #[test]
    fn the_drawer_only_fires_when_the_config_allows_it() {
        let mut cfg = config();
        let kick = encode_receipt(&[ReceiptOp::Drawer], &cfg).unwrap();
        assert!(kick.windows(2).any(|w| w == [0x1B, 0x70]));

        cfg.drawer_kick = false;
        let quiet = encode_receipt(&[ReceiptOp::Drawer], &cfg).unwrap();
        assert!(!quiet.windows(2).any(|w| w == [0x1B, 0x70]));
    }

    #[test]
    fn a_runaway_payload_is_refused() {
        let ops: Vec<ReceiptOp> = (0..MAX_OPS + 1).map(|_| ReceiptOp::Hr).collect();
        assert!(encode_receipt(&ops, &config()).is_err());
    }

    #[test]
    fn a_bad_config_never_validates() {
        let mut cfg = config();
        cfg.interface = "usb".into();
        assert!(cfg.validate().is_err());

        // The spooler transport is a first-class interface, not a typo.
        let mut cfg = config();
        cfg.interface = "printer".into();
        cfg.address = "فواتير".into();
        assert!(cfg.validate().is_ok());

        let mut cfg = config();
        cfg.width = 40;
        assert!(cfg.validate().is_err());

        let mut cfg = config();
        cfg.address = "  ".into();
        assert!(cfg.validate().is_err());
    }
}
