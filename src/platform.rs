// Platform seam: the native (desktop/eframe) and web (WASM/browser) builds need
// genuinely different mechanics for file I/O and process control - blocking OS
// dialogs and `std::process::exit` vs. Promise-based browser APIs and a canvas
// that never "quits". Rather than sprinkle `#[cfg(target_arch = "wasm32")]`
// across the GUI, each backend lives in its own file (platform/native.rs,
// platform/web.rs) exposing an *identical* interface (`IoState` + free fns).
// This module is the one place that chooses between them; every caller says
// `platform::…` with no cfg of its own, and the compiler enforces feature
// parity by requiring both backends to satisfy the same call sites.
#[cfg_attr(not(target_arch = "wasm32"), path = "platform/native.rs")]
#[cfg_attr(target_arch = "wasm32", path = "platform/web.rs")]
mod imp;

pub use imp::*;
