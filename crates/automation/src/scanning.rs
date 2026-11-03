//! Scanner tools: list scanners, and scan into a new document (`doc_create` from `scanner`).

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use pdfcraft_engine::scan::{self, ColorMode, Paper, Preset, ScanError, ScanSettings, Source};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

fn choice<T>(a: &Args, key: &str, parse: fn(&str) -> Option<T>, allowed: &str) -> Result<Option<T>> {
    match a.opt_str(key)? {
        None => Ok(None),
        Some(s) if s.len() > 128 => Err(bad(format!("{key} must be one of {allowed}"))),
        Some(s) => parse(s).map(Some).ok_or_else(|| bad(format!("unknown {key} {s:?} ({allowed})"))),
    }
}

impl Automation {
    /// The scan settings: the preset (default `color_document`), then any single overrides.
    pub(crate) fn scan_settings(&self, a: &Args) -> Result<ScanSettings> {
        let preset =
            choice(a, "preset", Preset::from_name, "bw_document, gray_document, color_document, color_photo")?.unwrap_or(Preset::ColorDocument);
        let mut s = preset.settings();
        if let Some(c) = choice(a, "color", ColorMode::from_name, "bw, gray, color")? {
            s.color = c;
        }
        if let Some(d) = a.opt_int("scan_dpi")? {
            if !scan::DPI_RANGE.contains(&(d.clamp(0, i64::from(u32::MAX)) as u32)) {
                return Err(bad(format!("scan_dpi must be between {} and {}", scan::DPI_RANGE.start(), scan::DPI_RANGE.end())));
            }
            s.dpi = d as u32;
        }
        if let Some(src) = choice(a, "source", Source::from_name, "flatbed, feeder, duplex")? {
            s.source = src;
        }
        if let Some(p) = choice(a, "paper", Paper::from_name, "letter, legal, a4, a5, full")? {
            s.paper = p;
        }
        Ok(s)
    }

    pub(crate) fn scanners(&self, a: &Args) -> Result<Value> {
        let wait = a.opt_num("wait")?.unwrap_or(3.0).clamp(0.0, 30.0);
        let list: Vec<Value> = scan::scanners(Duration::from_secs_f64(wait))
            .into_iter()
            .map(|s| json!({ "id": s.id, "name": s.name, "connection": s.backend.label() }))
            .collect();
        Ok(json!({ "count": list.len(), "scanners": list }))
    }

    /// Scan with the scanner named by `scanner`; the new document's name and bytes.
    pub(crate) fn scan_to_pdf(&self, a: &Args) -> Result<(String, std::sync::Arc<Vec<u8>>, Option<String>)> {
        if a.opt_bool("user_confirmed")? != Some(true) {
            return Err(bad("scanning operates physical hardware: obtain the user's explicit consent, then pass user_confirmed: true"));
        }
        // These fields are consumed after acquisition by doc_create. Validate them before
        // contacting hardware so a bad optional argument cannot waste a sheet or its image.
        if a.opt_str("name")?.is_some_and(|name| name.len() > 4096) {
            return Err(bad("scanner document names must be at most 4096 bytes"));
        }
        let _ = a.opt_bool("ocr")?;
        if a.opt_str("language")?.is_some_and(|language| language.len() > 64) {
            return Err(bad("OCR language codes must be at most 64 bytes"));
        }
        let id = a.opt_str("scanner")?.ok_or_else(|| bad("scanner is needed (an id from the scanners tool, or escl:<address>)"))?;
        let settings = self.scan_settings(a)?;
        let (pages, warning) = match scan::scan(id, &settings, &AtomicBool::new(false)) {
            Ok(pages) => (pages, None),
            Err(ScanError::Partial { pages, reason }) => (pages, Some(reason)),
            Err(error) => return Err(failed(error)),
        };
        let bytes = self.session.create_from_scan(&pages, "Scan").map_err(failed)?;
        Ok(("Scan.pdf".into(), bytes, warning))
    }
}
