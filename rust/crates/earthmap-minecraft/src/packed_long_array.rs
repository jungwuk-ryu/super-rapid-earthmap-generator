use crate::{MinecraftError, Result};

const LONG_BITS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedLongArray {
    value_count: usize,
    bits_per_value: u8,
    data: Vec<u64>,
    mask: u64,
}

impl PackedLongArray {
    pub fn pack(
        value_count: usize,
        bits_per_value: u8,
        mut values: impl FnMut(usize) -> i32,
    ) -> Result<Self> {
        require_shape(value_count, bits_per_value)?;
        let mask = mask_for(bits_per_value);
        let mut packed = vec![0u64; word_count(value_count, bits_per_value)?];
        if bits_per_value == 0 {
            for index in 0..value_count {
                require_value_fits(values(index), mask, bits_per_value)?;
            }
        } else {
            // Traverse each word's entries in their original order. Computing
            // a quotient and remainder for every block dominates small palettes.
            let per_word = values_per_word(bits_per_value);
            let mut index = 0;
            for word in &mut packed {
                let mut bit_offset = 0;
                for _ in 0..per_word.min(value_count - index) {
                    let value = values(index);
                    require_value_fits(value, mask, bits_per_value)?;
                    *word |= ((value as u64) & mask) << bit_offset;
                    bit_offset += usize::from(bits_per_value);
                    index += 1;
                }
            }
        }
        Self::from_words(value_count, bits_per_value, packed)
    }

    pub fn from_words(value_count: usize, bits_per_value: u8, data: Vec<u64>) -> Result<Self> {
        require_shape(value_count, bits_per_value)?;
        let expected_words = word_count(value_count, bits_per_value)?;
        if data.len() != expected_words {
            return Err(MinecraftError::invalid(format!(
                "data length {} must be {expected_words}",
                data.len()
            )));
        }
        Ok(Self {
            value_count,
            bits_per_value,
            data,
            mask: mask_for(bits_per_value),
        })
    }

    pub fn value_count(&self) -> usize {
        self.value_count
    }

    pub fn bits_per_value(&self) -> u8 {
        self.bits_per_value
    }

    pub fn data_word_count(&self) -> usize {
        self.data.len()
    }

    pub fn copy_data(&self) -> Vec<i64> {
        self.data.iter().map(|word| *word as i64).collect()
    }

    #[cfg(test)]
    fn copy_data_words(&self) -> Vec<u64> {
        self.data.clone()
    }

    pub fn get(&self, index: usize) -> Result<i32> {
        if index >= self.value_count {
            return Err(MinecraftError::invalid(format!(
                "index outside 0..{}: {index}",
                self.value_count.saturating_sub(1)
            )));
        }
        if self.bits_per_value == 0 {
            return Ok(0);
        }
        let values_per_word = values_per_word(self.bits_per_value);
        let word_index = index / values_per_word;
        let bit_offset = (index % values_per_word) * usize::from(self.bits_per_value);
        let value = self.data[word_index] >> bit_offset;
        Ok((value & self.mask) as i32)
    }
}

pub fn word_count(value_count: usize, bits_per_value: u8) -> Result<usize> {
    require_shape(value_count, bits_per_value)?;
    if bits_per_value == 0 || value_count == 0 {
        return Ok(0);
    }
    let values_per_word = values_per_word(bits_per_value);
    let words = value_count.div_ceil(values_per_word);
    if words > i32::MAX as usize {
        return Err(MinecraftError::invalid("packed array too large"));
    }
    Ok(words)
}

fn values_per_word(bits_per_value: u8) -> usize {
    if bits_per_value == 0 {
        0
    } else {
        LONG_BITS / usize::from(bits_per_value)
    }
}

fn require_shape(value_count: usize, bits_per_value: u8) -> Result<()> {
    if value_count > i32::MAX as usize {
        return Err(MinecraftError::invalid("packed array too large"));
    }
    if bits_per_value > 31 {
        return Err(MinecraftError::invalid(format!(
            "bitsPerValue must be in 0..31: {bits_per_value}"
        )));
    }
    Ok(())
}

fn require_value_fits(value: i32, mask: u64, bits_per_value: u8) -> Result<()> {
    if value < 0 || (value as u64) > mask {
        return Err(MinecraftError::invalid(format!(
            "value {value} does not fit in {bits_per_value} bits"
        )));
    }
    Ok(())
}

fn mask_for(bits_per_value: u8) -> u64 {
    if bits_per_value == 0 {
        0
    } else {
        (1u64 << bits_per_value) - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_counts_match_java_storage_shape() {
        assert_eq!(word_count(0, 9).unwrap(), 0);
        assert_eq!(word_count(256, 9).unwrap(), 37);
        assert_eq!(word_count(4096, 4).unwrap(), 256);
        assert_eq!(word_count(1, 0).unwrap(), 0);
        assert!(word_count((i32::MAX as usize) + 1, 1).is_err());
    }

    #[test]
    fn packs_values_little_endian_within_each_long_like_java() {
        let packed = PackedLongArray::pack(16, 4, |index| index as i32).unwrap();

        assert_eq!(packed.value_count(), 16);
        assert_eq!(packed.bits_per_value(), 4);
        assert_eq!(packed.data_word_count(), 1);
        assert_eq!(packed.copy_data_words()[0], 0xfedc_ba98_7654_3210);
        assert_eq!(packed.copy_data()[0], 0xfedc_ba98_7654_3210u64 as i64);
        for index in 0..16 {
            assert_eq!(packed.get(index).unwrap(), index as i32);
        }
    }

    #[test]
    fn heightmap_like_values_match_java_expectations() {
        let packed = PackedLongArray::pack(256, 9, |index| match index {
            0 => 1,
            1 => 65,
            255 => 384,
            _ => 0,
        })
        .unwrap();

        assert_eq!(packed.value_count(), 256);
        assert_eq!(packed.bits_per_value(), 9);
        assert_eq!(packed.data_word_count(), 37);
        assert_eq!(packed.get(0).unwrap(), 1);
        assert_eq!(packed.get(1).unwrap(), 65);
        assert_eq!(packed.get(255).unwrap(), 384);
    }

    #[test]
    fn java_section_palette_golden_packing_cases() {
        let zero = PackedLongArray::pack(4, 0, |_| 0).unwrap();
        assert_eq!(zero.value_count(), 4);
        assert_eq!(zero.bits_per_value(), 0);
        assert_eq!(zero.data_word_count(), 0);
        assert_eq!(zero.get(0).unwrap(), 0);
        assert_eq!(zero.get(3).unwrap(), 0);
        assert!(PackedLongArray::pack(1, 0, |_| 1).is_err());

        let nibble = PackedLongArray::pack(16, 4, |index| (index & 1) as i32).unwrap();
        assert_eq!(nibble.copy_data_words(), vec![0x1010_1010_1010_1010]);
        for index in 0..16 {
            assert_eq!(nibble.get(index).unwrap(), (index & 1) as i32);
        }

        let non_divisor =
            PackedLongArray::pack(13, 5, |index| if index == 12 { 31 } else { 0 }).unwrap();
        assert_eq!(non_divisor.copy_data_words(), vec![0, 0x1f]);
        assert_eq!(non_divisor.get(12).unwrap(), 31);

        let full_section = PackedLongArray::pack(4096, 5, |index| (index & 31) as i32).unwrap();
        assert_eq!(full_section.data_word_count(), 342);
        assert_eq!(full_section.get(0).unwrap(), 0);
        assert_eq!(full_section.get(11).unwrap(), 11);
        assert_eq!(full_section.get(12).unwrap(), 12);
        assert_eq!(full_section.get(4095).unwrap(), 31);
    }

    #[test]
    fn word_traversal_preserves_all_widths_padding_and_value_callback_order() {
        for bits in 0..=31 {
            for count in [0, 1, 13, 65, 256, 4096] {
                let mask = mask_for(bits);
                let mut calls = Vec::new();
                let packed = PackedLongArray::pack(count, bits, |index| {
                    calls.push(index);
                    ((index as u64 * 31) & mask) as i32
                })
                .unwrap();
                assert_eq!(calls, (0..count).collect::<Vec<_>>());
                let mut expected = vec![0_u64; word_count(count, bits).unwrap()];
                for index in 0..count {
                    let value = (index as u64 * 31) & mask;
                    assert_eq!(packed.get(index).unwrap(), value as i32);
                    if bits > 0 {
                        let per_word = 64 / usize::from(bits);
                        expected[index / per_word] |=
                            value << ((index % per_word) * usize::from(bits));
                    }
                }
                assert_eq!(packed.copy_data_words(), expected);
            }
        }
        let mut calls = Vec::new();
        let error = PackedLongArray::pack(65, 4, |index| {
            calls.push(index);
            if index == 17 {
                16
            } else {
                0
            }
        });
        assert!(error.is_err());
        assert_eq!(calls, (0..=17).collect::<Vec<_>>());
    }

    #[test]
    fn rejects_values_that_do_not_fit_java_rules() {
        assert!(PackedLongArray::pack(1, 4, |_| 16).is_err());
        assert!(PackedLongArray::pack(1, 4, |_| -1).is_err());
        assert!(PackedLongArray::pack(1, 32, |_| 0).is_err());
        assert!(PackedLongArray::from_words(2, 4, vec![]).is_err());
        assert!(PackedLongArray::from_words((i32::MAX as usize) + 1, 1, vec![]).is_err());
    }
}
