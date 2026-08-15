use exbawks_types::{BuildFlavor, GuestVa};
use serde::{Deserialize, Serialize};

use crate::reader::{read_array, read_c_string, read_u32};
use crate::{XbeError, XbeHeader, XbeSection, XbeSectionFlags};

const XBE_MAGIC: &[u8; 4] = b"XBEH";
const MINIMUM_HEADER_SIZE: usize = 0x178;
const SECTION_HEADER_SIZE: usize = 0x38;
const MAXIMUM_SECTION_COUNT: u32 = 4096;

const ENTRY_RETAIL_XOR: u32 = 0xA8FC_57AB;
const ENTRY_DEBUG_XOR: u32 = 0x9485_9D4B;
const KERNEL_RETAIL_XOR: u32 = 0x5B6D_40B6;
const KERNEL_DEBUG_XOR: u32 = 0xEFB1_F152;

/// A parsed and validated XBE image description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XbeImage {
    /// The parsed image header.
    pub header: XbeHeader,
    /// The parsed section headers.
    pub sections: Vec<XbeSection>,
    /// The complete input file size.
    pub file_size: u64,
}

impl XbeImage {
    /// Parses an XBE image from bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, XbeError> {
        if bytes.get(..4) != Some(XBE_MAGIC.as_slice()) {
            return Err(XbeError::InvalidMagic);
        }
        if bytes.len() < MINIMUM_HEADER_SIZE {
            return Err(XbeError::Truncated { field: "image header", offset: bytes.len() });
        }

        let base_address = GuestVa(read_u32(bytes, 0x104, "base address")?);
        let size_of_headers = read_u32(bytes, 0x108, "size of headers")?;
        let size_of_image = read_u32(bytes, 0x10C, "size of image")?;
        let size_of_image_header = read_u32(bytes, 0x110, "size of image header")?;
        let time_date_stamp = read_u32(bytes, 0x114, "time date stamp")?;
        let certificate_address = GuestVa(read_u32(bytes, 0x118, "certificate address")?);
        let section_count = read_u32(bytes, 0x11C, "section count")?;
        let section_headers_address = GuestVa(read_u32(bytes, 0x120, "section headers address")?);
        let initialization_flags = read_u32(bytes, 0x124, "initialization flags")?;
        let encoded_entry_point = read_u32(bytes, 0x128, "encoded entry point")?;
        let tls_address = GuestVa(read_u32(bytes, 0x12C, "TLS address")?);
        let stack_size = read_u32(bytes, 0x130, "stack size")?;
        let heap_reserve = read_u32(bytes, 0x134, "heap reserve")?;
        let heap_commit = read_u32(bytes, 0x138, "heap commit")?;
        let encoded_kernel_thunk_address = read_u32(bytes, 0x158, "kernel thunk address")?;

        validate_header_sizes(bytes, size_of_headers, size_of_image, size_of_image_header)?;
        if section_count > MAXIMUM_SECTION_COUNT {
            return Err(XbeError::SectionLimit {
                count: section_count,
                limit: MAXIMUM_SECTION_COUNT,
            });
        }

        let (build_flavor, entry_point) = decode_entry_point(
            encoded_entry_point,
            base_address,
            size_of_image,
        )?;
        let kernel_thunk_address = decode_kernel_thunk(
            encoded_kernel_thunk_address,
            build_flavor,
            base_address,
            size_of_image,
        )?;

        let header = XbeHeader {
            base_address,
            size_of_headers,
            size_of_image,
            size_of_image_header,
            time_date_stamp,
            certificate_address,
            section_count,
            section_headers_address,
            initialization_flags,
            encoded_entry_point,
            entry_point,
            build_flavor,
            tls_address,
            stack_size,
            heap_reserve,
            heap_commit,
            encoded_kernel_thunk_address,
            kernel_thunk_address,
        };

        let sections = parse_sections(bytes, &header)?;
        Ok(Self {
            header,
            sections,
            file_size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        })
    }

    /// Returns the raw bytes for one parsed section.
    pub fn section_data<'a>(
        &self,
        bytes: &'a [u8],
        section: &XbeSection,
    ) -> Result<&'a [u8], XbeError> {
        let start = usize::try_from(section.raw_address)
            .map_err(|_| XbeError::SectionRawRange { section_index: section.index })?;
        let size = usize::try_from(section.raw_size)
            .map_err(|_| XbeError::SectionRawRange { section_index: section.index })?;
        let end = start
            .checked_add(size)
            .ok_or(XbeError::SectionRawRange { section_index: section.index })?;
        bytes
            .get(start..end)
            .ok_or(XbeError::SectionRawRange { section_index: section.index })
    }
}

fn validate_header_sizes(
    bytes: &[u8],
    size_of_headers: u32,
    size_of_image: u32,
    size_of_image_header: u32,
) -> Result<(), XbeError> {
    if size_of_image_header < MINIMUM_HEADER_SIZE as u32 {
        return Err(XbeError::InvalidHeader("image header is smaller than 0x178 bytes"));
    }
    if size_of_headers < size_of_image_header {
        return Err(XbeError::InvalidHeader("header size is smaller than image-header size"));
    }
    if size_of_image < size_of_headers {
        return Err(XbeError::InvalidHeader("image size is smaller than header size"));
    }
    if usize::try_from(size_of_headers).is_none_or(|size| size > bytes.len()) {
        return Err(XbeError::InvalidHeader("file does not contain all declared header bytes"));
    }
    Ok(())
}

fn decode_entry_point(
    encoded: u32,
    base: GuestVa,
    image_size: u32,
) -> Result<(BuildFlavor, GuestVa), XbeError> {
    let candidates = [
        (BuildFlavor::Retail, GuestVa(encoded ^ ENTRY_RETAIL_XOR)),
        (BuildFlavor::Debug, GuestVa(encoded ^ ENTRY_DEBUG_XOR)),
    ];

    candidates
        .into_iter()
        .find(|(_, address)| image_contains(base, image_size, *address))
        .ok_or(XbeError::InvalidEncodedAddress { field: "entry point" })
}

fn decode_kernel_thunk(
    encoded: u32,
    flavor: BuildFlavor,
    base: GuestVa,
    image_size: u32,
) -> Result<GuestVa, XbeError> {
    let key = match flavor {
        BuildFlavor::Retail => KERNEL_RETAIL_XOR,
        BuildFlavor::Debug => KERNEL_DEBUG_XOR,
        BuildFlavor::Chihiro | BuildFlavor::Unknown => {
            return Err(XbeError::InvalidEncodedAddress { field: "kernel thunk address" });
        }
    };
    let address = GuestVa(encoded ^ key);
    if image_contains(base, image_size, address) {
        Ok(address)
    } else {
        Err(XbeError::InvalidEncodedAddress { field: "kernel thunk address" })
    }
}

fn image_contains(base: GuestVa, image_size: u32, address: GuestVa) -> bool {
    let start = u64::from(base.0);
    let end = start + u64::from(image_size);
    let value = u64::from(address.0);
    value >= start && value < end
}

fn parse_sections(bytes: &[u8], header: &XbeHeader) -> Result<Vec<XbeSection>, XbeError> {
    let table_offset = header_va_to_file_offset(
        header,
        header.section_headers_address,
        "section header table",
    )?;
    let count = usize::try_from(header.section_count)
        .map_err(|_| XbeError::InvalidHeader("section count does not fit in usize"))?;
    let table_size = count
        .checked_mul(SECTION_HEADER_SIZE)
        .ok_or(XbeError::InvalidHeader("section table size overflow"))?;
    let table_end = table_offset
        .checked_add(table_size)
        .ok_or(XbeError::InvalidHeader("section table range overflow"))?;
    let header_end = usize::try_from(header.size_of_headers)
        .map_err(|_| XbeError::InvalidHeader("header size does not fit in usize"))?;
    if table_end > header_end {
        return Err(XbeError::InvalidHeader(
            "section header table extends past the declared headers",
        ));
    }
    if table_end > bytes.len() {
        return Err(XbeError::Truncated { field: "section header table", offset: table_offset });
    }

    let mut sections = Vec::with_capacity(count);
    for index in 0..header.section_count {
        let index_usize = usize::try_from(index)
            .map_err(|_| XbeError::InvalidHeader("section index does not fit in usize"))?;
        let offset = table_offset + index_usize * SECTION_HEADER_SIZE;
        let flags = XbeSectionFlags::from_bits_retain(read_u32(bytes, offset, "section flags")?);
        let virtual_address = GuestVa(read_u32(bytes, offset + 0x04, "section virtual address")?);
        let virtual_size = read_u32(bytes, offset + 0x08, "section virtual size")?;
        let raw_address = read_u32(bytes, offset + 0x0C, "section raw address")?;
        let raw_size = read_u32(bytes, offset + 0x10, "section raw size")?;
        let name_address = GuestVa(read_u32(bytes, offset + 0x14, "section name address")?);
        let reference_count = read_u32(bytes, offset + 0x18, "section reference count")?;
        let digest = read_array::<20>(bytes, offset + 0x24, "section digest")?;

        validate_section_ranges(
            bytes,
            index,
            virtual_address,
            virtual_size,
            raw_address,
            raw_size,
        )?;
        let name_offset = header_va_to_file_offset(header, name_address, "section name")?;
        let name = read_c_string(bytes, name_offset, 256, "section name")
            .map_err(|_| XbeError::InvalidSectionName { section_index: index })?
            .to_owned();

        sections.push(XbeSection {
            index,
            name,
            flags,
            virtual_address,
            virtual_size,
            raw_address,
            raw_size,
            name_address,
            reference_count,
            digest,
        });
    }

    Ok(sections)
}

fn validate_section_ranges(
    bytes: &[u8],
    index: u32,
    virtual_address: GuestVa,
    virtual_size: u32,
    raw_address: u32,
    raw_size: u32,
) -> Result<(), XbeError> {
    let raw_end = u64::from(raw_address) + u64::from(raw_size);
    if raw_end > bytes.len() as u64 {
        return Err(XbeError::SectionRawRange { section_index: index });
    }

    let virtual_end = u64::from(virtual_address.0) + u64::from(virtual_size);
    if virtual_end > u64::from(u32::MAX) + 1 {
        return Err(XbeError::SectionVirtualRange { section_index: index });
    }

    Ok(())
}

fn header_va_to_file_offset(
    header: &XbeHeader,
    address: GuestVa,
    field: &'static str,
) -> Result<usize, XbeError> {
    let relative = address
        .0
        .checked_sub(header.base_address.0)
        .ok_or(XbeError::HeaderAddressOutOfRange { field, address })?;
    if relative >= header.size_of_headers {
        return Err(XbeError::HeaderAddressOutOfRange { field, address });
    }
    usize::try_from(relative).map_err(|_| XbeError::HeaderAddressOutOfRange { field, address })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_retail_image() {
        let bytes = synthetic_xbe();
        let image = XbeImage::parse(&bytes).expect("synthetic XBE must parse");

        assert_eq!(image.header.build_flavor, BuildFlavor::Retail);
        assert_eq!(image.header.entry_point, GuestVa(0x0001_1000));
        assert_eq!(image.header.kernel_thunk_address, GuestVa(0x0001_1200));
        assert_eq!(image.sections.len(), 1);
        assert_eq!(image.sections[0].name, ".text");
        assert!(image.sections[0].flags.contains(XbeSectionFlags::EXECUTABLE));
        assert_eq!(
            image
                .section_data(&bytes, &image.sections[0])
                .expect("data exists"),
            [0x90, 0xC3]
        );
    }

    #[test]
    fn rejects_a_section_outside_the_file() {
        let mut bytes = synthetic_xbe();
        write_u32(&mut bytes, 0x200 + 0x10, 0x1000);
        let error = XbeImage::parse(&bytes).expect_err("section range must fail");
        assert!(matches!(error, XbeError::SectionRawRange { .. }));
    }

    #[test]
    fn rejects_a_section_table_outside_declared_headers() {
        let mut bytes = synthetic_xbe();
        bytes.resize(0x300, 0);
        let base = read_u32(&bytes, 0x104, "base address").expect("base must exist");
        write_u32(&mut bytes, 0x120, base + 0x250);

        let error = XbeImage::parse(&bytes).expect_err("section table must fail");
        assert!(matches!(error, XbeError::InvalidHeader(_)));
    }

    pub(crate) fn synthetic_xbe() -> Vec<u8> {
        let mut bytes = vec![0_u8; 0x282];
        bytes[..4].copy_from_slice(XBE_MAGIC);
        let base = 0x0001_0000_u32;
        write_u32(&mut bytes, 0x104, base);
        write_u32(&mut bytes, 0x108, 0x280);
        write_u32(&mut bytes, 0x10C, 0x4000);
        write_u32(&mut bytes, 0x110, 0x178);
        write_u32(&mut bytes, 0x118, base + 0x178);
        write_u32(&mut bytes, 0x11C, 1);
        write_u32(&mut bytes, 0x120, base + 0x200);
        write_u32(&mut bytes, 0x128, (base + 0x1000) ^ ENTRY_RETAIL_XOR);
        write_u32(&mut bytes, 0x130, 0x10000);
        write_u32(&mut bytes, 0x134, 0x100000);
        write_u32(&mut bytes, 0x138, 0x1000);
        write_u32(&mut bytes, 0x158, (base + 0x1200) ^ KERNEL_RETAIL_XOR);

        write_u32(&mut bytes, 0x200, XbeSectionFlags::EXECUTABLE.bits());
        write_u32(&mut bytes, 0x204, base + 0x1000);
        write_u32(&mut bytes, 0x208, 2);
        write_u32(&mut bytes, 0x20C, 0x280);
        write_u32(&mut bytes, 0x210, 2);
        write_u32(&mut bytes, 0x214, base + 0x238);
        bytes[0x238..0x23E].copy_from_slice(b".text\0");
        bytes[0x280..0x282].copy_from_slice(&[0x90, 0xC3]);
        bytes
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}
