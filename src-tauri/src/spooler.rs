//! Windows print spooler, RAW mode — the transport a USB thermal printer needs.
//!
//! `print.rs` can reach a printer over TCP:9100 or a serial port, and neither
//! exists on the printers most shops actually buy: a USB receipt printer shows
//! up as a Windows PRINT QUEUE and nothing else. Its driver page is a sheet
//! (an XP-80C reports 80x297mm), so printing a document through it feeds a
//! third of a metre of blank roll and scales the text down to nothing.
//!
//! RAW mode skips all of that. `StartDocPrinter` with datatype "RAW" hands our
//! ESC/POS bytes to the device untouched — no page, no scaling, no dialog, the
//! printer's own font, its own cut, its own drawer pulse. Exactly what the
//! network and serial paths already do, over the one wire the hardware has.
//!
//! Everything here is Windows-only; `print.rs` guards the call on other OSes.

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;

use serde::Serialize;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Graphics::Printing::{
    ClosePrinter, EndDocPrinter, EndPagePrinter, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW,
    StartDocPrinterW, StartPagePrinter, WritePrinter, DOC_INFO_1W, PRINTER_ENUM_CONNECTIONS,
    PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_2W,
};

/// One installed Windows printer, as the settings page needs to show it.
#[derive(Clone, Serialize)]
pub struct InstalledPrinter {
    pub name: String,
    pub driver: String,
    pub port: String,
    pub is_default: bool,
    /// "Microsoft Print to PDF", XPS, Fax… — a queue that produces a file, not
    /// paper. Never auto-picked, and flagged so the UI can sink it in the list.
    pub is_virtual: bool,
    /// Looks like a receipt printer (driver/model match). Drives auto-setup.
    pub is_thermal: bool,
    /// Looks like a die-cut LABEL printer (TSPL class). Separate from
    /// `is_thermal`: a shop with both must never get its receipt on sticker
    /// stock or its stickers on the till roll.
    pub is_label: bool,
    /// Roll width in millimetres when the model name states it (XP-80C -> 80).
    pub roll_width_mm: Option<u32>,
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

/// Reads a `*mut u16` the spooler handed back inside its own buffer.
unsafe fn from_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

/// Queues that write a file instead of driving a print head. Matched on the
/// driver AND the name, because the name is whatever the shop typed.
const VIRTUAL_MARKERS: [&str; 7] = [
    "xps document writer",
    "microsoft print to pdf",
    "print to pdf",
    "onenote",
    "fax",
    "adobe pdf",
    "pdf24",
];

/// Substrings that identify a thermal receipt printer. Mostly driver/model
/// names: the queue's display name is user-chosen (a shop may well call it
/// "فواتير") and tells us nothing, so detection leans on the driver string the
/// vendor's installer wrote.
const THERMAL_MARKERS: [&str; 18] = [
    "xp-", "xprinter", "pos-", "pos58", "pos80", "thermal", "receipt", "tm-t", "tm-u", "srp-",
    "tsp1", "tsp6", "tsp7", "zj-", "gprinter", "rongta", "snbc", "bixolon",
];

/// Substrings that identify a die-cut LABEL printer (TSPL class). Note the
/// overlap trap: Xprinter's LABEL models are XP-2xx/3xx/4xx/DT — they match
/// the receipt marker "xp-" too, so receipt detection must test these FIRST
/// and step aside. XP-58/80/Q (receipts) do not match any of these.
const LABEL_MARKERS: [&str; 16] = [
    "label",
    "xp-1",
    "xp-2",
    "xp-3",
    "xp-4",
    "xp-d",
    "tsc",
    "ttp-",
    "zdesigner",
    "zebra",
    "godex",
    "argox",
    "postek",
    "sato",
    "citizen cl",
    "brother ql",
];

fn matches_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// 80mm and 58mm are the only two rolls in the field, and the model name almost
/// always says which ("XP-80C", "POS58"). None when it does not.
fn roll_width_from(text: &str) -> Option<u32> {
    if text.contains("80") {
        Some(80)
    } else if text.contains("58") || text.contains("57") {
        Some(58)
    } else {
        None
    }
}

fn default_printer_name() -> String {
    let mut needed: u32 = 0;

    unsafe {
        // First call sizes the buffer; it always "fails" with the needed length.
        let _ = GetDefaultPrinterW(None, &mut needed);
        if needed == 0 {
            return String::new();
        }

        let mut buffer = vec![0u16; needed as usize];
        if !GetDefaultPrinterW(Some(PWSTR(buffer.as_mut_ptr())), &mut needed).as_bool() {
            return String::new();
        }

        from_wide(buffer.as_ptr())
    }
}

/// Every printer installed for this user, at the richest level (2) so the
/// driver name comes with it — detection is worthless without it.
pub fn list_printers() -> Result<Vec<InstalledPrinter>, String> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let mut needed: u32 = 0;
    let mut returned: u32 = 0;

    unsafe {
        // Sizing pass. A spooler with no printers reports 0 bytes needed.
        let _ = EnumPrintersW(flags, None, 2, None, &mut needed, &mut returned);
        if needed == 0 {
            return Ok(Vec::new());
        }

        let mut buffer = vec![0u8; needed as usize];
        EnumPrintersW(
            flags,
            None,
            2,
            Some(buffer.as_mut_slice()),
            &mut needed,
            &mut returned,
        )
        .map_err(|e| format!("can't list the installed printers: {e}"))?;

        let default = default_printer_name();
        let entries = std::slice::from_raw_parts(
            buffer.as_ptr() as *const PRINTER_INFO_2W,
            returned as usize,
        );

        Ok(entries
            .iter()
            .map(|info| {
                let name = from_wide(info.pPrinterName.0);
                let driver = from_wide(info.pDriverName.0);
                let port = from_wide(info.pPortName.0);
                let haystack = format!("{} {}", name, driver).to_lowercase();
                let is_label = matches_any(&haystack, &LABEL_MARKERS);

                InstalledPrinter {
                    is_default: !default.is_empty() && name == default,
                    is_virtual: matches_any(&haystack, &VIRTUAL_MARKERS),
                    // A label model outranks the looser receipt match ("xp-"
                    // catches both Xprinter families).
                    is_thermal: !is_label && matches_any(&haystack, &THERMAL_MARKERS),
                    is_label,
                    roll_width_mm: roll_width_from(&driver).or_else(|| roll_width_from(&name)),
                    name,
                    driver,
                    port,
                }
            })
            .collect())
    }
}

/// The printer to use when nobody has configured one — the whole point being
/// that a counter PC should not need a settings visit at all.
///
/// A detected thermal printer wins (the Windows-default one first, if a shop
/// has several). Deliberately NOT "the Windows default printer" as a fallback:
/// that is usually an office laser or a PDF writer, and quietly sending every
/// sale's receipt to the wrong device is worse than printing none.
pub fn detect_receipt_printer() -> Option<InstalledPrinter> {
    let printers = list_printers().ok()?;

    let thermal: Vec<&InstalledPrinter> = printers
        .iter()
        .filter(|printer| printer.is_thermal && !printer.is_virtual)
        .collect();

    thermal
        .iter()
        .find(|printer| printer.is_default)
        .or_else(|| thermal.first())
        .map(|printer| (*printer).clone())
}

/// Pushes raw bytes at a queue in RAW datatype — no driver rendering at all.
pub fn send_raw(printer: &str, bytes: &[u8], document: &str) -> Result<(), String> {
    let mut name = wide(printer);
    let mut doc_name = wide(document);
    let mut datatype = wide("RAW");
    let mut handle = PRINTER_HANDLE::default();

    unsafe {
        OpenPrinterW(PCWSTR(name.as_mut_ptr()), &mut handle, None)
            .map_err(|e| format!("can't open the printer \"{printer}\": {e}"))?;

        let info = DOC_INFO_1W {
            pDocName: PWSTR(doc_name.as_mut_ptr()),
            pOutputFile: PWSTR::null(),
            pDatatype: PWSTR(datatype.as_mut_ptr()),
        };

        // From here on every exit must still ClosePrinter, or the queue leaks a
        // handle per failed sale until the spooler service is restarted.
        let job = StartDocPrinterW(handle, 1, &info);
        if job == 0 {
            let error = windows::core::Error::from_win32();
            let _ = ClosePrinter(handle);
            return Err(format!(
                "the printer \"{printer}\" refused the job: {error}"
            ));
        }

        let result = write_all(handle, bytes);

        let _ = EndPagePrinter(handle);
        let _ = EndDocPrinter(handle);
        let _ = ClosePrinter(handle);

        result
    }
}

unsafe fn write_all(handle: PRINTER_HANDLE, bytes: &[u8]) -> Result<(), String> {
    StartPagePrinter(handle)
        .ok()
        .map_err(|e| format!("printing failed before the first byte: {e}"))?;

    let mut offset = 0usize;

    // WritePrinter is free to accept less than it was handed; a receipt that
    // stops halfway is worse than one that never printed, so loop until done.
    while offset < bytes.len() {
        let mut written: u32 = 0;
        let chunk = &bytes[offset..];

        WritePrinter(
            handle,
            chunk.as_ptr() as *const c_void,
            chunk.len() as u32,
            &mut written,
        )
        .ok()
        .map_err(|e| format!("printing failed mid-receipt: {e}"))?;

        if written == 0 {
            return Err("the printer stopped accepting the receipt".into());
        }

        offset += written as usize;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pdf_and_xps_writers_are_never_mistaken_for_a_printer() {
        for name in [
            "microsoft xps document writer",
            "microsoft print to pdf",
            "fax",
            "onenote (desktop)",
        ] {
            assert!(matches_any(name, &VIRTUAL_MARKERS), "{name}");
            assert!(!matches_any(name, &THERMAL_MARKERS), "{name}");
        }
    }

    #[test]
    fn a_receipt_printer_is_recognised_by_its_driver_not_its_name() {
        // The queue name is whatever the shop typed — Arabic, a person's name,
        // anything. The driver string is what the vendor installed.
        let haystack = "فواتير xp-80c".to_lowercase();
        assert!(matches_any(&haystack, &THERMAL_MARKERS));
        assert!(!matches_any(&haystack, &VIRTUAL_MARKERS));
    }

    #[test]
    fn an_office_laser_is_not_treated_as_a_receipt_printer() {
        for name in [
            "hp laserjet mfp m428",
            "canon ir-adv c3520",
            "brother dcp-l2540",
        ] {
            assert!(!matches_any(name, &THERMAL_MARKERS), "{name}");
        }
    }

    #[test]
    fn label_models_are_never_mistaken_for_receipt_printers() {
        // Same vendor, same "xp-" prefix, different stock entirely.
        for label in [
            "xprinter xp-365b",
            "xprinter xp-237b",
            "tsc te244",
            "zdesigner gk420d",
        ] {
            assert!(matches_any(label, &LABEL_MARKERS), "{label}");
        }
        // The receipt family must NOT match the label markers.
        for receipt in ["xprinter xp-80c", "xprinter xp-q807k", "xp-58iih"] {
            assert!(!matches_any(receipt, &LABEL_MARKERS), "{receipt}");
            assert!(matches_any(receipt, &THERMAL_MARKERS), "{receipt}");
        }
    }

    #[test]
    fn the_roll_width_is_read_from_the_model_name() {
        assert_eq!(roll_width_from("xp-80c"), Some(80));
        assert_eq!(roll_width_from("pos58 printer"), Some(58));
        assert_eq!(roll_width_from("xprinter"), None);
    }
}
