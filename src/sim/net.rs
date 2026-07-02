use crate::sim::component::{CompKey, InIdx, OutIdx};
use crate::sim::value::Value;
use slotmap::new_key_type;

new_key_type! {
    pub struct NetKey;
}

#[derive(Debug, Default)]
pub struct Net {
    pub value: Value,
    pub source: Option<(CompKey, OutIdx)>,
    pub sinks: Vec<(CompKey, InIdx)>,
}
