//! Silent barcode-label printing — TSPL, the language of desktop label printers.
//!
//! Receipts speak ESC/POS; die-cut label stock speaks TSPL (Xprinter's label
//! series, TSC, Gprinter — the printers actually on counters here; Zebra's ZPL
//! is the notable other dialect and can be added as a `language` later). The
//! decisive design point: the BARCODE is drawn by the printer firmware via
//! TSPL's `BARCODE` command at exact dot widths — never as an image that a PDF
//! viewer or driver rescales. Rescaled bars are why a sticker can look perfect
//! and still refuse to scan on a 203 dpi head.
//!
//! Arabic product names are the one thing firmware can't draw (TSPL internal
//! fonts are Latin), so the page rasterizes the name/price strip on a canvas —
//! where the browser's own text stack shapes Arabic correctly — and sends it
//! as a 1-bit `BITMAP`, rendered at exact dot dimensions so nothing scales.
//!
//! Like receipts, the page sends semantic ops and this module owns the bytes.

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

use crate::print::send_bytes;
use crate::STORE_FILE;

const STORE_KEY_LABEL_PRINTER: &str = "pos_label_printer";

/// 203 dpi — every desktop label printer in this market. 300 dpi models exist;
/// the config carries the value so they work by changing one number.
const DEFAULT_DPMM: u8 = 8;

/// Hard ceiling on labels per job — a runaway loop must not empty a roll.
const MAX_LABELS_PER_JOB: usize = 100;

/// Hard ceiling on a name-strip bitmap (dots). 80mm × 50mm at 12 dots/mm is
/// well inside this; anything bigger is a malformed payload, not a label.
const MAX_BITMAP_BYTES: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub struct LabelPrinterConfig {
    /// "printer" (Windows spooler, RAW), "network" (TCP:9100) or "serial".
    pub interface: String,
    /// Queue name / `host:port` / COM port — same semantics as the receipt
    /// printer's address.
    pub address: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    /// The die-cut stock loaded, in millimetres.
    pub width_mm: f32,
    pub height_mm: f32,
    /// Gap between labels (die-cut). 2mm is the market default.
    #[serde(default = "default_gap")]
    pub gap_mm: f32,
    /// Print head resolution in dots per millimetre (203 dpi = 8).
    #[serde(default = "default_dpmm")]
    pub dpmm: u8,
    /// TSPL DENSITY 0–15.
    #[serde(default = "default_density")]
    pub density: u8,
}

fn default_baud() -> u32 {
    9600
}
fn default_gap() -> f32 {
    2.0
}
fn default_dpmm() -> u8 {
    DEFAULT_DPMM
}
fn default_density() -> u8 {
    8
}

impl LabelPrinterConfig {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.interface.as_str(), "printer" | "network" | "serial") {
            return Err("interface must be \"printer\", \"network\" or \"serial\"".into());
        }
        if self.address.trim().is_empty() {
            return Err("printer address is empty".into());
        }
        if !(10.0..=120.0).contains(&self.width_mm) || !(5.0..=120.0).contains(&self.height_mm) {
            return Err("label size must be between 10 and 120 mm".into());
        }
        if !(0.0..=10.0).contains(&self.gap_mm) {
            return Err("label gap must be between 0 and 10 mm".into());
        }
        if !matches!(self.dpmm, 8 | 12) {
            return Err("dpmm must be 8 (203 dpi) or 12 (300 dpi)".into());
        }
        if self.density > 15 {
            return Err("density must be 0-15".into());
        }
        Ok(())
    }
}

fn load_config(app: &AppHandle) -> Option<LabelPrinterConfig> {
    let store = app.store(STORE_FILE).ok()?;
    let value = store.get(STORE_KEY_LABEL_PRINTER)?;
    serde_json::from_value(value).ok()
}

// ---------------------------------------------------------------------------
// Label ops
// ---------------------------------------------------------------------------

/// One element on a label. Coordinates are DOTS from the top-left; the page
/// computes layout (it knows the fonts), this module owns only the protocol.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum LabelOp {
    /// A 1-bit bitmap strip (the Arabic name/price block). `data` is base64 of
    /// row-major packed bytes, `1` = black — inverted here to TSPL's sense.
    Bitmap {
        x: u32,
        y: u32,
        width_dots: u32,
        height_dots: u32,
        data: String,
    },
    /// A barcode drawn BY THE PRINTER. `symbology`: "ean13" | "code128".
    Barcode {
        x: u32,
        y: u32,
        symbology: String,
        value: String,
        height_dots: u32,
        /// Narrow-bar width in dots (2 at 203 dpi scans reliably).
        #[serde(default = "default_module")]
        module: u8,
    },
    /// ASCII-only text via the printer's internal font (codes, prices).
    /// Anything non-ASCII belongs in a Bitmap.
    Text {
        x: u32,
        y: u32,
        value: String,
        /// TSPL font "1"–"5"; sizes are firmware-defined.
        #[serde(default = "default_font")]
        font: String,
        #[serde(default = "default_scale")]
        scale: u8,
    },
}

fn default_module() -> u8 {
    2
}
fn default_font() -> String {
    "3".to_string()
}
fn default_scale() -> u8 {
    1
}

/// One physical sticker: its ops, printed `copies` times.
#[derive(Deserialize)]
pub struct LabelJob {
    pub ops: Vec<LabelOp>,
    #[serde(default = "default_copies")]
    pub copies: u16,
}

fn default_copies() -> u16 {
    1
}

// ---------------------------------------------------------------------------
// TSPL encoding
// ---------------------------------------------------------------------------

/// TSPL's names for the symbologies we print. EAN-13 takes the 12 data digits
/// and the firmware appends the check digit — handled in `encode_barcode`.
fn tspl_symbology(symbology: &str) -> Result<&'static str, String> {
    match symbology {
        "ean13" => Ok("EAN13"),
        "code128" => Ok("128"),
        other => Err(format!("unsupported barcode symbology {other}")),
    }
}

/// TSPL strings are quoted; a quote in the payload would end the argument and
/// the rest would parse as commands. Codes are alphanumeric in practice, so
/// stripping is safer than escaping (firmwares disagree on escapes).
fn tspl_quote(value: &str) -> String {
    value.replace(['"', '\r', '\n'], "")
}

fn encode_bitmap(
    out: &mut Vec<u8>,
    x: u32,
    y: u32,
    width_dots: u32,
    height_dots: u32,
    data: &str,
) -> Result<(), String> {
    let bytes_per_row = width_dots.div_ceil(8) as usize;
    let expected = bytes_per_row * height_dots as usize;

    if expected == 0 || expected > MAX_BITMAP_BYTES {
        return Err(format!("bitmap dimensions out of range ({width_dots}x{height_dots})"));
    }

    let raw = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("bitmap data is not valid base64: {e}"))?;

    if raw.len() != expected {
        return Err(format!(
            "bitmap data length {} does not match {width_dots}x{height_dots} ({expected} bytes)",
            raw.len()
        ));
    }

    out.extend_from_slice(
        format!("BITMAP {x},{y},{bytes_per_row},{height_dots},0,").as_bytes(),
    );
    // The page packs 1 = black (the natural reading of a canvas). TSPL's BITMAP
    // is the other way up: 0 = print. Invert here, once, at the wire.
    out.extend(raw.iter().map(|byte| !byte));
    out.extend_from_slice(b"\r\n");
    Ok(())
}

fn encode_barcode(
    out: &mut Vec<u8>,
    x: u32,
    y: u32,
    symbology: &str,
    value: &str,
    height_dots: u32,
    module: u8,
) -> Result<(), String> {
    let kind = tspl_symbology(symbology)?;
    let mut payload = tspl_quote(value);

    if kind == "EAN13" {
        // The firmware computes and appends the 13th (check) digit. Our codes
        // are stored as full valid EAN-13s, so hand it the first 12 — passing
        // 13 makes some firmwares print a 14-digit symbol that scans wrong.
        if payload.len() == 13 && payload.chars().all(|c| c.is_ascii_digit()) {
            payload.truncate(12);
        }
        if payload.len() != 12 || !payload.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("\"{payload}\" is not an EAN-13 payload"));
        }
    } else if payload.is_empty() || payload.len() > 64 {
        return Err("barcode payload must be 1-64 characters".into());
    }

    let module = module.clamp(1, 4);
    // `1` after rotation = print the human-readable digits under the bars —
    // the till types them in by hand the day the scanner's cable breaks.
    out.extend_from_slice(
        format!(
            "BARCODE {x},{y},\"{kind}\",{height_dots},1,0,{module},{module},\"{payload}\"\r\n"
        )
        .as_bytes(),
    );
    Ok(())
}

fn encode_text(out: &mut Vec<u8>, x: u32, y: u32, value: &str, font: &str, scale: u8) {
    let ascii: String = tspl_quote(value)
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect();
    let font = if matches!(font, "1" | "2" | "3" | "4" | "5") {
        font
    } else {
        "3"
    };
    let scale = scale.clamp(1, 4);

    out.extend_from_slice(
        format!("TEXT {x},{y},\"{font}\",0,{scale},{scale},\"{ascii}\"\r\n").as_bytes(),
    );
}

/// A full TSPL document for one job: stock header, the ops, PRINT with copies.
fn encode_job(job: &LabelJob, config: &LabelPrinterConfig) -> Result<Vec<u8>, String> {
    if job.ops.is_empty() {
        return Err("label has no content".into());
    }

    let mut out: Vec<u8> = Vec::with_capacity(1024);

    // Stock geometry every job, not once per session: the printer keeps state
    // per connection, and jobs must not depend on who printed before them.
    //
    // The tail of the header is what the vendor's own test tool sends, and it
    // is not decoration. Without `SET TEAR ON` the printer stops with the
    // label's last rows still under the head; the cashier tears at the bar,
    // the remainder stays attached to the strip, and the next job begins
    // mid-label — one sticker cut in two, every print eating two labels.
    // `REFERENCE 0,0` + `OFFSET 0` pin the page origin to the label's edge,
    // and `DIRECTION 0,0` matches the vendor default so the label leaves the
    // printer reading the right way up.
    out.extend_from_slice(
        format!(
            "SIZE {:.1} mm,{:.1} mm\r\nGAP {:.1} mm,0 mm\r\nDIRECTION 0,0\r\nREFERENCE 0,0\r\nOFFSET 0 mm\r\nSET TEAR ON\r\nDENSITY {}\r\nCLS\r\n",
            config.width_mm, config.height_mm, config.gap_mm, config.density
        )
        .as_bytes(),
    );

    for op in &job.ops {
        match op {
            LabelOp::Bitmap {
                x,
                y,
                width_dots,
                height_dots,
                data,
            } => encode_bitmap(&mut out, *x, *y, *width_dots, *height_dots, data)?,
            LabelOp::Barcode {
                x,
                y,
                symbology,
                value,
                height_dots,
                module,
            } => encode_barcode(&mut out, *x, *y, symbology, value, *height_dots, *module)?,
            LabelOp::Text {
                x,
                y,
                value,
                font,
                scale,
            } => encode_text(&mut out, *x, *y, value, font, *scale),
        }
    }

    let copies = job.copies.clamp(1, 50);
    out.extend_from_slice(format!("PRINT 1,{copies}\r\n").as_bytes());
    Ok(out)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn print_jobs(app: AppHandle, jobs: Vec<LabelJob>) -> Result<(), String> {
    let config = load_config(&app).ok_or("no label printer configured")?;
    config.validate()?;

    if jobs.is_empty() {
        return Err("nothing to print".into());
    }
    if jobs.len() > MAX_LABELS_PER_JOB {
        return Err(format!(
            "too many labels in one run ({}, max {MAX_LABELS_PER_JOB})",
            jobs.len()
        ));
    }

    // One byte stream for the whole run: per-label connections would slow a
    // 50-sticker batch to a crawl on the spooler path.
    let mut bytes: Vec<u8> = Vec::with_capacity(4096);
    for job in &jobs {
        bytes.extend(encode_job(job, &config)?);
    }

    tauri::async_runtime::spawn_blocking(move || {
        send_bytes(
            &config.interface,
            &config.address,
            config.baud,
            &bytes,
            "Orcaa labels",
        )
    })
    .await
    .map_err(|e| format!("label print task failed: {e}"))?
}

/// Prints label jobs built by the page (see the POS labels dialog).
#[tauri::command]
pub async fn shell_label_print(app: AppHandle, jobs: Vec<LabelJob>) -> Result<(), String> {
    print_jobs(app, jobs).await
}

/// One sticker that proves the stock geometry and the scanner in one pass:
/// a frame-corner text, an EAN-13, and the digits — if this scans, sales scan.
#[tauri::command]
pub async fn shell_label_test_print(app: AppHandle) -> Result<(), String> {
    let config = load_config(&app).ok_or("no label printer configured")?;
    let dots_per_mm = config.dpmm as u32;
    let width = (config.width_mm as u32) * dots_per_mm;
    // Bars take what is left above a 3mm bottom margin, capped at 12mm.
    let height = (config.height_mm as u32) * dots_per_mm;
    let bar_height = height
        .saturating_sub(24 + 24 + 8 + 24 + 3 * dots_per_mm)
        .clamp(6 * dots_per_mm, 12 * dots_per_mm);

    // 95 modules × module 2 — centred EAN-13 (Orcaa's in-house 02 prefix).
    let barcode_width = 95 * 2;
    let x = width.saturating_sub(barcode_width) / 2;

    let job = LabelJob {
        ops: vec![
            LabelOp::Text {
                x: 8,
                y: 24,
                value: "Orcaa".into(),
                font: "3".into(),
                scale: 1,
            },
            LabelOp::Barcode {
                x,
                y: 24 + 24 + 8,
                symbology: "ean13".into(),
                value: "0200000000008".into(),
                height_dots: bar_height,
                module: 2,
            },
        ],
        copies: 1,
    };

    print_jobs(app, vec![job]).await
}

/// Runs the printer's gap-sensor calibration on the loaded stock. Feeds a few
/// labels while it measures. This is the fix for "prints in the wrong place"
/// and "eats two labels per print" when the stock was changed or the printer
/// is fresh out of the box — nobody should have to know the FEED-button dance.
#[tauri::command]
pub async fn shell_label_calibrate(app: AppHandle) -> Result<(), String> {
    let config = load_config(&app).ok_or("no label printer configured")?;
    config.validate()?;

    let bytes = format!(
        "SIZE {:.1} mm,{:.1} mm\r\nGAP {:.1} mm,0 mm\r\nGAPDETECT\r\n",
        config.width_mm, config.height_mm, config.gap_mm
    )
    .into_bytes();

    tauri::async_runtime::spawn_blocking(move || {
        send_bytes(
            &config.interface,
            &config.address,
            config.baud,
            &bytes,
            "Orcaa label calibration",
        )
    })
    .await
    .map_err(|e| format!("calibration task failed: {e}"))?
}

#[tauri::command]
pub fn shell_label_printer_get(app: AppHandle) -> Option<LabelPrinterConfig> {
    load_config(&app)
}

#[tauri::command]
pub fn shell_label_printer_set(
    app: AppHandle,
    config: Option<LabelPrinterConfig>,
) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    match config {
        Some(config) => {
            config.validate()?;
            store.set(
                STORE_KEY_LABEL_PRINTER,
                serde_json::to_value(&config).map_err(|e| e.to_string())?,
            );
        }
        None => {
            store.delete(STORE_KEY_LABEL_PRINTER);
        }
    }
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// Finds the counter's LABEL printer and saves it — the same zero-setup promise
/// as the receipt printer, gated on the stricter label-printer markers so a
/// receipt Xprinter is never claimed as label stock (and vice versa).
///
/// Stock size can't be detected (the printer doesn't know what roll was
/// loaded); 40×30mm is the default the market sells, and the settings card is
/// where a different roll gets set.
#[tauri::command]
pub fn shell_label_printer_autodetect(app: AppHandle) -> Result<Option<LabelPrinterConfig>, String> {
    if let Some(existing) = load_config(&app) {
        return Ok(Some(existing));
    }

    #[cfg(windows)]
    {
        let printers = crate::spooler::list_printers()?;
        let Some(found) = printers
            .iter()
            .find(|printer| printer.is_label && !printer.is_virtual)
        else {
            return Ok(None);
        };

        let config = LabelPrinterConfig {
            interface: "printer".to_string(),
            address: found.name.clone(),
            baud: default_baud(),
            width_mm: 40.0,
            height_mm: 30.0,
            gap_mm: default_gap(),
            dpmm: default_dpmm(),
            density: default_density(),
        };

        config.validate()?;
        shell_label_printer_set(app, Some(config.clone()))?;

        Ok(Some(config))
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LabelPrinterConfig {
        LabelPrinterConfig {
            interface: "printer".into(),
            address: "XP-365B".into(),
            baud: 9600,
            width_mm: 40.0,
            height_mm: 30.0,
            gap_mm: 2.0,
            dpmm: 8,
            density: 8,
        }
    }

    fn text_of(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).to_string()
    }

    #[test]
    fn a_job_declares_its_stock_before_printing() {
        let job = LabelJob {
            ops: vec![LabelOp::Text {
                x: 0,
                y: 0,
                value: "5.00".into(),
                font: "3".into(),
                scale: 1,
            }],
            copies: 3,
        };
        let tspl = text_of(&encode_job(&job, &config()).unwrap());

        assert!(tspl.starts_with("SIZE 40.0 mm,30.0 mm\r\nGAP 2.0 mm,0 mm\r\n"));
        // Without TEAR ON the label stops under the head and the next print
        // starts mid-sticker — this line is the two-labels-per-print fix.
        assert!(tspl.contains("SET TEAR ON\r\n"));
        assert!(tspl.contains("REFERENCE 0,0\r\n"));
        assert!(tspl.contains("CLS\r\n"));
        assert!(tspl.ends_with("PRINT 1,3\r\n"));
    }

    #[test]
    fn an_ean13_hands_the_firmware_twelve_digits() {
        let mut out = Vec::new();
        // Stored codes are full 13-digit EAN-13s; the check digit is the
        // firmware's to append or the symbol double-carries it.
        encode_barcode(&mut out, 10, 20, "ean13", "6221031492020", 80, 2).unwrap();
        let tspl = text_of(&out);

        assert!(tspl.contains("\"EAN13\""));
        assert!(tspl.contains("\"622103149202\""));
        assert!(!tspl.contains("6221031492020"));
    }

    #[test]
    fn an_alphanumeric_code_prints_as_code128_verbatim() {
        let mut out = Vec::new();
        encode_barcode(&mut out, 0, 0, "code128", "PEN-ROTO-01", 80, 2).unwrap();
        assert!(text_of(&out).contains("\"128\",80,1,0,2,2,\"PEN-ROTO-01\""));
    }

    #[test]
    fn a_non_ean_payload_is_refused_rather_than_misprinted() {
        let mut out = Vec::new();
        assert!(encode_barcode(&mut out, 0, 0, "ean13", "PEN-01", 80, 2).is_err());
        assert!(encode_barcode(&mut out, 0, 0, "qr", "x", 80, 2).is_err());
    }

    #[test]
    fn bitmap_bits_are_inverted_to_tspl_black() {
        let mut out = Vec::new();
        // 8x1 dots, all black as the page packs it (0xFF).
        let data = base64::engine::general_purpose::STANDARD.encode([0xFFu8]);
        encode_bitmap(&mut out, 0, 0, 8, 1, &data).unwrap();

        // Header, then the payload byte must be 0x00 — TSPL prints 0-bits.
        let header = b"BITMAP 0,0,1,1,0,";
        let pos = out
            .windows(header.len())
            .position(|w| w == header)
            .expect("bitmap header");
        assert_eq!(out[pos + header.len()], 0x00);
    }

    #[test]
    fn a_bitmap_that_lies_about_its_size_is_refused() {
        let mut out = Vec::new();
        let data = base64::engine::general_purpose::STANDARD.encode([0xFFu8; 3]);
        assert!(encode_bitmap(&mut out, 0, 0, 8, 1, &data).is_err());
    }

    #[test]
    fn quotes_cannot_break_out_of_a_tspl_string() {
        let mut out = Vec::new();
        encode_text(&mut out, 0, 0, "5\"00\r\nPRINT", "3", 1);
        let tspl = text_of(&out);
        assert!(!tspl.contains("\r\nPRINT 1"));
        assert!(tspl.contains("\"500PRINT\""));
    }

    #[test]
    fn a_bad_config_never_validates() {
        let mut cfg = config();
        cfg.width_mm = 300.0;
        assert!(cfg.validate().is_err());

        let mut cfg = config();
        cfg.interface = "usb".into();
        assert!(cfg.validate().is_err());

        let mut cfg = config();
        cfg.dpmm = 10;
        assert!(cfg.validate().is_err());
    }
}
