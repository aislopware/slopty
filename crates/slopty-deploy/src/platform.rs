//! What a machine runs on, as `uname -sm` names it and as a binary's header says.

use std::io::Read as _;
use std::path::Path;

/// An OS and a CPU, as a machine or a binary names them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Platform {
    /// The OS.
    pub os: Os,
    /// The CPU.
    pub arch: Arch,
}

/// The OSes a worker runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    /// macOS (`Darwin`, Mach-O).
    MacOs,
    /// Linux (ELF).
    Linux,
}

/// The CPUs a worker runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    /// 64-bit ARM.
    Arm64,
    /// `x86_64`.
    X86_64,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let os = match self.os {
            Os::MacOs => "macOS",
            Os::Linux => "Linux",
        };
        let arch = match self.arch {
            Arch::Arm64 => "arm64",
            Arch::X86_64 => "x86_64",
        };
        write!(f, "{os} {arch}")
    }
}

/// A machine no worker is built for.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Unsupported {
    /// Its OS, as `uname -s` named it.
    #[error("no worker runs on {0}")]
    Os(String),
    /// Its CPU, as `uname -m` named it.
    #[error("no worker is built for a {0} CPU")]
    Cpu(String),
}

/// `CPU_TYPE_ARM64` and `CPU_TYPE_X86_64` from `<mach/machine.h>`.
pub const MACHO_ARM64: u32 = 0x0100_000c;
pub const MACHO_X86_64: u32 = 0x0100_0007;
/// `EM_X86_64` and `EM_AARCH64` from `<elf.h>`.
pub const ELF_X86_64: u16 = 62;
pub const ELF_AARCH64: u16 = 183;

impl Platform {
    /// The machine `uname -sm` describes.
    ///
    /// # Errors
    ///
    /// For an OS or a CPU no worker is built for.
    pub fn from_uname(uname: &str) -> Result<Self, Unsupported> {
        let mut words = uname.split_whitespace();
        let os = match words.next() {
            Some("Darwin") => Os::MacOs,
            Some("Linux") => Os::Linux,
            other => {
                return Err(Unsupported::Os(other.unwrap_or("an OS that has no name").to_owned()));
            }
        };
        let arch = match words.next() {
            Some("arm64" | "aarch64") => Arch::Arm64,
            Some("x86_64" | "amd64") => Arch::X86_64,
            other => return Err(Unsupported::Cpu(other.unwrap_or("nameless").to_owned())),
        };
        Ok(Self { os, arch })
    }

    /// Every platform a binary's header says it runs on: one for a thin binary, each slice of
    /// a universal one, none for what is no executable of ours.
    #[must_use]
    pub fn of_binary(head: &[u8]) -> Vec<Self> {
        let le32 =
            |at: usize| head.get(at..at.checked_add(4)?)?.try_into().ok().map(u32::from_le_bytes);
        let be32 =
            |at: usize| head.get(at..at.checked_add(4)?)?.try_into().ok().map(u32::from_be_bytes);
        let mac = |cpu: u32| {
            let arch = match cpu {
                MACHO_ARM64 => Arch::Arm64,
                MACHO_X86_64 => Arch::X86_64,
                _ => return None,
            };
            Some(Self { os: Os::MacOs, arch })
        };
        match head.get(..4) {
            // MH_MAGIC_64, as a little-endian machine writes it.
            Some([0xcf, 0xfa, 0xed, 0xfe]) => le32(4).and_then(mac).into_iter().collect(),
            // FAT_MAGIC: a count, then 20-byte `fat_arch` records, big-endian.
            Some([0xca, 0xfe, 0xba, 0xbe]) => {
                let count = be32(4).unwrap_or(0).min(16);
                (0..count)
                    .filter_map(|i| {
                        let at = usize::try_from(i).ok()?.checked_mul(20)?.checked_add(8)?;
                        be32(at).and_then(mac)
                    })
                    .collect()
            }
            // ELF, 64-bit, little-endian: `e_machine` at 18.
            Some([0x7f, b'E', b'L', b'F']) if head.get(4..6) == Some(&[2, 1]) => {
                let machine =
                    head.get(18..20).and_then(|b| b.try_into().ok()).map(u16::from_le_bytes);
                let arch = match machine {
                    Some(ELF_AARCH64) => Arch::Arm64,
                    Some(ELF_X86_64) => Arch::X86_64,
                    _ => return Vec::new(),
                };
                vec![Self { os: Os::Linux, arch }]
            }
            _ => Vec::new(),
        }
    }
}

/// The platforms the binary at `path` runs on.
pub fn platforms_of(path: &Path) -> std::io::Result<Vec<Platform>> {
    let mut head = Vec::with_capacity(512);
    std::fs::File::open(path).and_then(|file| file.take(512).read_to_end(&mut head))?;
    Ok(Platform::of_binary(&head))
}
