//! The Mach-O image this crate's code runs in: its UUID, where it is loaded, how long its code
//! is, and its symbol table.
//!
//! A signal record names its frames by offset into that image, and the next run of the same
//! build (same UUID) puts them back at its own load address to resolve them. The symbol table
//! gives each frame's function its whole path: the debug info the workspace builds with
//! (`line-tables-only`) names functions without their module.

use std::ffi::c_void;
use std::fmt::Write as _;

/// `MH_MAGIC_64`, `<mach-o/loader.h>`.
const MH_MAGIC_64: u32 = 0xfeed_facf;
/// `LC_SYMTAB`, `<mach-o/loader.h>`.
const LC_SYMTAB: u32 = 0x2;
/// `LC_SEGMENT_64`, `<mach-o/loader.h>`.
const LC_SEGMENT_64: u32 = 0x19;
/// `LC_UUID`, `<mach-o/loader.h>`.
const LC_UUID: u32 = 0x1b;
/// The size of `struct mach_header_64`, which the load commands follow (`<mach-o/loader.h>`).
const HEADER_64: usize = 32;
/// The size of `struct nlist_64`, `<mach-o/nlist.h>`.
const NLIST_64: usize = 16;
/// `N_STAB`, `<mach-o/nlist.h>`: a debugger entry, not a symbol.
const N_STAB: u8 = 0xe0;
/// `N_TYPE`, `<mach-o/nlist.h>`.
const N_TYPE: u8 = 0x0e;
/// `N_SECT`, `<mach-o/nlist.h>`: defined in a section of this image.
const N_SECT: u8 = 0x0e;

/// One loaded image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Image {
    /// `LC_UUID`.
    uuid: [u8; 16],
    /// The address its Mach-O header is loaded at, which is where `__TEXT` starts.
    base: usize,
    /// The size of `__TEXT`, which holds every instruction.
    text: usize,
    /// `__TEXT`'s address as linked, before the slide.
    text_vmaddr: u64,
    /// Where `LC_SYMTAB`'s tables are mapped, if it has them.
    symtab: Option<Symtab>,
}

/// The symbol and string tables, as mapped in `__LINKEDIT`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Symtab {
    symbols: usize,
    count: usize,
    strings: usize,
    strings_size: usize,
}

impl Image {
    /// The image holding this crate, which for a Rust binary is its executable.
    pub(crate) fn current() -> Option<Self> {
        let marker: fn() -> Option<Self> = Self::current;
        let mut info = empty_dl_info();
        // SAFETY: `dladdr` (dlfcn.h) looks the address up in dyld's image list and fills
        // `info`, which outlives the call; zero comes back for an address no image holds.
        let found = unsafe { libc::dladdr((marker as *const ()).cast(), &raw mut info) };
        if found == 0 || info.dli_fbase.is_null() {
            return None;
        }
        Self::at(info.dli_fbase.expose_provenance())
    }

    /// The image whose Mach-O header is loaded at `base`.
    fn at(base: usize) -> Option<Self> {
        // SAFETY: `base` is `dli_fbase` for a loaded image: dyld maps its Mach-O header there,
        // readable for as long as the image stays loaded, and the image holding this code is
        // never unloaded while it runs.
        let header = unsafe { mapped(base, HEADER_64) };
        if u32_at(header, 0)? != MH_MAGIC_64 {
            return None;
        }
        let count = u32_at(header, 16)?;
        let size = usize::try_from(u32_at(header, 20)?).ok()?;
        // SAFETY: the load commands follow the header for `sizeofcmds` bytes and are mapped
        // with it (`<mach-o/loader.h>`).
        let mut commands = unsafe { mapped(base.checked_add(HEADER_64)?, size) };

        let (mut uuid, mut text, mut linkedit, mut symtab) = (None, None, None, None);
        for _ in 0..count {
            let size = usize::try_from(u32_at(commands, 4)?).ok()?;
            let (command, rest) = commands.split_at_checked(size.max(8))?;
            match u32_at(command, 0)? {
                LC_UUID => uuid = command.get(8..24).and_then(|id| id.try_into().ok()),
                LC_SEGMENT_64 => {
                    let name = command.get(8..24)?;
                    let segment =
                        (u64_at(command, 24)?, u64_at(command, 32)?, u64_at(command, 40)?);
                    if name.starts_with(b"__TEXT\0") {
                        text = Some(segment);
                    } else if name.starts_with(b"__LINKEDIT\0") {
                        linkedit = Some(segment);
                    }
                }
                LC_SYMTAB => {
                    symtab = Some((
                        u32_at(command, 8)?,
                        u32_at(command, 12)?,
                        u32_at(command, 16)?,
                        u32_at(command, 20)?,
                    ));
                }
                _ => {}
            }
            commands = rest;
        }
        let (text_vmaddr, text_size, _) = text?;
        let slide = u64::try_from(base).ok()?.checked_sub(text_vmaddr)?;
        let symtab = linkedit
            .zip(symtab)
            .and_then(|(linkedit, symtab)| Symtab::mapped(slide, linkedit, symtab));
        Some(Self {
            uuid: uuid?,
            base,
            text: usize::try_from(text_size).ok()?,
            text_vmaddr,
            symtab,
        })
    }

    /// The address its Mach-O header is loaded at, which is where `__TEXT` starts.
    pub(crate) const fn base(&self) -> usize {
        self.base
    }

    /// The size of `__TEXT`, which holds every instruction.
    pub(crate) const fn text(&self) -> usize {
        self.text
    }

    /// The UUID as `dwarfdump --uuid` and crash reports print it, upper-case with dashes.
    pub(crate) fn uuid_string(&self) -> String {
        uuid_string(&self.uuid)
    }

    /// The symbol table, sorted to look functions up by offset; `None` for an image without one.
    /// Sorting costs some milliseconds on a large binary, so a crash's frames share one.
    pub(crate) fn symbols(&self) -> Option<Symbols> {
        let symtab = self.symtab?;
        // SAFETY: `Symtab::mapped` checked that both tables lie inside `__LINKEDIT`, which dyld
        // maps read-only for as long as the image is loaded.
        let symbols = unsafe { mapped(symtab.symbols, symtab.count.saturating_mul(NLIST_64)) };
        // SAFETY: as above.
        let strings = unsafe { mapped(symtab.strings, symtab.strings_size) };
        let text = self.text as u64;
        let mut starts: Vec<(u64, usize)> = symbols
            .as_chunks::<NLIST_64>()
            .0
            .iter()
            .filter_map(|entry| {
                let kind = *entry.get(4)?;
                if kind & N_STAB != 0 || kind & N_TYPE != N_SECT {
                    return None;
                }
                let offset = u64_at(entry, 8)?.checked_sub(self.text_vmaddr)?;
                let name = usize::try_from(u32_at(entry, 0)?).ok()?;
                (offset < text).then_some((offset, name))
            })
            .collect();
        starts.sort_unstable();
        Some(Symbols { starts, strings })
    }
}

/// An image's function symbols by offset.
pub(crate) struct Symbols {
    /// Each symbol's offset into the image and its name's offset into `strings`, by offset.
    starts: Vec<(u64, usize)>,
    strings: &'static [u8],
}

impl Symbols {
    /// The function holding `offset`, by its symbol-table name, demangled: the nearest symbol
    /// at or below it.
    pub(crate) fn function_at(&self, offset: u64) -> Option<String> {
        let after = self.starts.partition_point(|(start, _)| *start <= offset);
        let (_, name) = self.starts.get(after.checked_sub(1)?)?;
        let name = self.strings.get(*name..)?;
        let name = name.get(..name.iter().position(|b| *b == 0)?)?;
        Some(symbol_name(&String::from_utf8_lossy(name)))
    }
}

impl Symtab {
    /// Where `LC_SYMTAB`'s tables (`symoff`, `nsyms`, `stroff`, `strsize`) are mapped, given
    /// `__LINKEDIT` (`vmaddr`, `vmsize`, `fileoff`) and the image's slide; `None` unless both
    /// lie inside `__LINKEDIT`.
    fn mapped(slide: u64, linkedit: (u64, u64, u64), symtab: (u32, u32, u32, u32)) -> Option<Self> {
        let (vmaddr, vmsize, fileoff) = linkedit;
        let (symoff, count, stroff, strsize) = symtab;
        let at = |file_offset: u32, size: u64| -> Option<usize> {
            let inside = u64::from(file_offset).checked_sub(fileoff)?;
            if inside.checked_add(size)? > vmsize {
                return None;
            }
            usize::try_from(vmaddr.checked_add(slide)?.checked_add(inside)?).ok()
        };
        Some(Self {
            symbols: at(symoff, u64::from(count).checked_mul(NLIST_64 as u64)?)?,
            count: usize::try_from(count).ok()?,
            strings: at(stroff, u64::from(strsize))?,
            strings_size: usize::try_from(strsize).ok()?,
        })
    }
}

/// `len` mapped bytes at `address`.
///
/// # Safety
///
/// `address..address + len` must be mapped readable and stay so for `'static`.
const unsafe fn mapped(address: usize, len: usize) -> &'static [u8] {
    // SAFETY: the caller promises the range is mapped for good; bytes have no invalid values.
    unsafe { std::slice::from_raw_parts(std::ptr::with_exposed_provenance::<u8>(address), len) }
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// A symbol-table name as source spells it: Rust demangled, C without the underscore Mach-O
/// puts in front.
fn symbol_name(symbol: &str) -> String {
    let named = crate::report::demangle(symbol);
    if named == symbol { symbol.strip_prefix('_').unwrap_or(symbol).to_owned() } else { named }
}

/// `uuid` as `8-4-4-4-12` upper-case hex.
pub(crate) fn uuid_string(uuid: &[u8; 16]) -> String {
    let mut text = String::with_capacity(36);
    for (i, byte) in uuid.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        let _infallible = write!(text, "{byte:02X}");
    }
    text
}

/// What `dladdr` knows of `address` in this process: the file name of the image holding it,
/// where that image is loaded, and the nearest exported symbol at or below the address.
pub(crate) fn dl_image(address: usize) -> Option<(String, usize, Option<String>)> {
    let mut info = empty_dl_info();
    // SAFETY: `dladdr` (dlfcn.h) only looks the address up in dyld's image list, whatever it
    // is, and fills `info`, which outlives the call.
    let found = unsafe {
        libc::dladdr(std::ptr::with_exposed_provenance::<c_void>(address), &raw mut info)
    };
    if found == 0 || info.dli_fname.is_null() {
        return None;
    }
    // SAFETY: a non-null `dli_fname` is the image's NUL-terminated path, owned by dyld for as
    // long as the image is loaded (dlfcn.h).
    let path = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) };
    let path = path.to_string_lossy();
    let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
    let symbol = (!info.dli_sname.is_null()).then(|| {
        // SAFETY: a non-null `dli_sname` is the symbol's NUL-terminated name in the image's
        // string table, mapped for as long as the image is loaded (dlfcn.h).
        let symbol = unsafe { std::ffi::CStr::from_ptr(info.dli_sname) };
        crate::report::demangle(&symbol.to_string_lossy())
    });
    Some((name, info.dli_fbase.expose_provenance(), symbol))
}

/// A `Dl_info` for `dladdr` to fill.
const fn empty_dl_info() -> libc::Dl_info {
    libc::Dl_info {
        dli_fname: std::ptr::null(),
        dli_fbase: std::ptr::null_mut(),
        dli_sname: std::ptr::null(),
        dli_saddr: std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Image, symbol_name, uuid_string};

    #[inline(never)]
    fn a_function_of_this_module() -> usize {
        let here: fn() -> usize = a_function_of_this_module;
        (here as *const ()).addr()
    }

    #[test]
    fn this_binary_knows_its_own_image_and_names_its_functions() {
        let image = Image::current().expect("the test binary is a Mach-O image");
        let here = a_function_of_this_module();
        let offset = here.checked_sub(image.base()).expect("above the header");
        assert!(offset < image.text(), "this crate's code is in its image's __TEXT");
        assert!(image.uuid != [0; 16], "the linker stamps a UUID");
        let symbols = image.symbols().expect("a symbol table");
        let expected = Some("slopty_crash::image::tests::a_function_of_this_module".to_owned());
        assert_eq!(symbols.function_at(offset as u64), expected, "by path, at its start");
        assert_eq!(symbols.function_at(offset as u64 + 4), expected, "and inside it");
    }

    #[test]
    fn symbol_names_read_as_source_spells_them() {
        assert_eq!(symbol_name("_ghostty_terminal_new"), "ghostty_terminal_new", "C");
        assert_eq!(symbol_name("__RNvCs1234_7mycrate3foo"), "mycrate::foo", "Rust, v0");
    }

    #[test]
    fn uuids_print_like_dwarfdump() {
        let uuid = [
            0x24, 0xf5, 0x65, 0x06, 0xd1, 0x57, 0x3b, 0x47, 0x90, 0xf2, 0x0c, 0xda, 0x28, 0xd0,
            0x04, 0x9a,
        ];
        assert_eq!(uuid_string(&uuid), "24F56506-D157-3B47-90F2-0CDA28D0049A", "8-4-4-4-12");
    }
}
