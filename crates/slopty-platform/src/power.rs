//! This Mac's power sources, from the I/O Kit's power-source snapshot (`IOPSCopyPowerSourcesInfo`).

use objc2_core_foundation::{CFDictionary, CFString, CFType};
use objc2_io_kit::{
    IOPSCopyPowerSourcesInfo, IOPSCopyPowerSourcesList, IOPSGetPowerSourceDescription,
    kIOPSInternalBatteryType, kIOPSTypeKey,
};

/// Whether this Mac has a battery of its own: a laptop, which sleeps with its lid closed
/// whatever holds it awake. A UPS on a desktop is no battery of its own.
#[must_use]
pub fn has_battery() -> bool {
    let Some(blob) = IOPSCopyPowerSourcesInfo() else { return false };
    // SAFETY: IOKit rule (IOPowerSources.h): the list is read from the blob
    // `IOPSCopyPowerSourcesInfo` returned, and follows the Copy Rule.
    let Some(list) = (unsafe { IOPSCopyPowerSourcesList(Some(&blob)) }) else { return false };
    // SAFETY: IOKit rule (IOPowerSources.h): the list holds power-source handles, CoreFoundation
    // objects.
    let list = unsafe { list.cast_unchecked::<CFType>() };
    let type_key = CFString::from_str(&kIOPSTypeKey.to_string_lossy());
    let internal = kIOPSInternalBatteryType.to_string_lossy();
    list.iter().any(|source| {
        // SAFETY: IOKit rule (IOPowerSources.h): `source` is one of the blob's list, and the
        // description is the blob's, valid while it is.
        let Some(description) =
            (unsafe { IOPSGetPowerSourceDescription(Some(&blob), Some(&source)) })
        else {
            return false;
        };
        // SAFETY: IOKit rule (IOPSKeys.h): a description's keys are strings, its values
        // CoreFoundation objects.
        let description: &CFDictionary<CFString, CFType> = unsafe { description.cast_unchecked() };
        description
            .get(&type_key)
            .and_then(|kind| kind.downcast::<CFString>().ok())
            .is_some_and(|kind| kind.to_string() == internal)
    })
}

#[cfg(test)]
mod tests {
    /// The snapshot reads on this Mac, whichever it is, without failing. A machine of known
    /// kind would pin the answer; a test's machine may be either.
    #[test]
    fn the_power_sources_read() {
        let first = super::has_battery();
        assert_eq!(super::has_battery(), first, "the same answer twice");
    }
}
