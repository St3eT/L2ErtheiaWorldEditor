//! Unreal Engine 2 binary primitives.
//!
//! Decoded values retain their original representation. `write_to` uses those
//! bytes until `set` is called, making an unchanged decode/encode byte-exact.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompactInt {
    value: i32,
    original: Option<Vec<u8>>,
}

impl CompactInt {
    pub fn new(value: i32) -> Self {
        Self {
            value,
            original: None,
        }
    }
    pub fn get(&self) -> i32 {
        self.value
    }
    pub fn set(&mut self, value: i32) {
        self.value = value;
        self.original = None;
    }

    pub fn read(reader: &mut BinaryReader<'_>) -> Result<Self, CodecError> {
        let start = reader.position();
        let first = reader.u8()?;
        let negative = first & 0x80 != 0;
        let mut value = (first & 0x3f) as u32;
        if first & 0x40 != 0 {
            for i in 1..5 {
                let byte = reader.u8()?;
                let mask = if i == 4 { 0x1f } else { 0x7f };
                value |= ((byte & mask) as u32) << (6 + (i - 1) * 7);
                if i == 4 || byte & 0x80 == 0 {
                    break;
                }
            }
        }
        let value = if negative {
            if value == 0 {
                i32::MIN
            } else {
                -(value as i32)
            }
        } else {
            value as i32
        };
        Ok(Self {
            value,
            original: Some(reader.data[start..reader.position()].to_vec()),
        })
    }

    pub fn write(&self, writer: &mut BinaryWriter) {
        if let Some(original) = &self.original {
            writer.bytes(original);
        } else {
            writer.bytes(&encode_compact(self.value));
        }
    }
}

pub trait UnrealRead: Sized {
    fn read_unreal(reader: &mut BinaryReader<'_>) -> Result<Self, CodecError>;
}

pub trait UnrealWrite {
    fn write_unreal(&self, writer: &mut BinaryWriter) -> Result<(), CodecError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[repr(u8)]
pub enum PropertyType {
    None = 0,
    Byte,
    Int,
    Bool,
    Float,
    Object,
    Name,
    Delegate,
    Class,
    Array,
    Struct,
    Vector,
    Rotator,
    String,
    Map,
    FixedArray,
}

impl PropertyType {
    fn from_info(info: u8) -> Self {
        use PropertyType::*;
        [
            None, Byte, Int, Bool, Float, Object, Name, Delegate, Class, Array, Struct, Vector,
            Rotator, String, Map, FixedArray,
        ][(info & 0x0f) as usize]
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PropertyRecord {
    End(CompactInt),
    Value(PropertyTag),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PropertyTag {
    pub name: CompactInt,
    pub property_type: PropertyType,
    pub struct_name: Option<CompactInt>,
    pub array_index: Option<CompactInt>,
    value: Vec<u8>,
    original_header: Option<Vec<u8>>,
}

impl PropertyTag {
    pub fn value(&self) -> &[u8] {
        &self.value
    }
    pub fn set_value(&mut self, value: Vec<u8>) {
        self.value = value;
        self.original_header = None;
    }

    fn read_after_name(
        name: CompactInt,
        reader: &mut BinaryReader<'_>,
    ) -> Result<Self, CodecError> {
        let header_start = reader.position();
        let info = reader.u8()?;
        let property_type = PropertyType::from_info(info);
        let struct_name = if property_type == PropertyType::Struct {
            Some(CompactInt::read(reader)?)
        } else {
            None
        };
        let size = match (info >> 4) & 7 {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 12,
            4 => 16,
            5 => reader.u8()? as usize,
            6 => reader.u16()? as usize,
            7 => reader.u32()? as usize,
            _ => unreachable!(),
        };
        let array_index = if info & 0x80 != 0 && property_type != PropertyType::Bool {
            Some(CompactInt::read(reader)?)
        } else {
            None
        };
        let original_header = reader.data[header_start..reader.position()].to_vec();
        let value = reader.take(size)?.to_vec();
        Ok(Self {
            name,
            property_type,
            struct_name,
            array_index,
            value,
            original_header: Some(original_header),
        })
    }

    fn write(&self, writer: &mut BinaryWriter) {
        self.name.write(writer);
        if let Some(header) = &self.original_header {
            writer.bytes(header);
        } else {
            let (size_code, extra) = encode_property_size(self.value.len());
            let mut info = self.property_type as u8 | size_code << 4;
            if self.array_index.is_some() || self.property_type == PropertyType::Bool {
                info |= 0x80;
            }
            writer.bytes(&[info]);
            if let Some(name) = &self.struct_name {
                name.write(writer);
            }
            writer.bytes(&extra);
            if let Some(index) = &self.array_index {
                index.write(writer);
            }
        }
        writer.bytes(&self.value);
    }
}

impl PropertyRecord {
    pub fn read(reader: &mut BinaryReader<'_>, none_name_index: i32) -> Result<Self, CodecError> {
        let name = CompactInt::read(reader)?;
        if name.get() == none_name_index {
            Ok(Self::End(name))
        } else {
            Ok(Self::Value(PropertyTag::read_after_name(name, reader)?))
        }
    }
}

impl UnrealWrite for PropertyRecord {
    fn write_unreal(&self, writer: &mut BinaryWriter) -> Result<(), CodecError> {
        match self {
            Self::End(name) => name.write(writer),
            Self::Value(tag) => tag.write(writer),
        }
        Ok(())
    }
}

impl UnrealRead for CompactInt {
    fn read_unreal(reader: &mut BinaryReader<'_>) -> Result<Self, CodecError> {
        Self::read(reader)
    }
}

impl UnrealWrite for CompactInt {
    fn write_unreal(&self, writer: &mut BinaryWriter) -> Result<(), CodecError> {
        self.write(writer);
        Ok(())
    }
}

pub struct BinaryReader<'a> {
    pub(crate) data: &'a [u8],
    position: usize,
}

impl<'a> BinaryReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }
    pub fn position(&self) -> usize {
        self.position
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.position
    }
    pub fn u8(&mut self) -> Result<u8, CodecError> {
        let value = *self
            .data
            .get(self.position)
            .ok_or(CodecError::UnexpectedEnd {
                offset: self.position,
            })?;
        self.position += 1;
        Ok(value)
    }
    pub fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32, CodecError> {
        Ok(self.u32()? as i32)
    }
    pub fn f32(&mut self) -> Result<f32, CodecError> {
        Ok(f32::from_bits(self.u32()?))
    }
    pub fn compact_int(&mut self) -> Result<CompactInt, CodecError> {
        CompactInt::read(self)
    }
    pub fn compact_count(&mut self) -> Result<usize, CodecError> {
        usize::try_from(self.compact_int()?.get()).map_err(|_| CodecError::NegativeCount)
    }
    pub fn take(&mut self, length: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(CodecError::OffsetOverflow)?;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or(CodecError::UnexpectedEnd {
                offset: self.position,
            })?;
        self.position = end;
        Ok(bytes)
    }
}

#[derive(Default)]
pub struct BinaryWriter {
    data: Vec<u8>,
}

impl BinaryWriter {
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
    }
    pub fn position(&self) -> usize {
        self.data.len()
    }
    pub fn u8(&mut self, value: u8) {
        self.data.push(value);
    }
    pub fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }
    pub fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }
    pub fn i32(&mut self, value: i32) {
        self.bytes(&value.to_le_bytes());
    }
    pub fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }
    pub fn compact(&mut self, value: i32) {
        CompactInt::new(value).write(self);
    }
    pub fn patch_i32(&mut self, position: usize, value: i32) {
        self.data[position..position + 4].copy_from_slice(&value.to_le_bytes());
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodecError {
    UnexpectedEnd { offset: usize },
    OffsetOverflow,
    NegativeCount,
}

pub(crate) fn encode_property_size(size: usize) -> (u8, Vec<u8>) {
    match size {
        1 => (0, vec![]),
        2 => (1, vec![]),
        4 => (2, vec![]),
        12 => (3, vec![]),
        16 => (4, vec![]),
        n if n < 0x100 => (5, vec![n as u8]),
        n if n < 0x10000 => (6, (n as u16).to_le_bytes().to_vec()),
        n => (7, (n as u32).to_le_bytes().to_vec()),
    }
}

fn encode_compact(value: i32) -> Vec<u8> {
    let negative = value < 0;
    let mut magnitude = if value == i32::MIN {
        0u32
    } else {
        value.unsigned_abs()
    };
    let mut first = (magnitude & 0x3f) as u8;
    magnitude >>= 6;
    if negative {
        first |= 0x80;
    }
    if magnitude != 0 {
        first |= 0x40;
    }
    let mut bytes = vec![first];
    while magnitude != 0 && bytes.len() < 5 {
        let mut byte = (magnitude & if bytes.len() == 4 { 0x1f } else { 0x7f }) as u8;
        magnitude >>= 7;
        if magnitude != 0 && bytes.len() < 4 {
            byte |= 0x80;
        }
        bytes.push(byte);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_compact_int_is_byte_exact() {
        let original = [0x40, 0x01];
        let value = CompactInt::read(&mut BinaryReader::new(&original)).unwrap();
        let mut writer = BinaryWriter::default();
        value.write(&mut writer);
        assert_eq!(writer.into_bytes(), original);
    }

    #[test]
    fn edit_invalidates_only_original_encoding() {
        let mut value = CompactInt::read(&mut BinaryReader::new(&[0x40, 0x01])).unwrap();
        value.set(63);
        let mut writer = BinaryWriter::default();
        value.write(&mut writer);
        assert_eq!(writer.into_bytes(), [0x3f]);
    }

    #[test]
    fn compact_integer_extremes_round_trip() {
        for expected in [i32::MIN, -1, 0, 1, 63, 64, i32::MAX] {
            let encoded = encode_compact(expected);
            let decoded = CompactInt::read(&mut BinaryReader::new(&encoded)).unwrap();
            assert_eq!(decoded.get(), expected);
        }
    }

    #[test]
    fn unchanged_property_is_byte_exact() {
        // name=1, Float with fixed 4-byte size, value=1.5, None name=0
        let original = [1, 0x24, 0, 0, 0xc0, 0x3f, 0];
        let mut reader = BinaryReader::new(&original);
        let property = PropertyRecord::read(&mut reader, 0).unwrap();
        let end = PropertyRecord::read(&mut reader, 0).unwrap();
        let mut writer = BinaryWriter::default();
        property.write_unreal(&mut writer).unwrap();
        end.write_unreal(&mut writer).unwrap();
        assert_eq!(writer.into_bytes(), original);
    }

    #[test]
    fn edited_property_recomputes_size_header() {
        let original = [1, 0x24, 0, 0, 0xc0, 0x3f];
        let mut property = match PropertyRecord::read(&mut BinaryReader::new(&original), 0).unwrap()
        {
            PropertyRecord::Value(value) => value,
            _ => unreachable!(),
        };
        property.set_value(vec![7]);
        let mut writer = BinaryWriter::default();
        PropertyRecord::Value(property)
            .write_unreal(&mut writer)
            .unwrap();
        assert_eq!(writer.into_bytes(), [1, 0x04, 7]);
    }

    #[test]
    fn property_records_round_trip_through_serde_json() {
        let original = [1, 0x24, 0, 0, 0xc0, 0x3f];
        let property = PropertyRecord::read(&mut BinaryReader::new(&original), 0).unwrap();
        let json = serde_json::to_string(&property).unwrap();
        let decoded: PropertyRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, property);
    }
}
