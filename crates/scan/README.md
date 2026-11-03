# pdfcraft-scan

Layer L4: find scanners and scan pages, for **File ▸ Create ▸ PDF from Scanner** (issue #312).
It has no UI and no PDF code: the engine turns the scanned images into pages
(`Session::create_from_scan`), the shell shows the dialog, and the automation crate offers the
`scanners` tool and `doc_create` with `from: "scanner"`.

```rust
let found = pdfcraft_scan::scanners(Duration::from_secs(3));   // never fails; maybe empty
let settings = Preset::GrayscaleDocument.settings();            // colour mode + dpi
let scanner = found.first().ok_or("no scanner found")?;
let pages = pdfcraft_scan::scan(&scanner.id, &settings, &cancel)?;   // Vec<ScannedPage>
```

A scanner's `id` names the backend that drives it:

| id | Backend | Platforms | Finds scanners with |
|---|---|---|---|
| `escl:http://host:port/eSCL` | **eSCL** (AirScan / Mopria): HTTP + XML | all but the web build | mDNS `_uscan._tcp`, or typed: `escl:192.168.1.20` |
| `sane:<device>` | **SANE** through the `scanimage` program | Linux, FreeBSD, other Unix | `scanimage -L` |
| `wia:<device id>` | **WIA**, installed USB/network/WSD drivers | Windows | WIA DeviceManager |

No application `unsafe`: eSCL uses HTTP (`ureq`) and XML (`roxmltree`), discovery uses
`mdns-sd`, SANE drives `scanimage`, and WIA uses the maintained safe `winsafe` COM wrapper.
No PowerShell helper is launched. WIA needs a scanner driver, including its network
configuration; a printer-only driver is insufficient. TWAIN-only and macOS ICA drivers are
not supported. Device support depends on the installed driver or eSCL implementation; no
hardware coverage percentage has been established. Direct eSCL uses HTTP; HTTPS-only devices
are not supported by this transport.

Typed addresses and mDNS results must use numeric loopback, link-local or private addresses
(RFC 1918 / IPv6 ULA). Hostnames, public addresses, credentials, queries and fragments are
refused. HTTP redirects are never followed, proxies are disabled, and scan-job `Location`
headers must remain on the same origin (scheme, address and port). Numeric addresses avoid
DNS rebinding; mDNS names are shown as labels, while connections use the advertised local IP.

The automation `scanners` and `doc_create` tools advertise `openWorldHint: true`.
`doc_create` with `from: "scanner"` requires `user_confirmed: true` only after explicit user
consent; omitted or false consent is rejected before any scanner request. This is a caller
attestation, not a way for the application to verify a human interaction.

Network access is limited to the scanner workflow: local-address HTTP and mDNS discovery.
The restrictions above implement the PR #359 review requirements; they do not authorize
general outbound requests elsewhere in the app.

- **Settings**: colour mode (black and white, gray, colour), resolution (the device's closest
  supported one is used), source (flatbed, document feeder, feeder both sides) and paper
  (Letter, Legal, A4, A5, or the scanner's whole bed). The four presets (Black & White Document,
  Grayscale Document, Color Document, Color Photo) set the colour mode and resolution.
- **Pages** come back as the image file the device sent (PNG, JPEG, TIFF) with their resolution;
  `pdfcraft-create` embeds them without resampling, sized from that resolution.
- **Errors** are `ScanError`s an agent or person can act on: feeder empty, jam, busy, cover open,
  no such source, not found, cancelled.
- **Cancel**: checked at page boundaries for eSCL/WIA (a driver transfer must return first);
  SANE's child process is killed. Acquired sheets survive failure/cancellation as
  `ScanError::Partial`. Automation creates the partial PDF with `scan_warning`; the GUI keeps
  pages for Create PDF, and closing the dialog opens collected sheets as an unsaved document.
- **Limits**: at most 500 pages and 512 MiB of encoded images in a batch; every answer from a device or a
  program is untrusted and size-capped (AGENTS.md §4).

## Testing without hardware

- `fake::FakeEscl` (feature `fake-escl`) is an eSCL scanner on loopback with a flatbed and a duplex
  feeder; the automation and UI tests scan from it.
- SANE's `test` device (`sane-utils`) runs the real `scanimage` path, including the 10-sheet feeder
  (skipped when `scanimage` is missing).
- Address/Location restrictions and redirect refusal are covered by loopback tests; automation
  tests verify that missing consent sends no requests.

## Hardware check

On 2026-11-03, the installed Windows build at `00c88a2f` successfully acquired and saved
one color, 200 dpi, Letter flatbed page from a Brother MFC-7860DW using its network WIA
driver. The test used the installed CLI/engine and produced a one-page PDF. No source PDF,
scan image, address, or private diagnostic log is included here. This does not qualify USB,
ADF/duplex, OCR, other models, or the subsequently rebased source tree against hardware.

## Provenance

Network/SANE acquisition, the fake device, initial dialog and automation are adapted from
[PdfCraft PR #359](https://github.com/storytold/pdfcraft/pull/359), commit
`c79035ee18cf853e7a335bf8eb120b6cf10e0066` (DWAA1660 and contributors), under this project's
MIT OR Apache-2.0 licence. Local changes integrate the current layout, add safe WIA, retain
partial scans and per-page density, and bound batch storage. No code from PR #1724's unsafe
WIA implementation was imported.

WIA behaviour and identifiers come from Microsoft's
[Automation reference](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/wiaaut/-wiaaut-about-wia-automation),
[Transfer](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/wiaaut/-wiaaut-iitem-transfer),
[document handling](https://learn.microsoft.com/en-us/windows-hardware/drivers/image/wia-ips-document-handling-select)
and [error codes](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/wia/-wia-error-codes).
