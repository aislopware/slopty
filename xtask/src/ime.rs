//! `cargo xtask ime`: list or select macOS input sources (Text Input Sources, `HIToolbox`).
//!
//! Synthetic key events never trigger the "select previous input source" hotkey, so driving
//! input-method tests (Telex composition in a terminal) needs a real `TISSelectInputSource`.
//! Dev tooling only; the app never touches input sources.

use std::ffi::c_void;
use std::ptr::NonNull;

use anyhow::{Context as _, Result, bail};
use objc2_core_foundation::{CFArray, CFDictionary, CFRetained, CFString, CFType};

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C-unwind" {
    static kTISPropertyInputSourceID: &'static CFString;
    fn TISCreateInputSourceList(
        properties: *const CFDictionary,
        include_all_installed: u8,
    ) -> *mut CFArray<CFType>;
    fn TISGetInputSourceProperty(source: *const CFType, key: &CFString) -> *const c_void;
    fn TISSelectInputSource(source: *const CFType) -> i32;
    fn TISEnableInputSource(source: *const CFType) -> i32;
}

/// Input sources: the enabled ones, or every installed one (input modes such as Telex only
/// appear in the full list; their parent input method is what "enabled" tracks).
fn sources(all: bool) -> Result<CFRetained<CFArray<CFType>>> {
    // SAFETY: `TISCreateInputSourceList` follows the Create rule and returns an owned CFArray
    // of TISInputSourceRef (a CFType); a null filter means no property filter.
    let list = unsafe { TISCreateInputSourceList(std::ptr::null(), u8::from(all)) };
    let list = NonNull::new(list).context("TISCreateInputSourceList returned null")?;
    // SAFETY: ownership of the +1 array passes to us here, once.
    Ok(unsafe { CFRetained::from_raw(list) })
}

fn source_id(source: &CFType) -> String {
    // SAFETY: `kTISPropertyInputSourceID` is documented to yield a CFStringRef that the source
    // owns (Get rule), valid while `source` lives; we only read it.
    let raw = unsafe { TISGetInputSourceProperty(source, kTISPropertyInputSourceID) };
    if raw.is_null() {
        return String::from("?");
    }
    // SAFETY: the property is a CFString per the HIToolbox documentation for this key.
    let id = unsafe { &*raw.cast::<CFString>() };
    id.to_string()
}

/// The source with this exact id in `list`.
fn find(list: &CFArray<CFType>, wanted: &str) -> Option<CFRetained<CFType>> {
    (0..list.len()).filter_map(|i| list.get(i)).find(|s| source_id(s) == wanted)
}

/// List sources, or select the one whose id matches exactly.
pub fn run(select: Option<&str>, all: bool) -> Result<()> {
    let Some(wanted) = select else {
        let list = sources(all)?;
        for i in 0..list.len() {
            if let Some(source) = list.get(i) {
                println!("{}", source_id(&source));
            }
        }
        return Ok(());
    };
    let enabled_list = sources(false)?;
    let source = if let Some(source) = find(&enabled_list, wanted) {
        source
    } else {
        // Not enabled in this login session: enable it, then take the enabled instance.
        let all_list = sources(true)?;
        let installed = find(&all_list, wanted)
            .with_context(|| format!("no input source {wanted}; see `cargo xtask ime --all`"))?;
        // SAFETY: `installed` is a live TISInputSourceRef from the list above.
        let status = unsafe { TISEnableInputSource(&raw const *installed) };
        if status != 0 {
            bail!("TISEnableInputSource failed: OSStatus {status}");
        }
        let now_enabled = sources(false)?;
        find(&now_enabled, wanted).context("enabled, but still not listed as enabled")?
    };
    // SAFETY: `source` is a live TISInputSourceRef from the enabled list.
    let status = unsafe { TISSelectInputSource(&raw const *source) };
    if status != 0 {
        bail!("TISSelectInputSource failed: OSStatus {status}");
    }
    println!("selected {wanted}");
    Ok(())
}
