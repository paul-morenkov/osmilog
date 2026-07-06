use crate::sim::component::{CompKey, InIdx, OutIdx};
use crate::sim::value::Value;
use slotmap::new_key_type;

new_key_type! {
    pub struct NetKey;
}

#[derive(Debug, Default)]
pub struct Net {
    pub value: Value,
    // Every output pin currently driving this net. A well-formed net has at
    // most one; two or more is a driver conflict (a short) that resolve_net
    // flags as Value::Invalid, mirroring the width-conflict handling.
    pub sources: Vec<(CompKey, OutIdx)>,
    pub sinks: Vec<(CompKey, InIdx)>,
}
