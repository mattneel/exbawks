use crate::XbeError;

pub(crate) fn read_u32(bytes: &[u8], offset: usize, field: &'static str) -> Result<u32, XbeError> {
    let value = read_array::<4>(bytes, offset, field)?;
    Ok(u32::from_le_bytes(value))
}

pub(crate) fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
    field: &'static str,
) -> Result<[u8; N], XbeError> {
    let end = offset.checked_add(N).ok_or(XbeError::Truncated { field, offset })?;
    let slice = bytes.get(offset..end).ok_or(XbeError::Truncated { field, offset })?;
    let mut value = [0_u8; N];
    value.copy_from_slice(slice);
    Ok(value)
}

pub(crate) fn read_c_string(
    bytes: &[u8],
    offset: usize,
    maximum_len: usize,
    field: &'static str,
) -> Result<&str, XbeError> {
    let available = bytes.get(offset..).ok_or(XbeError::Truncated { field, offset })?;
    let bounded = &available[..available.len().min(maximum_len)];
    let end = bounded.iter().position(|byte| *byte == 0).unwrap_or(bounded.len());
    std::str::from_utf8(&bounded[..end])
        .map_err(|_| XbeError::InvalidHeader("invalid UTF-8 string"))
}
