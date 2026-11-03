use pdfcraft_automation::{Automation, Content};
use pdfcraft_scan::fake::FakeEscl;
use serde_json::{Value, json};

fn call(a: &mut Automation, name: &str, args: Value) -> Value {
    a.call(name, &args)
        .unwrap()
        .into_iter()
        .find_map(|c| match c {
            Content::Json(v) => Some(v),
            _ => None,
        })
        .unwrap()
}

#[test]
fn scanner_acquisition_creates_dirty_document_and_renders_it() {
    let fake = FakeEscl::start(2).unwrap();
    let mut a = Automation::new();
    let doc = call(&mut a, "doc_create", json!({"from":"scanner", "scanner":fake.id(), "source":"feeder", "user_confirmed":true}));
    assert_eq!(doc["pages"], 2);
    assert_eq!(doc["dirty"], true);
    let output = a.call("page_render", &json!({"doc":doc["doc"],"page":1,"dpi":36})).unwrap();
    assert!(output.iter().any(|c| matches!(c, Content::Png {data, width, height} if !data.is_empty() && *width > 0 && *height > 0)));
}

#[test]
fn scanner_acquisition_requires_consent_before_contacting_device() {
    let fake = FakeEscl::start(0).unwrap();
    let mut a = Automation::new();
    for confirmed in [json!(null), json!(false)] {
        let error = a.call("doc_create", &json!({"from":"scanner", "scanner":fake.id(), "user_confirmed":confirmed})).unwrap_err().to_string();
        assert!(error.contains("consent"), "{error}");
    }
    assert!(a.session().docs().is_empty());
    assert!(fake.requests().is_empty());
}

#[test]
fn invalid_optional_scan_arguments_do_not_feed_paper() {
    for (key, value) in [
        ("name", json!(1)),
        ("ocr", json!("yes")),
        ("language", json!(1)),
        ("name", json!("x".repeat(4097))),
        ("language", json!("x".repeat(65))),
        ("preset", json!("x".repeat(129))),
    ] {
        let fake = FakeEscl::start(0).unwrap();
        let mut a = Automation::new();
        let mut args = json!({"from":"scanner","scanner":fake.id(),"scan_dpi":75,"user_confirmed":true});
        args[key] = value;
        assert!(a.call("doc_create", &args).is_err());
        assert!(fake.requests().is_empty(), "invalid {key} must be rejected before operating the device");
    }
}

#[test]
fn jam_keeps_delivered_pages_and_returns_a_warning() {
    let fake = FakeEscl::start(2).unwrap();
    fake.jam_after(1);
    let mut a = Automation::new();
    let doc = call(
        &mut a,
        "command_run",
        json!({"id":"create.scanner","params":{"scanner":fake.id(),"source":"feeder","scan_dpi":75,"user_confirmed":true}}),
    );
    assert_eq!(doc["pages"], 1);
    assert_eq!(doc["dirty"], true);
    assert!(doc["scan_warning"].as_str().unwrap().contains("jammed"));
}

#[test]
fn different_page_resolutions_keep_their_physical_sizes() {
    use pdfcraft_engine::{
        Session,
        scan::{self, ScannedPage},
    };
    use std::sync::atomic::AtomicBool;
    let fake = FakeEscl::start(0).unwrap();
    let settings = scan::ScanSettings { dpi: 75, ..Default::default() };
    let page = scan::scan(&fake.id(), &settings, &AtomicBool::new(false)).unwrap().remove(0);
    let pages = [ScannedPage { bytes: page.bytes.clone(), dpi: 75.0 }, ScannedPage { bytes: page.bytes, dpi: 150.0 }];
    let mut session = Session::new();
    let bytes = session.create_from_scan(&pages, "Scan").unwrap();
    let id = session.open_new("Scan.pdf", bytes).unwrap();
    let document = session.get(id).unwrap();
    assert!((document.info.pages[0].width / document.info.pages[1].width - 2.0).abs() < 0.01);
    assert!(session.create_from_scan(&[], "empty").is_err());
    assert!(session.create_from_scan(&[ScannedPage { bytes: Vec::new(), dpi: f64::NAN }], "invalid").is_err());
}
