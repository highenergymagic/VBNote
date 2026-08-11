//! Building a NOR flash image the way a factory-provisioned machine has it.
//!
//! A real BrailleNote's flash is not just `NK.bin` dropped at an offset. The
//! bootloader occupies the first 256 KB, a twelve-byte HumanWare image header
//! sits immediately after it, and the Windows CE image follows. EBOOT reads
//! that header before it will boot anything:
//!
//! ```text
//!   0x000000  EBOOT, stored at image-base-relative offsets. Flash offset 0
//!             is EBOOT.bin's base address, and holds the reset branch.
//!   0x040000  Image header: { u32 id = 0x45464748, u32 start, u32 length }
//!   0x041000  NK.bin, at the offset its link address implies: it is based at
//!             0x80041000 and nCS0 maps to 0x80000000.
//! ```
//!
//! The header offset and magic come from EBOOT itself: the constructor at
//! `0x96c7cf7c` passes `0x40000`, and the validator at `0x96c858f0` reads
//! twelve bytes there and compares the first word against `0x45464748`.

use ceromfs::CeImage;

/// Signature EBOOT requires in the image header.
pub const IMAGE_MAGIC: u32 = 0x4546_4748;

/// Flash offset of the image header.
pub const HEADER_OFFSET: usize = 0x0004_0000;

/// Flash offset the Windows CE image starts at.
pub const KERNEL_OFFSET: usize = 0x0004_1000;

/// Virtual address nCS0 is mapped to by the OEMAddressTable.
pub const FLASH_VA_BASE: u32 = 0x8000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageHeader {
    pub id: u32,
    pub start: u32,
    pub length: u32,
}

impl ImageHeader {
    pub fn to_bytes(self) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[0..4].copy_from_slice(&self.id.to_le_bytes());
        b[4..8].copy_from_slice(&self.start.to_le_bytes());
        b[8..12].copy_from_slice(&self.length.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<ImageHeader> {
        if b.len() < 12 {
            return None;
        }
        Some(ImageHeader {
            id: u32::from_le_bytes(b[0..4].try_into().ok()?),
            start: u32::from_le_bytes(b[4..8].try_into().ok()?),
            length: u32::from_le_bytes(b[8..12].try_into().ok()?),
        })
    }

    pub fn is_valid(&self) -> bool {
        self.id == IMAGE_MAGIC
    }
}

/// How the kernel's Start field should be expressed.
///
/// EBOOT prints the value but the code path that consumes it runs through a
/// boot-device vtable, so which convention it wants is not yet proven from
/// static analysis. Both are one line apart, so the choice is exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartConvention {
    /// Offset within the flash device, matching how the header itself is
    /// addressed (`Read(0x40000, ...)`).
    FlashOffset,
    /// Kernel virtual address, matching NK.bin's own link base.
    VirtualAddress,
}

pub struct Provisioned {
    pub image: Vec<u8>,
    pub header: ImageHeader,
    pub eboot_bytes: usize,
    pub kernel_bytes: usize,
}

impl std::fmt::Debug for Provisioned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never format the image itself; it is 64 MB.
        f.debug_struct("Provisioned")
            .field("image_len", &self.image.len())
            .field("header", &self.header)
            .field("eboot_bytes", &self.eboot_bytes)
            .field("kernel_bytes", &self.kernel_bytes)
            .finish()
    }
}

/// Lay out a complete flash image from the two firmware files.
pub fn build_flash_image(
    size: usize,
    eboot: &CeImage,
    kernel: Option<&CeImage>,
    convention: StartConvention,
) -> Result<Provisioned, String> {
    let mut image = vec![0xFFu8; size];

    // EBOOT: stored relative to its own base, so flash offset 0 is the reset
    // branch. Its self-copy loop reads from physical 0 and depends on this.
    let mut eboot_bytes = 0;
    for r in &eboot.records {
        let off = r.addr.wrapping_sub(eboot.base) as usize;
        let end = off + r.data.len();
        if end > HEADER_OFFSET {
            return Err(format!(
                "bootloader record at {:#010x} reaches flash offset {:#x}, \
                 which would overwrite the image header at {:#x}",
                r.addr, end, HEADER_OFFSET
            ));
        }
        image[off..end].copy_from_slice(&r.data);
        eboot_bytes += r.data.len();
    }

    let Some(kernel) = kernel else {
        return Ok(Provisioned {
            image,
            header: ImageHeader { id: 0xFFFF_FFFF, start: 0xFFFF_FFFF, length: 0xFFFF_FFFF },
            eboot_bytes,
            kernel_bytes: 0,
        });
    };

    // Kernel: at the flash offset its link address implies.
    let (lo, hi) = kernel.extent().ok_or("the kernel image has no records")?;
    if lo < FLASH_VA_BASE {
        return Err(format!("kernel record at {lo:#010x} is below the flash window"));
    }
    let mut kernel_bytes = 0;
    for r in &kernel.records {
        let off = (r.addr - FLASH_VA_BASE) as usize;
        let end = off + r.data.len();
        if end > size {
            return Err(format!(
                "kernel record at {:#010x} runs past the end of a {} MB device",
                r.addr,
                size / (1024 * 1024)
            ));
        }
        if off < KERNEL_OFFSET {
            return Err(format!(
                "kernel record at {:#010x} maps to flash offset {off:#x}, \
                 below the expected start of {KERNEL_OFFSET:#x}",
                r.addr
            ));
        }
        image[off..end].copy_from_slice(&r.data);
        kernel_bytes += r.data.len();
    }

    let start = match convention {
        StartConvention::FlashOffset => lo - FLASH_VA_BASE,
        StartConvention::VirtualAddress => lo,
    };
    let header = ImageHeader { id: IMAGE_MAGIC, start, length: hi - lo };
    image[HEADER_OFFSET..HEADER_OFFSET + 12].copy_from_slice(&header.to_bytes());

    Ok(Provisioned { image, header, eboot_bytes, kernel_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(base: u32, records: &[(u32, usize)], launch: u32) -> CeImage {
        let mut bytes = b"B000FF\n".to_vec();
        bytes.extend_from_slice(&base.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for (addr, len) in records {
            let data = vec![0xA5u8; *len];
            bytes.extend_from_slice(&addr.to_le_bytes());
            bytes.extend_from_slice(&(*len as u32).to_le_bytes());
            let sum: u32 = data.iter().map(|b| *b as u32).sum();
            bytes.extend_from_slice(&sum.to_le_bytes());
            bytes.extend_from_slice(&data);
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&launch.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        CeImage::parse(&bytes).unwrap()
    }

    const SIZE: usize = 64 * 1024 * 1024;

    #[test]
    fn bootloader_lands_at_flash_offset_zero() {
        let eboot = image(0x96C7_8000, &[(0x96C7_8000, 4), (0x96C7_9000, 0x100)], 0x96C7_9000);
        let p = build_flash_image(SIZE, &eboot, None, StartConvention::FlashOffset).unwrap();
        assert_eq!(&p.image[0..4], &[0xA5; 4], "reset branch at offset 0");
        assert_eq!(&p.image[0x1000..0x1004], &[0xA5; 4]);
        assert_eq!(p.eboot_bytes, 4 + 0x100);
    }

    #[test]
    fn header_is_written_where_eboot_looks_for_it() {
        let eboot = image(0x96C7_8000, &[(0x96C7_8000, 4)], 0x96C7_9000);
        let nk = image(0x8004_1000, &[(0x8004_1000, 0x2000)], 0x8004_1000);
        let p = build_flash_image(SIZE, &eboot, Some(&nk), StartConvention::FlashOffset).unwrap();

        let h = ImageHeader::from_bytes(&p.image[HEADER_OFFSET..HEADER_OFFSET + 12]).unwrap();
        assert!(h.is_valid());
        assert_eq!(h.id, IMAGE_MAGIC);
        assert_eq!(h.start, 0x41000);
        assert_eq!(h.length, 0x2000);
        assert_eq!(h, p.header);
    }

    #[test]
    fn virtual_address_convention_reports_the_link_base() {
        let eboot = image(0x96C7_8000, &[(0x96C7_8000, 4)], 0x96C7_9000);
        let nk = image(0x8004_1000, &[(0x8004_1000, 0x2000)], 0x8004_1000);
        let p = build_flash_image(SIZE, &eboot, Some(&nk), StartConvention::VirtualAddress).unwrap();
        assert_eq!(p.header.start, 0x8004_1000);
    }

    #[test]
    fn kernel_lands_at_the_offset_its_link_address_implies() {
        let eboot = image(0x96C7_8000, &[(0x96C7_8000, 4)], 0x96C7_9000);
        let nk = image(0x8004_1000, &[(0x8004_1000, 8)], 0x8004_1000);
        let p = build_flash_image(SIZE, &eboot, Some(&nk), StartConvention::FlashOffset).unwrap();
        assert_eq!(&p.image[KERNEL_OFFSET..KERNEL_OFFSET + 8], &[0xA5; 8]);
    }

    #[test]
    fn without_a_kernel_the_header_area_stays_erased() {
        let eboot = image(0x96C7_8000, &[(0x96C7_8000, 4)], 0x96C7_9000);
        let p = build_flash_image(SIZE, &eboot, None, StartConvention::FlashOffset).unwrap();
        assert_eq!(&p.image[HEADER_OFFSET..HEADER_OFFSET + 12], &[0xFF; 12]);
        assert!(!p.header.is_valid());
    }

    #[test]
    fn a_bootloader_that_would_clobber_the_header_is_rejected() {
        let eboot = image(0x96C7_8000, &[(0x96C7_8000, HEADER_OFFSET + 16)], 0x96C7_9000);
        let err = build_flash_image(SIZE, &eboot, None, StartConvention::FlashOffset).unwrap_err();
        assert!(err.contains("image header"), "{err}");
    }

    #[test]
    fn a_kernel_below_the_expected_offset_is_rejected() {
        let eboot = image(0x96C7_8000, &[(0x96C7_8000, 4)], 0x96C7_9000);
        let nk = image(0x8000_0000, &[(0x8000_0000, 16)], 0x8000_0000);
        let err =
            build_flash_image(SIZE, &eboot, Some(&nk), StartConvention::FlashOffset).unwrap_err();
        assert!(err.contains("below the expected start"), "{err}");
    }

    #[test]
    fn header_round_trips_through_bytes() {
        let h = ImageHeader { id: IMAGE_MAGIC, start: 0x41000, length: 0x2A86FD0 };
        assert_eq!(ImageHeader::from_bytes(&h.to_bytes()), Some(h));
    }
}
