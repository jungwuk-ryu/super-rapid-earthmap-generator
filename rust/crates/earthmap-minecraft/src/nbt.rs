use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::{MinecraftError, Result};

pub const TAG_END: u8 = 0;
pub const TAG_BYTE: u8 = 1;
pub const TAG_SHORT: u8 = 2;
pub const TAG_INT: u8 = 3;
pub const TAG_LONG: u8 = 4;
pub const TAG_FLOAT: u8 = 5;
pub const TAG_DOUBLE: u8 = 6;
pub const TAG_BYTE_ARRAY: u8 = 7;
pub const TAG_STRING: u8 = 8;
pub const TAG_LIST: u8 = 9;
pub const TAG_COMPOUND: u8 = 10;
pub const TAG_INT_ARRAY: u8 = 11;
pub const TAG_LONG_ARRAY: u8 = 12;

#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(ListTag),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn type_id(&self) -> u8 {
        match self {
            Tag::Byte(_) => TAG_BYTE,
            Tag::Short(_) => TAG_SHORT,
            Tag::Int(_) => TAG_INT,
            Tag::Long(_) => TAG_LONG,
            Tag::Float(_) => TAG_FLOAT,
            Tag::Double(_) => TAG_DOUBLE,
            Tag::ByteArray(_) => TAG_BYTE_ARRAY,
            Tag::String(_) => TAG_STRING,
            Tag::List(_) => TAG_LIST,
            Tag::Compound(_) => TAG_COMPOUND,
            Tag::IntArray(_) => TAG_INT_ARRAY,
            Tag::LongArray(_) => TAG_LONG_ARRAY,
        }
    }

    pub(crate) fn write_payload(&self, output: &mut impl Write) -> Result<()> {
        match self {
            Tag::Byte(value) => write_i8(output, *value),
            Tag::Short(value) => write_i16(output, *value),
            Tag::Int(value) => write_i32(output, *value),
            Tag::Long(value) => write_i64(output, *value),
            Tag::Float(value) => write_u32(output, value.to_bits()),
            Tag::Double(value) => write_u64(output, value.to_bits()),
            Tag::ByteArray(values) => {
                write_len_i32(output, values.len(), "NBT byte array")?;
                for value in values {
                    write_i8(output, *value)?;
                }
                Ok(())
            }
            Tag::String(value) => write_utf(output, value),
            Tag::List(list) => list.write_payload(output),
            Tag::Compound(compound) => compound.write_payload(output),
            Tag::IntArray(values) => {
                write_len_i32(output, values.len(), "NBT int array")?;
                for value in values {
                    write_i32(output, *value)?;
                }
                Ok(())
            }
            Tag::LongArray(values) => {
                write_len_i32(output, values.len(), "NBT long array")?;
                for value in values {
                    write_i64(output, *value)?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Compound {
    entries: Vec<(String, Tag)>,
}

impl Compound {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn put(&mut self, name: impl Into<String>, tag: Tag) -> Result<&mut Self> {
        let name = name.into();
        require_name(&name)?;
        if let Some((_, existing)) = self
            .entries
            .iter_mut()
            .find(|(existing_name, _)| existing_name == &name)
        {
            *existing = tag;
        } else {
            self.entries.push((name, tag));
        }
        Ok(self)
    }

    pub fn put_byte(&mut self, name: impl Into<String>, value: i32) -> Result<&mut Self> {
        self.put(name, Tag::Byte(value as i8))
    }

    pub fn put_short(&mut self, name: impl Into<String>, value: i32) -> Result<&mut Self> {
        self.put(name, Tag::Short(value as i16))
    }

    pub fn put_int(&mut self, name: impl Into<String>, value: i32) -> Result<&mut Self> {
        self.put(name, Tag::Int(value))
    }

    pub fn put_long(&mut self, name: impl Into<String>, value: i64) -> Result<&mut Self> {
        self.put(name, Tag::Long(value))
    }

    pub fn put_float(&mut self, name: impl Into<String>, value: f32) -> Result<&mut Self> {
        self.put(name, Tag::Float(value))
    }

    pub fn put_double(&mut self, name: impl Into<String>, value: f64) -> Result<&mut Self> {
        self.put(name, Tag::Double(value))
    }

    pub fn put_string(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<&mut Self> {
        self.put(name, Tag::String(value.into()))
    }

    pub fn put_byte_array(&mut self, name: impl Into<String>, value: Vec<i8>) -> Result<&mut Self> {
        self.put(name, Tag::ByteArray(value))
    }

    pub fn put_int_array(&mut self, name: impl Into<String>, value: Vec<i32>) -> Result<&mut Self> {
        self.put(name, Tag::IntArray(value))
    }

    pub fn put_long_array(
        &mut self,
        name: impl Into<String>,
        value: Vec<i64>,
    ) -> Result<&mut Self> {
        self.put(name, Tag::LongArray(value))
    }

    pub fn put_compound(&mut self, name: impl Into<String>, value: Compound) -> Result<&mut Self> {
        self.put(name, Tag::Compound(value))
    }

    pub fn remove(&mut self, name: &str) -> Result<&mut Self> {
        require_name(name)?;
        self.entries
            .retain(|(existing_name, _)| existing_name != name);
        Ok(self)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|(existing_name, _)| existing_name == name)
    }

    pub fn get(&self, name: &str) -> Result<&Tag> {
        self.entries
            .iter()
            .find(|(existing_name, _)| existing_name == name)
            .map(|(_, tag)| tag)
            .ok_or_else(|| MinecraftError::invalid(format!("missing NBT tag: {name}")))
    }

    pub fn get_compound(&self, name: &str) -> Result<&Compound> {
        match self.get(name)? {
            Tag::Compound(value) => Ok(value),
            tag => Err(wrong_type(name, TAG_COMPOUND, tag.type_id())),
        }
    }

    pub fn get_list(&self, name: &str) -> Result<&ListTag> {
        match self.get(name)? {
            Tag::List(value) => Ok(value),
            tag => Err(wrong_type(name, TAG_LIST, tag.type_id())),
        }
    }

    pub fn get_int(&self, name: &str) -> Result<i32> {
        match self.get(name)? {
            Tag::Int(value) => Ok(*value),
            tag => Err(wrong_type(name, TAG_INT, tag.type_id())),
        }
    }

    pub fn get_long(&self, name: &str) -> Result<i64> {
        match self.get(name)? {
            Tag::Long(value) => Ok(*value),
            tag => Err(wrong_type(name, TAG_LONG, tag.type_id())),
        }
    }

    pub fn get_byte(&self, name: &str) -> Result<i8> {
        match self.get(name)? {
            Tag::Byte(value) => Ok(*value),
            tag => Err(wrong_type(name, TAG_BYTE, tag.type_id())),
        }
    }

    pub fn get_string(&self, name: &str) -> Result<&str> {
        match self.get(name)? {
            Tag::String(value) => Ok(value),
            tag => Err(wrong_type(name, TAG_STRING, tag.type_id())),
        }
    }

    pub fn get_long_array(&self, name: &str) -> Result<Vec<i64>> {
        match self.get(name)? {
            Tag::LongArray(value) => Ok(value.clone()),
            tag => Err(wrong_type(name, TAG_LONG_ARRAY, tag.type_id())),
        }
    }

    pub fn entries(&self) -> &[(String, Tag)] {
        &self.entries
    }

    pub(crate) fn write_payload(&self, output: &mut impl Write) -> Result<()> {
        for (name, tag) in &self.entries {
            write_u8(output, tag.type_id())?;
            write_utf(output, name)?;
            tag.write_payload(output)?;
        }
        write_u8(output, TAG_END)
    }
}

impl Default for Compound {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListTag {
    element_type: u8,
    values: Vec<Tag>,
}

impl ListTag {
    pub fn new(element_type: u8, values: Vec<Tag>) -> Result<Self> {
        if element_type == TAG_END && !values.is_empty() {
            return Err(MinecraftError::invalid(
                "non-empty NBT list must not use TAG_End",
            ));
        }
        for value in &values {
            if value.type_id() != element_type {
                return Err(MinecraftError::invalid("NBT list element type mismatch"));
            }
        }
        Ok(Self {
            element_type,
            values,
        })
    }

    pub fn element_type(&self) -> u8 {
        self.element_type
    }

    pub fn values(&self) -> &[Tag] {
        &self.values
    }

    fn write_payload(&self, output: &mut impl Write) -> Result<()> {
        write_u8(output, self.element_type)?;
        write_len_i32(output, self.values.len(), "NBT list")?;
        for value in &self.values {
            value.write_payload(output)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NamedTag {
    name: String,
    tag: Tag,
}

impl NamedTag {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tag(&self) -> &Tag {
        &self.tag
    }

    pub fn into_tag(self) -> Tag {
        self.tag
    }
}

pub fn compound() -> Compound {
    Compound::new()
}

pub fn byte_tag(value: i32) -> Tag {
    Tag::Byte(value as i8)
}

pub fn short_tag(value: i32) -> Tag {
    Tag::Short(value as i16)
}

pub fn int_tag(value: i32) -> Tag {
    Tag::Int(value)
}

pub fn long_tag(value: i64) -> Tag {
    Tag::Long(value)
}

pub fn float_tag(value: f32) -> Tag {
    Tag::Float(value)
}

pub fn double_tag(value: f64) -> Tag {
    Tag::Double(value)
}

pub fn string_tag(value: impl Into<String>) -> Tag {
    Tag::String(value.into())
}

pub fn byte_array(value: Vec<i8>) -> Tag {
    Tag::ByteArray(value)
}

pub fn int_array(value: Vec<i32>) -> Tag {
    Tag::IntArray(value)
}

pub fn long_array(value: Vec<i64>) -> Tag {
    Tag::LongArray(value)
}

pub fn list(element_type: u8, values: Vec<Tag>) -> Result<ListTag> {
    ListTag::new(element_type, values)
}

pub fn write_to_bytes(root_name: &str, root: &Compound) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    write(&mut bytes, root_name, root)?;
    Ok(bytes)
}

pub fn write(output: &mut impl Write, root_name: &str, root: &Compound) -> Result<()> {
    write_u8(output, TAG_COMPOUND)?;
    write_utf(output, root_name)?;
    root.write_payload(output)
}

pub fn read_from_bytes(bytes: &[u8]) -> Result<NamedTag> {
    read(&mut io::Cursor::new(bytes))
}

pub fn read(input: &mut impl Read) -> Result<NamedTag> {
    let root_type = read_u8(input)?;
    if root_type == TAG_END {
        return Err(MinecraftError::invalid("root NBT tag must not be TAG_End"));
    }
    let root_name = read_utf(input)?;
    let tag = read_payload(root_type, input)?;
    Ok(NamedTag {
        name: root_name,
        tag,
    })
}

pub fn write_gzip(path: impl AsRef<Path>, root_name: &str, root: &Compound) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(path)?;
    let buffered = BufWriter::new(file);
    let mut gzip = GzEncoder::new(buffered, Compression::default());
    write(&mut gzip, root_name, root)?;
    let mut buffered = gzip.finish()?;
    buffered.flush()?;
    Ok(())
}

pub fn read_gzip(path: impl AsRef<Path>) -> Result<NamedTag> {
    let file = File::open(path)?;
    let buffered = BufReader::new(file);
    let mut gzip = GzDecoder::new(buffered);
    read(&mut gzip)
}

fn read_payload(type_id: u8, input: &mut impl Read) -> Result<Tag> {
    match type_id {
        TAG_BYTE => Ok(Tag::Byte(read_i8(input)?)),
        TAG_SHORT => Ok(Tag::Short(read_i16(input)?)),
        TAG_INT => Ok(Tag::Int(read_i32(input)?)),
        TAG_LONG => Ok(Tag::Long(read_i64(input)?)),
        TAG_FLOAT => Ok(Tag::Float(f32::from_bits(read_u32(input)?))),
        TAG_DOUBLE => Ok(Tag::Double(f64::from_bits(read_u64(input)?))),
        TAG_BYTE_ARRAY => read_byte_array(input),
        TAG_STRING => Ok(Tag::String(read_utf(input)?)),
        TAG_LIST => read_list(input),
        TAG_COMPOUND => read_compound(input),
        TAG_INT_ARRAY => read_int_array(input),
        TAG_LONG_ARRAY => read_long_array(input),
        _ => Err(MinecraftError::invalid(format!(
            "unsupported NBT tag type: {type_id}"
        ))),
    }
}

fn read_compound(input: &mut impl Read) -> Result<Tag> {
    let mut compound = Compound::new();
    loop {
        let type_id = read_u8(input)?;
        if type_id == TAG_END {
            return Ok(Tag::Compound(compound));
        }
        let name = read_utf(input)?;
        let tag = read_payload(type_id, input)?;
        compound.put(name, tag)?;
    }
}

fn read_list(input: &mut impl Read) -> Result<Tag> {
    let element_type = read_u8(input)?;
    let size = read_len_i32(input, "NBT list")?;
    let mut values = Vec::with_capacity(size);
    for _ in 0..size {
        values.push(read_payload(element_type, input)?);
    }
    Ok(Tag::List(ListTag::new(element_type, values)?))
}

fn read_byte_array(input: &mut impl Read) -> Result<Tag> {
    let size = read_len_i32(input, "NBT byte array")?;
    let mut values = vec![0u8; size];
    input.read_exact(&mut values)?;
    Ok(Tag::ByteArray(
        values.into_iter().map(|value| value as i8).collect(),
    ))
}

fn read_int_array(input: &mut impl Read) -> Result<Tag> {
    let size = read_len_i32(input, "NBT int array")?;
    let mut values = Vec::with_capacity(size);
    for _ in 0..size {
        values.push(read_i32(input)?);
    }
    Ok(Tag::IntArray(values))
}

fn read_long_array(input: &mut impl Read) -> Result<Tag> {
    let size = read_len_i32(input, "NBT long array")?;
    let mut values = Vec::with_capacity(size);
    for _ in 0..size {
        values.push(read_i64(input)?);
    }
    Ok(Tag::LongArray(values))
}

fn write_len_i32(output: &mut impl Write, len: usize, what: &str) -> Result<()> {
    if len > i32::MAX as usize {
        return Err(MinecraftError::invalid(format!("{what} too large")));
    }
    write_i32(output, len as i32)
}

fn read_len_i32(input: &mut impl Read, what: &str) -> Result<usize> {
    let size = read_i32(input)?;
    if size < 0 {
        return Err(MinecraftError::invalid(format!(
            "negative {what} size: {size}"
        )));
    }
    Ok(size as usize)
}

pub(crate) fn write_utf(output: &mut impl Write, value: &str) -> Result<()> {
    if value.is_ascii() && !value.as_bytes().contains(&0) {
        let length = u16::try_from(value.len())
            .map_err(|_| MinecraftError::invalid("encoded UTF string too long"))?;
        write_u16(output, length)?;
        output.write_all(value.as_bytes())?;
        return Ok(());
    }
    let bytes = modified_utf8_bytes(value);
    if bytes.len() > u16::MAX as usize {
        return Err(MinecraftError::invalid("encoded UTF string too long"));
    }
    write_u16(output, bytes.len() as u16)?;
    output.write_all(&bytes)?;
    Ok(())
}

fn read_utf(input: &mut impl Read) -> Result<String> {
    let len = read_u16(input)? as usize;
    let mut bytes = vec![0u8; len];
    input.read_exact(&mut bytes)?;
    decode_modified_utf8(&bytes)
}

fn modified_utf8_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for code_unit in value.encode_utf16() {
        match code_unit {
            0x0001..=0x007f => bytes.push(code_unit as u8),
            0x0000..=0x07ff => {
                bytes.push((0xc0 | ((code_unit >> 6) & 0x1f)) as u8);
                bytes.push((0x80 | (code_unit & 0x3f)) as u8);
            }
            _ => {
                bytes.push((0xe0 | ((code_unit >> 12) & 0x0f)) as u8);
                bytes.push((0x80 | ((code_unit >> 6) & 0x3f)) as u8);
                bytes.push((0x80 | (code_unit & 0x3f)) as u8);
            }
        }
    }
    bytes
}

fn decode_modified_utf8(bytes: &[u8]) -> Result<String> {
    let mut code_units = Vec::<u16>::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte >> 7 == 0 {
            code_units.push(u16::from(byte));
            index += 1;
        } else if byte >> 5 == 0b110 {
            if index + 1 >= bytes.len() {
                return Err(MinecraftError::invalid("truncated modified UTF-8 sequence"));
            }
            let byte2 = bytes[index + 1];
            if byte2 >> 6 != 0b10 {
                return Err(MinecraftError::invalid(
                    "invalid modified UTF-8 continuation",
                ));
            }
            code_units.push((u16::from(byte & 0x1f) << 6) | u16::from(byte2 & 0x3f));
            index += 2;
        } else if byte >> 4 == 0b1110 {
            if index + 2 >= bytes.len() {
                return Err(MinecraftError::invalid("truncated modified UTF-8 sequence"));
            }
            let byte2 = bytes[index + 1];
            let byte3 = bytes[index + 2];
            if byte2 >> 6 != 0b10 || byte3 >> 6 != 0b10 {
                return Err(MinecraftError::invalid(
                    "invalid modified UTF-8 continuation",
                ));
            }
            code_units.push(
                (u16::from(byte & 0x0f) << 12)
                    | (u16::from(byte2 & 0x3f) << 6)
                    | u16::from(byte3 & 0x3f),
            );
            index += 3;
        } else {
            return Err(MinecraftError::invalid(
                "invalid modified UTF-8 leading byte",
            ));
        }
    }
    String::from_utf16(&code_units)
        .map_err(|error| MinecraftError::invalid(format!("invalid modified UTF-8 string: {error}")))
}

fn require_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(MinecraftError::invalid("NBT name must not be blank"));
    }
    Ok(())
}

fn wrong_type(name: &str, expected: u8, actual: u8) -> MinecraftError {
    MinecraftError::invalid(format!(
        "NBT tag {name} expected type {expected} but got {actual}"
    ))
}

fn write_u8(output: &mut impl Write, value: u8) -> Result<()> {
    output.write_all(&[value])?;
    Ok(())
}

fn write_i8(output: &mut impl Write, value: i8) -> Result<()> {
    write_u8(output, value as u8)
}

fn write_u16(output: &mut impl Write, value: u16) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn write_i16(output: &mut impl Write, value: i16) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn write_u32(output: &mut impl Write, value: u32) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn write_i32(output: &mut impl Write, value: i32) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn write_u64(output: &mut impl Write, value: u64) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn write_i64(output: &mut impl Write, value: i64) -> Result<()> {
    output.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn read_u8(input: &mut impl Read) -> Result<u8> {
    let mut bytes = [0u8; 1];
    input.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn read_i8(input: &mut impl Read) -> Result<i8> {
    Ok(read_u8(input)? as i8)
}

fn read_u16(input: &mut impl Read) -> Result<u16> {
    let mut bytes = [0u8; 2];
    input.read_exact(&mut bytes)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_i16(input: &mut impl Read) -> Result<i16> {
    let mut bytes = [0u8; 2];
    input.read_exact(&mut bytes)?;
    Ok(i16::from_be_bytes(bytes))
}

fn read_u32(input: &mut impl Read) -> Result<u32> {
    let mut bytes = [0u8; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_i32(input: &mut impl Read) -> Result<i32> {
    let mut bytes = [0u8; 4];
    input.read_exact(&mut bytes)?;
    Ok(i32::from_be_bytes(bytes))
}

fn read_u64(input: &mut impl Read) -> Result<u64> {
    let mut bytes = [0u8; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

fn read_i64(input: &mut impl Read) -> Result<i64> {
    let mut bytes = [0u8; 8];
    input.read_exact(&mut bytes)?;
    Ok(i64::from_be_bytes(bytes))
}

impl From<io::Error> for MinecraftError {
    fn from(error: io::Error) -> Self {
        MinecraftError::invalid(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_simple_compound_bytes_like_java_data_output_stream() {
        let mut root = Compound::new();
        root.put_int("A", 123).unwrap();

        let bytes = write_to_bytes("", &root).unwrap();

        assert_eq!(
            bytes,
            vec![0x0a, 0x00, 0x00, 0x03, 0x00, 0x01, b'A', 0x00, 0x00, 0x00, 0x7b, 0x00]
        );
    }

    #[test]
    fn primitive_round_trip_matches_java_leveldat_test_contract() {
        let mut root = Compound::new();
        root.put_byte("Byte", 1)
            .unwrap()
            .put_int("Int", 123)
            .unwrap()
            .put_long("Long", 456)
            .unwrap()
            .put_string("String", "hello")
            .unwrap()
            .put_long_array("LongArray", vec![7, 8])
            .unwrap();

        let path = std::env::temp_dir().join(format!(
            "earthmap-rust-nbt-{}-primitive.dat",
            std::process::id()
        ));
        write_gzip(&path, "root", &root).unwrap();
        let read = read_gzip(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(read.name(), "root");
        let Tag::Compound(compound) = read.tag() else {
            panic!("root tag must be compound");
        };
        assert_eq!(compound.get_byte("Byte").unwrap(), 1);
        assert_eq!(compound.get_int("Int").unwrap(), 123);
        assert_eq!(compound.get_long("Long").unwrap(), 456);
        assert_eq!(compound.get_string("String").unwrap(), "hello");
        assert_eq!(compound.get_long_array("LongArray").unwrap(), vec![7, 8]);
    }

    #[test]
    fn modified_utf8_matches_java_write_utf_null_and_supplementary_cases() {
        assert_eq!(modified_utf8_bytes("\0"), vec![0xc0, 0x80]);
        assert_eq!(modified_utf8_bytes("A"), vec![0x41]);
        assert_eq!(decode_modified_utf8(&[0xc0, 0x80]).unwrap(), "\0");

        let musical_symbol = "\u{1d11e}";
        let encoded = modified_utf8_bytes(musical_symbol);
        assert_eq!(encoded.len(), 6);
        assert_eq!(decode_modified_utf8(&encoded).unwrap(), musical_symbol);
    }

    #[test]
    fn compound_replaces_existing_tag_without_reordering_like_linked_hash_map() {
        let mut root = Compound::new();
        root.put_int("A", 1)
            .unwrap()
            .put_int("B", 2)
            .unwrap()
            .put_int("A", 3)
            .unwrap();

        assert_eq!(root.entries()[0].0, "A");
        assert_eq!(root.entries()[1].0, "B");
        assert_eq!(root.get_int("A").unwrap(), 3);
    }

    #[test]
    fn validation_matches_java_nbt_tests() {
        assert!(Compound::new().put_string("", "bad").is_err());
        assert!(ListTag::new(TAG_INT, vec![string_tag("bad")]).is_err());
        assert!(ListTag::new(TAG_END, vec![byte_tag(1)]).is_err());
        assert!(read_from_bytes(&[TAG_END]).is_err());
    }
}
