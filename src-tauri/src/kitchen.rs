//! Silent kitchen-ticket (KOT) printing — ESC/POS, one printer per station.
//!
//! The third printer family after receipts (`print.rs`) and labels
//! (`label.rs`), and byte-for-byte the first one's protocol: a kitchen ticket
//! is a receipt-class document on receipt-class hardware, so the ops, the
//! encoder and the transport are all borrowed from `print.rs` rather than
//! re-invented. What earns this module its own file is ROUTING: a restaurant
//! has a hot kitchen, a bar, a bakery — each with its own printer — so the
//! store holds a MAP of station id (a backend uuid, or the literal
//! `"default"`) → printer config instead of the single config the other two
//! families keep.
//!
//! `"default"` is the one-printer kitchen everybody starts with: a ticket for
//! a station with no printer of its own falls back to the `"default"` entry,
//! so routing can be adopted station by station without a flag day.
//!
//! Two receipt ops never ride a KOT: `qr` (nothing to scan on a ticket rail)
//! and `drawer` (kitchens have no till — and a page bug must never be able to
//! pop a cash drawer from the pass). Both are dropped here at the wire.

use std::collections::HashMap;

use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

use crate::print::{encode_receipt, send_bytes, PrinterConfig, ReceiptOp};
use crate::STORE_FILE;

const STORE_KEY_KITCHEN_PRINTERS: &str = "pos_kitchen_printers";

/// The station key a ticket falls back to when its own station has no printer.
const DEFAULT_STATION: &str = "default";

/// Hard ceiling on stored stations — a runaway settings loop must not bloat
/// the store file. No kitchen has 32 printers.
const MAX_STATIONS: usize = 32;

// ---------------------------------------------------------------------------
// Station map
// ---------------------------------------------------------------------------

/// Station ids are backend uuids (or `"default"`) — short opaque strings. The
/// bound is a sanity check on the payload, not a format check.
pub(crate) fn normalize_station(station_id: &str) -> Result<String, String> {
    let station = station_id.trim();
    if station.is_empty() {
        return Err("station id is empty".into());
    }
    if station.len() > 64 {
        return Err("station id is too long".into());
    }
    Ok(station.to_string())
}

pub(crate) fn load_map(app: &AppHandle) -> HashMap<String, PrinterConfig> {
    let Ok(store) = app.store(STORE_FILE) else {
        return HashMap::new();
    };
    store
        .get(STORE_KEY_KITCHEN_PRINTERS)
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn save_map(app: &AppHandle, map: &HashMap<String, PrinterConfig>) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    if map.is_empty() {
        store.delete(STORE_KEY_KITCHEN_PRINTERS);
    } else {
        store.set(
            STORE_KEY_KITCHEN_PRINTERS,
            serde_json::to_value(map).map_err(|e| e.to_string())?,
        );
    }
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// The station's own printer, else the `"default"` entry, else a clear error
/// the page can surface in its own (localized) toast.
pub(crate) fn resolve_config(
    map: &HashMap<String, PrinterConfig>,
    station: &str,
) -> Result<PrinterConfig, String> {
    map.get(station)
        .or_else(|| map.get(DEFAULT_STATION))
        .cloned()
        .ok_or_else(|| {
            format!(
                "no kitchen printer configured for station \"{station}\" \
                 and no \"default\" station to fall back to"
            )
        })
}

/// Drops the two ops a kitchen ticket must never carry. `Drawer` is already
/// gated on the config's `drawer_kick`, but a copied receipt config could have
/// it on — the pass gets no say over the till either way.
fn sanitize_kot_ops(ops: Vec<ReceiptOp>) -> Vec<ReceiptOp> {
    ops.into_iter()
        .filter(|op| !matches!(op, ReceiptOp::Qr { .. } | ReceiptOp::Drawer))
        .collect()
}

// ---------------------------------------------------------------------------
// Printing
// ---------------------------------------------------------------------------

async fn print_ticket(
    app: AppHandle,
    station_id: String,
    ops: Vec<ReceiptOp>,
) -> Result<(), String> {
    let station = normalize_station(&station_id)?;
    let config = resolve_config(&load_map(&app), &station)?;
    config.validate()?;

    let ops = sanitize_kot_ops(ops);
    if ops.is_empty() {
        return Err("nothing to print".into());
    }
    // `encode_receipt` owns the byte protocol AND the MAX_OPS ceiling.
    let bytes = encode_receipt(&ops, &config)?;

    // Blocking socket/serial I/O has no business on the async runtime's core.
    tauri::async_runtime::spawn_blocking(move || {
        send_bytes(
            &config.interface,
            &config.address,
            config.baud,
            &bytes,
            "Orcaa kitchen ticket",
        )
    })
    .await
    .map_err(|e| format!("kitchen print task failed: {e}"))?
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Prints a kitchen ticket built by the page for one station. Falls back to
/// the `"default"` station's printer when the station has none of its own.
#[tauri::command]
pub async fn shell_kot_print(
    app: AppHandle,
    station_id: String,
    ops: Vec<ReceiptOp>,
) -> Result<(), String> {
    print_ticket(app, station_id, ops).await
}

/// A short ticket that answers "does THIS station's routing print where the
/// cooks are standing" — names the station so two test tickets fired at two
/// stations can be told apart at the printers.
#[tauri::command]
pub async fn shell_kitchen_test_print(app: AppHandle, station_id: String) -> Result<(), String> {
    let station = normalize_station(&station_id)?;
    let ops = vec![
        ReceiptOp::Text {
            value: "Orcaa".into(),
            align: Some("center".into()),
            bold: true,
            big: true,
        },
        ReceiptOp::Text {
            value: "Kitchen printer test".into(),
            align: Some("center".into()),
            bold: false,
            big: false,
        },
        ReceiptOp::Hr,
        ReceiptOp::Pair {
            left: "Station".into(),
            right: station.clone(),
        },
        ReceiptOp::Text {
            value: "اختبار طابعة المطبخ".into(),
            align: Some("center".into()),
            bold: false,
            big: false,
        },
        ReceiptOp::Hr,
        ReceiptOp::Feed { lines: 2 },
        ReceiptOp::Cut,
    ];
    print_ticket(app, station, ops).await
}

/// The whole station → printer map, for the settings page.
#[tauri::command]
pub fn shell_kitchen_printers_get(app: AppHandle) -> HashMap<String, PrinterConfig> {
    load_map(&app)
}

/// Saves (or clears, with `None`) ONE station's printer. Validated here so a
/// broken config can never be persisted and then fail every ticket.
#[tauri::command]
pub fn shell_kitchen_printer_set(
    app: AppHandle,
    station_id: String,
    config: Option<PrinterConfig>,
) -> Result<(), String> {
    let station = normalize_station(&station_id)?;
    let mut map = load_map(&app);
    match config {
        Some(config) => {
            config.validate()?;
            if !map.contains_key(&station) && map.len() >= MAX_STATIONS {
                return Err(format!("too many kitchen stations (max {MAX_STATIONS})"));
            }
            map.insert(station, config);
        }
        None => {
            map.remove(&station);
        }
    }
    save_map(&app, &map)
}

/// Finds a thermal printer and saves it under THIS station's key — same
/// zero-setup promise as the other two families, same "never overwrites an
/// existing choice" rule.
///
/// Detection reuses the receipt-class matcher (`detect_receipt_printer`):
/// kitchen printers ARE receipt-class ESC/POS hardware, and the label markers
/// would find sticker stock instead. Note the flip side: on a counter PC the
/// first thermal printer found is usually the RECEIPT printer, so this is
/// wired to a deliberate button in settings — never called silently on the
/// print path, where claiming the till's printer would double-print every
/// sale. A networked kitchen printer can't be detected at all (it is not a
/// Windows queue) and is configured by hand.
#[tauri::command]
pub fn shell_kitchen_printer_autodetect(
    app: AppHandle,
    station_id: String,
) -> Result<Option<PrinterConfig>, String> {
    let station = normalize_station(&station_id)?;
    if let Some(existing) = load_map(&app).remove(&station) {
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
            baud: 9600,
            // Same roll heuristic as the receipt autodetect: 80mm is the safer
            // guess — narrow on wide paper beats wrapping every line.
            width: if found.roll_width_mm == Some(58) { 32 } else { 48 },
            codepage: None,
            encoding: "cp1256".to_string(),
            // Kitchens have no till; a KOT must never be able to pop one.
            drawer_kick: false,
            text_mode: "codepage".to_string(),
        };

        config.validate()?;
        shell_kitchen_printer_set(app, station, Some(config.clone()))?;

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

    fn config(address: &str) -> PrinterConfig {
        PrinterConfig {
            interface: "network".into(),
            address: address.into(),
            baud: 9600,
            width: 48,
            codepage: None,
            encoding: "cp1256".into(),
            drawer_kick: false,
            text_mode: "codepage".to_string(),
        }
    }

    #[test]
    fn a_station_resolves_its_own_printer_before_the_default() {
        let mut map = HashMap::new();
        map.insert(DEFAULT_STATION.to_string(), config("192.168.1.60"));
        map.insert("bar-uuid".to_string(), config("192.168.1.61"));

        assert_eq!(
            resolve_config(&map, "bar-uuid").unwrap().address,
            "192.168.1.61"
        );
    }

    #[test]
    fn a_station_without_a_printer_falls_back_to_the_default() {
        let mut map = HashMap::new();
        map.insert(DEFAULT_STATION.to_string(), config("192.168.1.60"));

        assert_eq!(
            resolve_config(&map, "bakery-uuid").unwrap().address,
            "192.168.1.60"
        );
    }

    #[test]
    fn an_unconfigured_kitchen_is_a_clear_error() {
        // A match, not `unwrap_err`: `PrinterConfig` derives no `Debug`.
        let error = match resolve_config(&HashMap::new(), "bar-uuid") {
            Err(error) => error,
            Ok(_) => panic!("an unconfigured kitchen must not resolve"),
        };
        assert!(error.contains("bar-uuid"), "the error must name the station");
        assert!(error.contains("default"), "and mention the missing fallback");
    }

    #[test]
    fn a_kot_never_carries_qr_or_drawer_ops() {
        let ops = vec![
            ReceiptOp::Text {
                value: "2x Koshary".into(),
                align: None,
                bold: true,
                big: false,
            },
            ReceiptOp::Qr {
                value: "https://orcaa.cloud".into(),
            },
            ReceiptOp::Drawer,
            ReceiptOp::Cut,
        ];

        let kept = sanitize_kot_ops(ops);
        assert_eq!(kept.len(), 2);
        assert!(kept
            .iter()
            .all(|op| !matches!(op, ReceiptOp::Qr { .. } | ReceiptOp::Drawer)));
    }

    #[test]
    fn a_blank_station_id_is_refused() {
        assert!(normalize_station("   ").is_err());
        assert!(normalize_station(&"x".repeat(65)).is_err());
        assert_eq!(normalize_station(" default ").unwrap(), "default");
    }
}
