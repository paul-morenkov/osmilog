use std::cell::RefCell;
use std::rc::Rc;

use super::CombLogic;
use crate::sim::value::Value;

// Read-only memory: a combinational lookup table. The single input "A"
// (width = address_width) indexes into `data`, and the single output "D"
// (width = data_width) reports the stored word. Unlike every other combinational
// component, a ROM carries bulk state (`data`) - but it stays combinational
// because evaluate() is a pure read: the contents only change through an explicit
// GUI edit (Circuit::write_rom), never through the signal logic.
//
// The contents are shared, not duplicated. The GUI's placed spec
// (ComponentSpec::Rom) and the live circuit component both hold a Rom, and both
// must see the same bytes without keeping two copies of what can be 64 MiB - so
// `data` is an Rc<RefCell<..>> and `to_component()` shares the handle (Rom::shared,
// an Rc bump) rather than copying. Interior mutability lets Circuit::write_rom
// edit contents in place through a shared handle.
//
// The one wrinkle: `Clone` must stay a *deep, independent* copy, because the
// whole codebase treats cloning a ComponentSpec as producing an independent
// record (paste, undo snapshots, save). A derived Clone would only bump the Rc,
// so a pasted ROM would alias the original's contents - hence the hand-written
// deep Clone below, with Rom::shared() as the single explicit opt-in to sharing.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rom {
    pub data_width: u8,
    pub address_width: u8,
    // Shared, interior-mutable contents. On disk this is transparent - it
    // serializes as a plain array of words (RefCell and Rc add no wrapping),
    // so the save format is unchanged. Requires serde's "rc" feature.
    pub data: Rc<RefCell<Vec<u32>>>,
}

// Largest address_width we allow: 2^24 words (64 MiB of u32) is the ceiling the
// GUI clamps address_width to.
pub const MAX_ADDRESS_WIDTH: u8 = 24;

impl Clone for Rom {
    // Deep, independent copy - a fresh buffer, NOT a shared Rc handle (that's
    // Rom::shared). This keeps "cloning a spec yields an independent spec" true
    // everywhere the codebase relies on it (paste, undo, save).
    fn clone(&self) -> Self {
        Self {
            data_width: self.data_width,
            address_width: self.address_width,
            data: Rc::new(RefCell::new(self.data.borrow().clone())),
        }
    }
}

impl Rom {
    // A fresh ROM of the given widths, zero-filled to 2^address_width words.
    pub fn new(data_width: u8, address_width: u8) -> Self {
        let len = 1usize << address_width;
        Self {
            data_width,
            address_width,
            data: Rc::new(RefCell::new(vec![0; len])),
        }
    }

    // A handle sharing the SAME backing buffer (Rc bump). The single, deliberate
    // exception to Clone's deep-copy rule: to_component() uses this so the placed
    // spec and the live circuit component are one copy, not two.
    pub fn shared(&self) -> Self {
        Self {
            data_width: self.data_width,
            address_width: self.address_width,
            data: Rc::clone(&self.data),
        }
    }

    // Low `data_width` bits set - the mask every stored/emitted word is held to.
    pub fn mask(&self) -> u32 {
        mask_for(self.data_width)
    }

    pub fn len(&self) -> usize {
        self.data.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.borrow().is_empty()
    }

    // The stored word at `index` (0 if out of range), as stored (already masked).
    pub fn word(&self, index: usize) -> u32 {
        self.data.borrow().get(index).copied().unwrap_or(0)
    }

    // Writes one word in place (masked to data_width), through the shared handle.
    // Takes &self: interior mutability means no &mut Rom is needed, so a write
    // via either the placed spec's handle or the live component's is visible to
    // both. No-op if out of range.
    pub fn set_word(&self, index: usize, value: u32) {
        let mut data = self.data.borrow_mut();
        if index < data.len() {
            data[index] = value & self.mask();
        }
    }

    // A deep copy resized to new widths, preserving as much data as possible:
    // growing address_width zero-extends, shrinking truncates (drops the high
    // addresses), and a narrower data_width masks every retained word down. The
    // result owns a fresh buffer (independent of self).
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
            // A well-formed address of the expected width reads the stored word,
            // masked to data_width. An address's bits are always < 2^width, and
            // data.len() == 2^address_width, so the index is always in range - the
            // get() guard is just belt-and-braces.
            Value::Fixed { bits, width } if width == self.address_width => {
                let word = self.data.borrow().get(bits as usize).copied().unwrap_or(0) & self.mask();
                vec![Value::new(word, self.data_width)]
            }
            // Floating/Invalid address, or a width mismatch (mirrors how Mux
            // treats a bad selector): nothing to read → Floating output.
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
        let rom = Rom::new(4, 3); // data_width 4, 8 words
        rom.set_word(5, 0xAB); // set_word already masks to 4 bits
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
        // Address is width 2, but the ROM expects address_width 3.
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
        assert_eq!(b.word(1), 0x42); // b sees a's write - one buffer
    }

    #[test]
    fn test_clone_is_independent() {
        let a = Rom::new(8, 2);
        let b = a.clone();
        a.set_word(1, 0x42);
        assert_eq!(b.word(1), 0); // b is a deep copy - unaffected
    }

    #[test]
    fn test_resized_grows_with_zeros_and_preserves_low_addresses() {
        let rom = Rom::new(8, 2); // 4 words
        for (i, v) in [1, 2, 3, 4].into_iter().enumerate() {
            rom.set_word(i, v);
        }
        let grown = rom.resized(8, 3); // 8 words
        assert_eq!(*grown.data.borrow(), vec![1, 2, 3, 4, 0, 0, 0, 0]);
    }

    #[test]
    fn test_resized_shrinks_by_truncation() {
        let rom = Rom::new(8, 3);
        for (i, v) in [1, 2, 3, 4, 5, 6, 7, 8].into_iter().enumerate() {
            rom.set_word(i, v);
        }
        let shrunk = rom.resized(8, 2); // 4 words
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
