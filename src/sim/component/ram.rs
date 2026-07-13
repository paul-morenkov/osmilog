use std::cell::RefCell;
use std::rc::Rc;

use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

// Read/write memory: structurally similar to Rom (bulk contents in a shared
// Rc<RefCell<Vec<u32>>>, mask/word/shared/resized all mirror Rom's), but
// sequential rather than combinational - "DO" is a *registered* read,
// updated only on tick() (like Reg's latched value), not recomputed on every
// settle() the way Rom::evaluate() is. `Ram` itself plays the same role
// Rom does: it IS the ComponentSpec payload (ComponentSpec::Ram(Ram)), and
// to_component() shares its buffer (Ram::shared) with the live component -
// see RamCell below for the one bit of genuinely-live-only state (the output
// latch) that can't live here.
//
// Unlike Rom, contents are deliberately NOT part of the persisted/copied
// record: a fresh Ram always starts zero-filled, and Clone produces a FRESH
// zero buffer rather than Rom's deep copy of existing contents - the
// codebase treats a RAM's live contents as ephemeral debug state (cleared at
// the start of any program run), not program data like a ROM's. That's also
// why Serialize/Deserialize are hand-written below instead of derived: only
// the three config fields round-trip through a save file, never `data`. The
// one place contents survive a `ComponentSpec` op is `Ram::resized`,
// mirroring Rom's preserve-and-fit width-change behavior in the properties
// panel - a deliberate, explicit copy of the *current* buffer, not the
// implicit one Clone would do.
#[derive(Debug, PartialEq)]
pub struct Ram {
    pub data_width: u8,
    pub address_width: u8,
    pub read_behavior: ReadBehavior,
    pub data: Rc<RefCell<Vec<u32>>>,
}

// Resolves what data_out reports when write_enable and load_enable are both
// asserted for the same address on the same tick - classic block-RAM
// READ_FIRST ("write-after-read": the output reflects the pre-write value)
// vs WRITE_FIRST ("read-after-write": the output reflects the just-written
// value) semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ReadBehavior {
    #[default]
    ReadAfterWrite,
    WriteAfterRead,
}

impl Clone for Ram {
    // A fresh, independent, ZERO-FILLED buffer - not a shared Rc handle
    // (that's Ram::shared), and NOT a copy of self's current contents
    // (that's Ram::resized). This is what makes "cloning a spec yields an
    // independent spec" (paste, undo, save) also satisfy "RAM contents don't
    // persist across a copy" - both fall out of the same Clone impl.
    fn clone(&self) -> Self {
        Self::new(self.data_width, self.address_width, self.read_behavior)
    }
}

impl Ram {
    pub const ADDR_PIN: usize = 0;
    pub const WE_PIN: usize = 1;
    pub const LE_PIN: usize = 2;
    pub const DATA_IN_PIN: usize = 3;
    pub const DATA_OUT_PIN: usize = 0;

    // A fresh RAM of the given widths, zero-filled to 2^address_width words.
    pub fn new(data_width: u8, address_width: u8, read_behavior: ReadBehavior) -> Self {
        let len = 1usize << address_width;
        Self {
            data_width,
            address_width,
            read_behavior,
            data: Rc::new(RefCell::new(vec![0; len])),
        }
    }

    // A handle sharing the SAME backing buffer (Rc bump). The single,
    // deliberate exception to Clone's zero-fill rule: to_component() uses
    // this so the placed spec and the live circuit component are one copy,
    // not two - mirrors Rom::shared exactly.
    pub fn shared(&self) -> Self {
        Self {
            data_width: self.data_width,
            address_width: self.address_width,
            read_behavior: self.read_behavior,
            data: Rc::clone(&self.data),
        }
    }

    pub fn n_inputs(&self) -> usize {
        4
    }

    pub fn n_outputs(&self) -> usize {
        1
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::ADDR_PIN => Some(self.address_width),
            Self::WE_PIN => Some(1),
            Self::LE_PIN => Some(1),
            Self::DATA_IN_PIN => Some(self.data_width),
            _ => None,
        }
    }

    pub fn output_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::DATA_OUT_PIN => Some(self.data_width),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        self.data.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.borrow().is_empty()
    }

    // The stored word at `index` (0 if out of range), already masked.
    pub fn word(&self, index: usize) -> u32 {
        self.data.borrow().get(index).copied().unwrap_or(0)
    }

    // Writes one word in place (masked to data_width), through the shared
    // handle - takes &self (interior mutability) so a write via either the
    // placed spec's handle or the live component's is visible to both.
    // No-op if out of range.
    pub fn set_word(&self, index: usize, value: u32) {
        let mut data = self.data.borrow_mut();
        if index < data.len() {
            data[index] = value & Value::mask(self.data_width);
        }
    }

    // A deep copy resized to new widths, preserving as much data as
    // possible - identical preserve-and-fit rules to Rom::resized. Unlike
    // Clone, this deliberately reads self's CURRENT contents: it backs the
    // properties panel's width-change edit, where wiping an in-progress
    // debug session on a mere width tweak would be surprising.
    pub fn resized(&self, new_data_width: u8, new_address_width: u8) -> Self {
        let new_len = 1usize << new_address_width;
        let mut data = self.data.borrow().clone();
        data.resize(new_len, 0);
        if new_data_width < self.data_width {
            let m = Value::mask(new_data_width);
            for w in &mut data {
                *w &= m;
            }
        }
        Self {
            data_width: new_data_width,
            address_width: new_address_width,
            read_behavior: self.read_behavior,
            data: Rc::new(RefCell::new(data)),
        }
    }
}

// Hand-written (not derived): only the three config fields are persisted.
// `data` never appears on disk - a loaded RAM always starts zero-filled via
// Deserialize's Ram::new call, exactly like a freshly-placed one.
impl serde::Serialize for Ram {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Ram", 3)?;
        s.serialize_field("data_width", &self.data_width)?;
        s.serialize_field("address_width", &self.address_width)?;
        s.serialize_field("read_behavior", &self.read_behavior)?;
        s.end()
    }
}

impl<'de> serde::Deserialize<'de> for Ram {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct RamFields {
            data_width: u8,
            address_width: u8,
            read_behavior: ReadBehavior,
        }
        let f = RamFields::deserialize(deserializer)?;
        Ok(Ram::new(f.data_width, f.address_width, f.read_behavior))
    }
}

// The live sequential component. `conf` carries the shared buffer (aliased
// with the placed spec via Ram::shared, see to_component()); `output` is the
// one piece of state that can't live in `Ram`/ComponentSpec - the registered
// "DO" latch, exactly analogous to Reg's `value` field sitting outside
// RegConf.
#[derive(Debug)]
pub struct RamCell {
    conf: Ram,
    output: Value,
}

impl RamCell {
    pub fn new(conf: Ram) -> Self {
        let output = Value::new(0, conf.data_width);
        Self { conf, output }
    }

    // Bits of `addr` if it's a Fixed value of exactly `address_width` bits,
    // else None - an unexpected width (or Floating/Invalid) means neither a
    // read nor a write can target a real word this tick, mirroring how Rom
    // treats a bad address as unreadable.
    fn index_of(addr: Value, address_width: u8) -> Option<usize> {
        match addr {
            Value::Fixed { bits, width } if width == address_width => Some(bits as usize),
            _ => None,
        }
    }

    // Writes data_in at `idx` if it's a well-formed Fixed value of exactly
    // data_width bits; otherwise leaves memory untouched rather than storing
    // garbage.
    fn write_if_valid(&self, idx: usize, data_in: Value) {
        if let Value::Fixed { bits, width } = data_in {
            if width == self.conf.data_width {
                self.conf.set_word(idx, bits);
            }
        }
    }

    fn read(&self, idx: usize) -> Value {
        Value::new(self.conf.word(idx), self.conf.data_width)
    }

    // Read-only access to the shared contents buffer, for Circuit::write_ram
    // (the GUI's live RAM contents editor) - a debug-time direct memory
    // write, deliberately bypassing tick()'s write_enable/read_behavior
    // logic entirely, mirroring how write_rom reaches straight into a Rom's
    // buffer.
    pub fn contents(&self) -> &Ram {
        &self.conf
    }
}

impl SeqLogic for RamCell {
    fn n_inputs(&self) -> usize {
        self.conf.n_inputs()
    }

    fn n_outputs(&self) -> usize {
        self.conf.n_outputs()
    }

    fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        let addr = inputs[Ram::ADDR_PIN];
        // Unwired write_enable/load_enable default to active, mirroring
        // Reg's write_enable convention: a bare RAM with only A/DI wired
        // still reads and writes every tick.
        let write_enable = matches!(inputs[Ram::WE_PIN], Value::ONE | Value::Floating);
        let load_enable = matches!(inputs[Ram::LE_PIN], Value::ONE | Value::Floating);
        let data_in = inputs[Ram::DATA_IN_PIN];

        let Some(idx) = Self::index_of(addr, self.conf.address_width) else {
            // No valid address: nothing to write, and a requested read
            // yields Floating (mirrors Rom on a bad address) rather than
            // silently holding a stale value.
            if load_enable {
                self.output = Value::Floating;
            }
            return vec![self.output];
        };

        // read_behavior only matters when both are asserted for the same
        // address this tick: it picks whether the read sees the pre- or
        // post-write word.
        match self.conf.read_behavior {
            ReadBehavior::WriteAfterRead => {
                if load_enable {
                    self.output = self.read(idx);
                }
                if write_enable {
                    self.write_if_valid(idx, data_in);
                }
            }
            ReadBehavior::ReadAfterWrite => {
                if write_enable {
                    self.write_if_valid(idx, data_in);
                }
                if load_enable {
                    self.output = self.read(idx);
                }
            }
        }

        vec![self.output]
    }

    // RAM has no asynchronous inputs (no async reset pin, unlike Reg/the
    // flip-flops) - its state only changes on a clock tick.
    fn apply_async(&mut self, _inputs: &[Value]) {}

    fn observe(&self) -> Vec<Value> {
        vec![self.output]
    }

    // Deliberately does NOT touch self.conf.data (the stored words) - only
    // the registered output resets to power-on. Every other sequential
    // component's Stop wipes its latched state; RAM is the one exception,
    // so a RAM being hand-edited/populated mid-build survives a
    // play/pause/step/stop cycle instead of being zeroed on every Stop.
    fn reset(&mut self) {
        self.output = Value::new(0, self.conf.data_width);
    }

    fn snapshot(&self) -> SeqState {
        SeqState::Ram(self.output)
    }

    fn input_width(&self, i: usize) -> Option<u8> {
        self.conf.input_width(i)
    }

    fn output_width(&self, i: usize) -> Option<u8> {
        self.conf.output_width(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::component::LogicSeq;
    use test_case::test_case;

    fn new_ram(data_width: u8, address_width: u8, rb: ReadBehavior) -> LogicSeq {
        LogicSeq::Ram(RamCell::new(Ram::new(data_width, address_width, rb)))
    }

    // Full input vector in pin order [A, WE, LE, DI].
    fn ins(addr: Value, we: Value, le: Value, di: Value) -> [Value; 4] {
        [addr, we, le, di]
    }

    const WE: Value = Value::ONE;
    const LE: Value = Value::ONE;
    const NO_WE: Value = Value::ZERO;
    const NO_LE: Value = Value::ZERO;

    #[test]
    fn test_initial_output_is_zero() {
        let ram = new_ram(4, 3, ReadBehavior::ReadAfterWrite);
        assert_eq!(ram.observe(), vec![Value::new(0, 4)]);
    }

    #[test]
    fn test_write_then_read_next_tick() {
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        // Write 0x42 at address 5.
        ram.tick(&ins(Value::new(5, 4), WE, NO_LE, Value::new(0x42, 8)));
        // Load address 5 on the next tick.
        assert_eq!(
            ram.tick(&ins(Value::new(5, 4), NO_WE, LE, Value::Floating)),
            vec![Value::new(0x42, 8)]
        );
    }

    #[test]
    fn test_load_disabled_holds_previous_output() {
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        ram.tick(&ins(Value::new(1, 4), WE, LE, Value::new(0x9, 8)));
        assert_eq!(ram.observe(), vec![Value::new(0x9, 8)]);
        // LE deasserted: reading a different address does not update output.
        assert_eq!(
            ram.tick(&ins(Value::new(2, 4), NO_WE, NO_LE, Value::Floating)),
            vec![Value::new(0x9, 8)]
        );
    }

    #[test]
    fn test_write_disabled_does_not_modify_memory() {
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        ram.tick(&ins(Value::new(3, 4), NO_WE, NO_LE, Value::new(0xFF, 8)));
        assert_eq!(
            ram.tick(&ins(Value::new(3, 4), NO_WE, LE, Value::Floating)),
            vec![Value::new(0, 8)]
        );
    }

    #[test]
    fn test_read_after_write_sees_new_value_same_tick() {
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        assert_eq!(
            ram.tick(&ins(Value::new(0, 4), WE, LE, Value::new(0x7, 8))),
            vec![Value::new(0x7, 8)]
        );
    }

    #[test]
    fn test_write_after_read_sees_old_value_same_tick() {
        let mut ram = new_ram(8, 4, ReadBehavior::WriteAfterRead);
        // Pre-seed address 0 with 0x1.
        ram.tick(&ins(Value::new(0, 4), WE, NO_LE, Value::new(0x1, 8)));
        // Same tick write 0x7 and load: WriteAfterRead reports the pre-write value.
        assert_eq!(
            ram.tick(&ins(Value::new(0, 4), WE, LE, Value::new(0x7, 8))),
            vec![Value::new(0x1, 8)]
        );
        // The write still took effect for the *next* read.
        assert_eq!(
            ram.tick(&ins(Value::new(0, 4), NO_WE, LE, Value::Floating)),
            vec![Value::new(0x7, 8)]
        );
    }

    #[test]
    fn test_unwired_we_and_le_default_to_active() {
        // Mirrors Reg's write_enable convention: Floating WE/LE behave as enabled.
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        assert_eq!(
            ram.tick(&ins(
                Value::new(2, 4),
                Value::Floating,
                Value::Floating,
                Value::new(0x5, 8)
            )),
            vec![Value::new(0x5, 8)]
        );
    }

    #[test_case(Value::new(1, 2) ; "wrong width")]
    #[test_case(Value::ZERO ; "exactly zero")]
    fn test_we_non_writing_cases(we: Value) {
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        ram.tick(&ins(Value::new(0, 4), we, NO_LE, Value::new(0xAB, 8)));
        assert_eq!(
            ram.tick(&ins(Value::new(0, 4), NO_WE, LE, Value::Floating)),
            vec![Value::new(0, 8)]
        );
    }

    #[test]
    fn test_floating_address_yields_floating_on_load_and_skips_write() {
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        assert_eq!(
            ram.tick(&ins(Value::Floating, WE, LE, Value::new(0x9, 8))),
            vec![Value::Floating]
        );
        // No address means nothing was written anywhere - probe address 0.
        assert_eq!(
            ram.tick(&ins(Value::new(0, 4), NO_WE, LE, Value::Floating)),
            vec![Value::new(0, 8)]
        );
    }

    #[test]
    fn test_address_width_mismatch_yields_floating_on_load() {
        let mut ram = new_ram(4, 3, ReadBehavior::ReadAfterWrite);
        assert_eq!(
            ram.tick(&ins(Value::new(1, 2), NO_WE, LE, Value::Floating)),
            vec![Value::Floating]
        );
    }

    #[test]
    fn test_malformed_data_in_does_not_write() {
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        // Wrong-width data_in: write is skipped, memory stays 0.
        ram.tick(&ins(Value::new(0, 4), WE, NO_LE, Value::new(0xFF, 4)));
        assert_eq!(
            ram.tick(&ins(Value::new(0, 4), NO_WE, LE, Value::Floating)),
            vec![Value::new(0, 8)]
        );
    }

    #[test]
    fn test_reset_clears_output_but_preserves_memory() {
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        ram.tick(&ins(Value::new(0, 4), WE, LE, Value::new(0x42, 8)));
        assert_eq!(ram.observe(), vec![Value::new(0x42, 8)]);

        ram.reset();
        assert_eq!(ram.observe(), vec![Value::new(0, 8)]);

        // Memory itself must survive: reading address 0 again shows 0x42.
        assert_eq!(
            ram.tick(&ins(Value::new(0, 4), NO_WE, LE, Value::Floating)),
            vec![Value::new(0x42, 8)]
        );
    }

    #[test]
    fn test_apply_async_is_a_noop() {
        let mut ram = new_ram(8, 4, ReadBehavior::ReadAfterWrite);
        ram.tick(&ins(Value::new(0, 4), WE, LE, Value::new(0x3, 8)));
        ram.apply_async(&ins(Value::new(0, 4), NO_WE, LE, Value::new(0x9, 8)));
        assert_eq!(ram.observe(), vec![Value::new(0x3, 8)]);
    }

    #[test]
    fn test_clone_zero_fills_and_does_not_alias() {
        let ram = Ram::new(8, 4, ReadBehavior::ReadAfterWrite);
        ram.set_word(1, 0x42);
        let cloned = ram.clone();
        assert_eq!(cloned.word(1), 0);
        // Independent: writing to the original doesn't affect the clone.
        ram.set_word(2, 0x11);
        assert_eq!(cloned.word(2), 0);
    }

    #[test]
    fn test_shared_aliases_the_same_buffer() {
        let a = Ram::new(8, 2, ReadBehavior::ReadAfterWrite);
        let b = a.shared();
        a.set_word(1, 0x42);
        assert_eq!(b.word(1), 0x42);
    }

    #[test]
    fn test_resized_preserves_current_contents() {
        let ram = Ram::new(8, 2, ReadBehavior::ReadAfterWrite); // 4 words
        for (i, v) in [1, 2, 3, 4].into_iter().enumerate() {
            ram.set_word(i, v);
        }
        let grown = ram.resized(8, 3); // 8 words
        assert_eq!(*grown.data.borrow(), vec![1, 2, 3, 4, 0, 0, 0, 0]);
    }

    #[test]
    fn test_serde_round_trip_never_persists_contents() {
        let ram = Ram::new(8, 4, ReadBehavior::WriteAfterRead);
        ram.set_word(3, 0xAB);
        let json = serde_json::to_string(&ram).unwrap();
        assert!(!json.contains("171")); // 0xAB decimal - contents never serialized
        let reloaded: Ram = serde_json::from_str(&json).unwrap();
        assert_eq!(reloaded.data_width, 8);
        assert_eq!(reloaded.address_width, 4);
        assert_eq!(reloaded.read_behavior, ReadBehavior::WriteAfterRead);
        assert_eq!(reloaded.word(3), 0); // zero-filled, not the original 0xAB
    }
}
