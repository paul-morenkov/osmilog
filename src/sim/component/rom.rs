use std::cell::RefCell;
use std::rc::Rc;

use super::CombLogic;
use crate::sim::value::Value;

// A ROM carries bulk state (`data`) but stays combinational: evaluate() is a
// pure read, and contents only change through an explicit GUI edit
// (Circuit::write_rom). `data` is an Rc<RefCell<..>> so the placed spec and
// the live component can share one buffer without duplicating up to 64 MiB
// (see Rom::shared). Clone stays a deep, independent copy, though — paste,
// undo, and save all rely on cloning a spec producing an independent record.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rom {
    pub data_width: u8,
    pub address_width: u8,
    pub data: Rc<RefCell<Vec<u32>>>,
}

// 2^24 words (64 MiB of u32) — the ceiling the GUI clamps address_width to.
pub const MAX_ADDRESS_WIDTH: u8 = 24;

impl Clone for Rom {
    // A fresh buffer, not a shared Rc handle (that's Rom::shared).
    fn clone(&self) -> Self {
        Self {
            data_width: self.data_width,
            address_width: self.address_width,
            data: Rc::new(RefCell::new(self.data.borrow().clone())),
        }
    }
}

impl Rom {
    pub fn new(data_width: u8, address_width: u8) -> Self {
        let len = 1usize << address_width;
        Self {
            data_width,
            address_width,
            data: Rc::new(RefCell::new(vec![0; len])),
        }
    }

    // Rc handle sharing the same buffer — the one exception to Clone's
    // deep-copy rule, so to_component() aliases the live component's data.
    pub fn shared(&self) -> Self {
        Self {
            data_width: self.data_width,
            address_width: self.address_width,
            data: Rc::clone(&self.data),
        }
    }

    pub fn mask(&self) -> u32 {
        mask_for(self.data_width)
    }

    pub fn len(&self) -> usize {
        self.data.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.borrow().is_empty()
    }

    pub fn word(&self, index: usize) -> u32 {
        self.data.borrow().get(index).copied().unwrap_or(0)
    }

    // &self (interior mutability): a write via either handle is visible to
    // both. No-op if out of range.
    pub fn set_word(&self, index: usize, value: u32) {
        let mut data = self.data.borrow_mut();
        if index < data.len() {
            data[index] = value & self.mask();
        }
    }

    // Shrinking truncates the high addresses; a narrower data_width masks
    // every retained word down. Returns a fresh, independent buffer.
    pub fn resized(&self, new_data_width: u8, new_address_width: u8) -> Self {
        let new_len = 1usize << new_address_width;
        let mut data = self.data.borrow().clone();
        data.resize(new_len, 0);
        if new_data_width < self.data_width {
            let m = mask_for(new_data_width);
            for w in &mut data {
                *w &= m;
            }
        }
        Self {
            data_width: new_data_width,
            address_width: new_address_width,
            data: Rc::new(RefCell::new(data)),
        }
    }
}

fn mask_for(width: u8) -> u32 {
    if width >= 32 {
        u32::MAX
    } else {
        (1u32 << width) - 1
    }
}

impl CombLogic for Rom {
    fn n_inputs(&self) -> usize {
        1
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        match inputs[0] {
            Value::Fixed { bits, width } if width == self.address_width => {
                let word =
                    self.data.borrow().get(bits as usize).copied().unwrap_or(0) & self.mask();
                vec![Value::new(word, self.data_width)]
            }
            // Bad address (mirrors how Mux treats a bad selector): Floating output.
            _ => vec![Value::Floating],
        }
    }
    fn input_width(&self, _i: usize) -> Option<u8> {
        Some(self.address_width)
    }
    fn output_width(&self, _i: usize) -> Option<u8> {
        Some(self.data_width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_zero_filled_to_length() {
        let rom = Rom::new(8, 4);
        assert_eq!(rom.len(), 16);
        assert!(rom.data.borrow().iter().all(|&w| w == 0));
    }

    #[test]
    fn test_reads_stored_word_masked_to_data_width() {
        let rom = Rom::new(4, 3);
        rom.set_word(5, 0xAB);
        assert_eq!(rom.evaluate(&[Value::new(5, 3)]), vec![Value::new(0xB, 4)]);
    }

    #[test]
    fn test_full_width_data_reads_all_32_bits() {
        let rom = Rom::new(32, 2);
        rom.set_word(3, 0xDEAD_BEEF);
        assert_eq!(
            rom.evaluate(&[Value::new(3, 2)]),
            vec![Value::new(0xDEAD_BEEF, 32)]
        );
    }

    #[test]
    fn test_address_width_mismatch_yields_floating() {
        let rom = Rom::new(4, 3);
        rom.set_word(1, 0xF);
        assert_eq!(rom.evaluate(&[Value::new(1, 2)]), vec![Value::Floating]);
    }

    #[test]
    fn test_floating_address_yields_floating() {
        let rom = Rom::new(4, 3);
        assert_eq!(rom.evaluate(&[Value::Floating]), vec![Value::Floating]);
    }

    #[test]
    fn test_shared_aliases_the_same_buffer() {
        let a = Rom::new(8, 2);
        let b = a.shared();
        a.set_word(1, 0x42);
        assert_eq!(b.word(1), 0x42);
    }

    #[test]
    fn test_clone_is_independent() {
        let a = Rom::new(8, 2);
        let b = a.clone();
        a.set_word(1, 0x42);
        assert_eq!(b.word(1), 0);
    }

    #[test]
    fn test_resized_grows_with_zeros_and_preserves_low_addresses() {
        let rom = Rom::new(8, 2);
        for (i, v) in [1, 2, 3, 4].into_iter().enumerate() {
            rom.set_word(i, v);
        }
        let grown = rom.resized(8, 3);
        assert_eq!(*grown.data.borrow(), vec![1, 2, 3, 4, 0, 0, 0, 0]);
    }

    #[test]
    fn test_resized_shrinks_by_truncation() {
        let rom = Rom::new(8, 3);
        for (i, v) in [1, 2, 3, 4, 5, 6, 7, 8].into_iter().enumerate() {
            rom.set_word(i, v);
        }
        let shrunk = rom.resized(8, 2);
        assert_eq!(*shrunk.data.borrow(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_resized_narrower_data_width_masks_words() {
        let rom = Rom::new(8, 2);
        for (i, v) in [0xFF, 0xAB, 0x12, 0x00].into_iter().enumerate() {
            rom.set_word(i, v);
        }
        let narrowed = rom.resized(4, 2);
        assert_eq!(*narrowed.data.borrow(), vec![0xF, 0xB, 0x2, 0x0]);
    }
}
