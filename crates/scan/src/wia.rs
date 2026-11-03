//! Windows Image Acquisition through winsafe's safe COM automation API.
//!
//! Installed USB and network/WSD scanner drivers share this interface. COM objects stay on
//! the calling worker thread. Transfer is synchronous inside the driver: cancellation is
//! checked before connecting and between pages, never by terminating a driver thread.
//! Protocol/identifiers: Microsoft's WIA Automation and WIA property documentation.

use crate::ScanError;

/// Translate documented WIA HRESULTs without depending on the machine's message language.
pub(crate) fn error_code(code: u32) -> Option<ScanError> {
    match code {
        0x8021_0002 => Some(ScanError::Jammed),
        0x8021_0003 => Some(ScanError::FeederEmpty),
        0x8021_0006 | 0x8021_000d => Some(ScanError::Busy),
        0x8021_0016 => Some(ScanError::CoverOpen),
        0x8021_0005 | 0x8021_000a | 0x8021_0015 => Some(ScanError::NotFound("check the scanner's power, connection and WIA driver".into())),
        _ => None,
    }
}

#[cfg(windows)]
mod native {
    use crate::{Backend, ColorMode, MAX_PAGE_BYTES, MAX_PAGES, ScanError, ScanSettings, ScannedPage, Scanner, Source};
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use winsafe::{self as w, IDispatch, Variant, co, prelude::*};

    const BMP: &str = "{B96B3CAB-0728-11D3-9D7B-0000F81EF32E}";
    const FLATBED: &str = "{FB607B1F-43F3-488B-855B-FB703EC342A6}";
    const FEEDER: &str = "{FE131934-F84C-42AD-8DA4-6129CDDD7288}";

    fn failed(message: impl std::fmt::Display) -> ScanError {
        ScanError::Failed(crate::tidy(&message.to_string()))
    }

    fn com_error(error: Box<dyn std::error::Error + Send + Sync>) -> ScanError {
        let code = error.downcast_ref::<w::EXCEPINFO>().map(|e| e.scode as u32).or_else(|| error.downcast_ref::<co::HRESULT>().map(|e| e.raw()));
        code.and_then(super::error_code).unwrap_or_else(|| failed(error))
    }

    fn object(value: Variant) -> Result<IDispatch, ScanError> {
        match value {
            Variant::Dispatch(object) => Ok(object),
            Variant::Unknown(object) => object.QueryInterface::<IDispatch>().map_err(failed),
            _ => Err(failed("the WIA driver returned an object of the wrong type")),
        }
    }

    fn get(object: &IDispatch, name: &str) -> Result<Variant, ScanError> {
        object.invoke_get(name, &[]).map_err(com_error)
    }

    fn child(object: &IDispatch, name: &str) -> Result<IDispatch, ScanError> {
        self::object(get(object, name)?)
    }

    fn integer(value: Variant) -> Result<i32, ScanError> {
        match value {
            Variant::I4(n) => Ok(n),
            Variant::I2(n) => Ok(i32::from(n)),
            Variant::UI4(n) => i32::try_from(n).map_err(failed),
            Variant::UI2(n) => Ok(i32::from(n)),
            _ => Err(failed("the WIA driver returned a number of the wrong type")),
        }
    }

    fn string(value: Variant) -> Result<String, ScanError> {
        match value {
            Variant::Bstr(s) => Ok(s),
            _ => Err(failed("the WIA driver returned text of the wrong type")),
        }
    }

    fn count(collection: &IDispatch, cap: i32) -> Result<i32, ScanError> {
        let n = integer(get(collection, "Count")?)?;
        if !(0..=cap).contains(&n) {
            return Err(failed("the WIA driver returned too many items"));
        }
        Ok(n)
    }

    fn item(collection: &IDispatch, key: &Variant) -> Result<IDispatch, ScanError> {
        object(collection.invoke_get("Item", &[key]).map_err(com_error)?)
    }

    fn property(object: &IDispatch, id: i32) -> Result<IDispatch, ScanError> {
        let properties = child(object, "Properties")?;
        // Some automation drivers accept decimal property IDs as names, others expose
        // only collection positions. Verify the ID and fall back to enumeration, never
        // confuse a collection index with a WIA property ID or depend on translated names.
        if let Ok(property) = item(&properties, &Variant::Bstr(id.to_string()))
            && get(&property, "PropertyID").and_then(integer).ok() == Some(id)
        {
            return Ok(property);
        }
        for index in 1..=count(&properties, 1024)? {
            let property = item(&properties, &Variant::I4(index))?;
            if get(&property, "PropertyID").and_then(integer).ok() == Some(id) {
                return Ok(property);
            }
        }
        Err(failed(format!("the scanner driver does not provide WIA property {id}; update its scanner driver")))
    }

    fn text(object: &IDispatch, id: i32) -> Result<String, ScanError> {
        string(get(&property(object, id)?, "Value")?)
    }

    fn set(object: &IDispatch, id: i32, value: i32) -> Result<(), ScanError> {
        property(object, id)?.invoke_put("Value", &Variant::I4(value)).map_err(com_error)?;
        Ok(())
    }

    fn manager() -> Result<IDispatch, ScanError> {
        w::CoCreateInstance::<IDispatch>(&w::CLSIDFromProgID("WIA.DeviceManager").map_err(failed)?, None::<&w::IUnknown>, co::CLSCTX::INPROC_SERVER)
            .map_err(failed)
    }

    pub fn scanners() -> Result<Vec<Scanner>, ScanError> {
        let _apartment = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE).map_err(failed)?;
        let infos = child(&manager()?, "DeviceInfos")?;
        let mut found = Vec::new();
        for index in 1..=count(&infos, 256)? {
            let Ok(info) = item(&infos, &Variant::I4(index)) else {
                continue;
            };
            if get(&info, "Type").and_then(integer).ok() != Some(1) {
                continue;
            }
            let Ok(id) = get(&info, "DeviceID").and_then(string) else {
                continue;
            };
            if id.is_empty() || id.len() > 4096 {
                continue;
            }
            let name = text(&info, 7).unwrap_or_else(|_| "Windows scanner".into());
            found.push(Scanner { id: format!("wia:{id}"), name: crate::tidy(&name), backend: Backend::Wia });
        }
        Ok(found)
    }

    fn connect(id: &str) -> Result<IDispatch, ScanError> {
        let infos = child(&manager()?, "DeviceInfos")?;
        for index in 1..=count(&infos, 256)? {
            let info = item(&infos, &Variant::I4(index))?;
            if integer(get(&info, "Type")?)? == 1 && string(get(&info, "DeviceID")?)? == id {
                return object(info.invoke_method("Connect", &[]).map_err(com_error)?);
            }
        }
        Err(ScanError::NotFound("install the manufacturer's WIA scanner driver or add the network scanner by IP address".into()))
    }

    fn source(device: &IDispatch, source: Source) -> Result<IDispatch, ScanError> {
        let items = child(device, "Items")?;
        let mut legacy = None;
        let desired = if source.is_feeder() { FEEDER } else { FLATBED };
        for index in 1..=count(&items, 256)? {
            let item = item(&items, &Variant::I4(index))?;
            match text(&item, 4125) {
                Ok(category) if category.eq_ignore_ascii_case(desired) => {
                    if source == Source::FeederDuplex {
                        set(&item, 3088, 5)?;
                    } else if source == Source::Feeder {
                        let _ = set(&item, 3088, 1);
                    }
                    return Ok(item);
                }
                Err(_) if legacy.is_none() => legacy = Some(item),
                _ => {}
            }
        }
        // WIA 1.x drivers use one transfer item and choose the source on the root device.
        if let Some(item) = legacy {
            let selection = match source {
                Source::Flatbed => 2,
                Source::Feeder => 1,
                Source::FeederDuplex => 5,
            };
            if let Err(error) = set(device, 3088, selection) {
                // A flatbed-only driver may omit this property; feeder/duplex must never silently scan glass.
                if source.is_feeder() {
                    return Err(error);
                }
            }
            return Ok(item);
        }
        Err(ScanError::NoSource(if source.is_feeder() { "document feeder" } else { "flatbed" }))
    }

    fn resolution(item: &IDispatch, id: i32, wanted: u32) -> Result<i32, ScanError> {
        let prop = property(item, id)?;
        let mut wanted = i32::try_from(wanted).map_err(failed)?;
        match integer(get(&prop, "SubType")?)? {
            1 => {
                let min = integer(get(&prop, "SubTypeMin")?)?.max(1);
                let max = integer(get(&prop, "SubTypeMax")?)?.max(min);
                let step = integer(get(&prop, "SubTypeStep")?)?.max(1);
                wanted = min.saturating_add(wanted.clamp(min, max).saturating_sub(min) / step * step);
            }
            2 => {
                let values = child(&prop, "SubTypeValues")?;
                let mut supported = Vec::new();
                for index in 1..=count(&values, 256)? {
                    if let Ok(n) = values.invoke_get("Item", &[&Variant::I4(index)]).map_err(com_error).and_then(integer)
                        && let Ok(n) = u32::try_from(n)
                    {
                        supported.push(n);
                    }
                }
                if let Some(n) = crate::closest_dpi(&supported, wanted as u32) {
                    wanted = i32::try_from(n).map_err(failed)?;
                }
            }
            _ => {}
        }
        prop.invoke_put("Value", &Variant::I4(wanted)).map_err(com_error)?;
        let actual = integer(get(&prop, "Value")?)?;
        if !(1..=1200).contains(&actual) {
            return Err(failed("the driver reported an invalid scan resolution"));
        }
        Ok(actual)
    }

    fn configure(item: &IDispatch, settings: &ScanSettings) -> Result<f64, ScanError> {
        let intent = match settings.color {
            ColorMode::Color => 1,
            ColorMode::Gray => 2,
            ColorMode::BlackWhite => 4,
        };
        set(item, 6146, intent)?;
        let original = [6151, 6152].map(|id| property(item, id).and_then(|p| get(&p, "Value")).and_then(integer).ok());
        let original_dpi = property(item, 6147).and_then(|p| get(&p, "Value")).and_then(integer).ok().filter(|n| *n > 0);
        // Some drivers validate extents when changing DPI: shrink before changing density.
        let _ = set(item, 6151, 1);
        let _ = set(item, 6152, 1);
        let x = resolution(item, 6147, settings.dpi)?;
        let y = resolution(item, 6148, x as u32)?;
        if x != y {
            return Err(failed("the driver selected unequal horizontal and vertical resolutions; choose another resolution"));
        }
        let mut dimensions = Vec::new();
        for ((id, mm), previous) in
            [(6151, settings.paper.size_mm().map(|s| s.0)), (6152, settings.paper.size_mm().map(|s| s.1))].into_iter().zip(original)
        {
            let prop = property(item, id)?;
            let max = get(&prop, "SubTypeMax")
                .and_then(integer)
                .ok()
                .filter(|n| *n > 0)
                .or_else(|| {
                    previous.filter(|n| *n > 0).zip(original_dpi).and_then(|(n, dpi)| {
                        i64::from(n).checked_mul(i64::from(x)).map(|n| n / i64::from(dpi)).and_then(|n| i32::try_from(n).ok()).filter(|n| *n > 0)
                    })
                })
                .ok_or_else(|| failed("the scanner did not report a usable scan area; update its WIA driver"))?;
            let pixels = mm.map_or(max, |mm| ((mm / 25.4 * f64::from(x)).round() as i32).clamp(1, max));
            prop.invoke_put("Value", &Variant::I4(pixels)).map_err(com_error)?;
            dimensions.push(pixels);
        }
        let pixels = dimensions.iter().try_fold(1u64, |n, p| n.checked_mul(*p as u64));
        if pixels.is_none_or(|n| n > MAX_PAGE_BYTES / 4) {
            return Err(failed("the selected scan area and resolution are too large; choose a lower resolution or smaller paper"));
        }
        Ok(f64::from(x))
    }

    fn transfer(item: &IDispatch, dpi: f64) -> Result<ScannedPage, ScanError> {
        let image = object(item.invoke_method("Transfer", &[&Variant::Bstr(BMP.into())]).map_err(com_error)?)?;
        // SaveFile is a safe automation method; a private, unpredictable directory owns the
        // absent destination. Never write a user path or read a driver's arbitrary path.
        let directory = tempfile::Builder::new().prefix("pdfcraft-scan-").tempdir().map_err(failed)?;
        let path = directory.path().join("page.bmp");
        let name = path.to_str().ok_or_else(|| failed("the temporary scan path is not Unicode"))?;
        image.invoke_method("SaveFile", &[&Variant::Bstr(name.into())]).map_err(com_error)?;
        let file = std::fs::File::open(&path).map_err(failed)?;
        let length = file.metadata().map_err(failed)?.len();
        if length == 0 || length > MAX_PAGE_BYTES {
            return Err(failed("the scanner returned an empty or oversized image"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_PAGE_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(failed)?;
        if bytes.len() as u64 > MAX_PAGE_BYTES {
            return Err(failed("the scanner returned an oversized image"));
        }
        Ok(ScannedPage { bytes, dpi })
    }

    pub fn scan(id: &str, settings: &ScanSettings, cancel: &AtomicBool) -> Result<Vec<ScannedPage>, ScanError> {
        let _apartment = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE).map_err(failed)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(ScanError::Cancelled);
        }
        let device = connect(id)?;
        let item = source(&device, settings.source)?;
        let dpi = configure(&item, settings)?;
        let mut pages = Vec::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return crate::interrupted(pages, ScanError::Cancelled);
            }
            match transfer(&item, dpi) {
                Ok(page) => {
                    if let Err(error) = crate::push_page(&mut pages, page) {
                        return crate::interrupted(pages, error);
                    }
                    if !settings.source.is_feeder() {
                        return Ok(pages);
                    }
                    if pages.len() >= MAX_PAGES {
                        return crate::interrupted(pages, failed("page limit reached; save these pages before scanning more"));
                    }
                }
                Err(ScanError::FeederEmpty) if !pages.is_empty() => return Ok(pages),
                Err(error) => return crate::interrupted(pages, error),
            }
        }
    }
}

#[cfg(windows)]
pub use native::{scan, scanners};

#[cfg(test)]
mod tests {
    use super::error_code;
    use crate::ScanError;
    #[test]
    fn hardware_errors_do_not_depend_on_windows_language() {
        assert_eq!(error_code(0x80210003), Some(ScanError::FeederEmpty));
        assert_eq!(error_code(0x80210002), Some(ScanError::Jammed));
        assert_eq!(error_code(0x8021000d), Some(ScanError::Busy));
        assert_eq!(error_code(0x80210016), Some(ScanError::CoverOpen));
        assert_eq!(error_code(0x80004005), None);
    }
}
