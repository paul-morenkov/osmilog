use egui_macroquad::macroquad;
use macroquad::prelude::*;
use slotmap::DefaultKey;

use crate::{
    components::{PinIndex, Signal, SignalRef},
    WireTarget,
};
use petgraph::stable_graph::NodeIndex;

#[derive(Debug, Clone)]
pub enum WireLink {
    Wire(NodeIndex),
    Pin(NodeIndex, PinIndex),
}

impl From<&WireTarget> for WireLink {
    fn from(value: &WireTarget) -> Self {
        match value {
            WireTarget::Pin(nx, px) => WireLink::Pin(*nx, *px),
            WireTarget::Wire(wx, _) => WireLink::Wire(wx.nx),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum WireEnd {
    Start,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireIndex {
    pub group: DefaultKey,
    pub nx: NodeIndex,
}

impl WireIndex {
    pub fn new(group: DefaultKey, nx: NodeIndex) -> Self {
        Self { group, nx }
    }
}

#[derive(Debug, Clone)]
pub struct WireSeg {
    pub start_pos: Vec2,
    pub end_pos: Vec2,
    start_link: Option<WireLink>,
    end_link: Option<WireLink>,
}

impl WireSeg {
    pub fn new(
        start_pos: Vec2,
        end_pos: Vec2,
        start_link: Option<WireLink>,
        end_link: Option<WireLink>,
    ) -> Self {
        Self {
            start_pos,
            end_pos,
            start_link,
            end_link,
        }
    }

    pub fn get_pos(&self, end: WireEnd) -> Vec2 {
        match end {
            WireEnd::Start => self.start_pos,
            WireEnd::End => self.end_pos,
        }
    }
}

#[derive(Debug)]
pub struct Wire {
    pub start_comp: NodeIndex,
    pub start_pin: usize,
    pub end_comp: NodeIndex,
    pub end_pin: usize,
    pub data_bits: u8,
    pub wire_group: DefaultKey,
    value: Option<Signal>,
    pub is_virtual: bool,
}

impl Wire {
    pub fn new(
        start_comp: NodeIndex,
        start_pin: usize,
        end_comp: NodeIndex,
        end_pin: usize,
        data_bits: u8,
        wire_group: DefaultKey,
        is_virtual: bool,
    ) -> Self {
        Self {
            start_comp,
            start_pin,
            end_comp,
            end_pin,
            data_bits,
            wire_group,
            value: None,
            is_virtual,
        }
    }

    pub fn set_signal(&mut self, value: Option<SignalRef>) {
        match value {
            None => self.value = None,
            Some(new) => match &mut self.value {
                Some(old) => old.copy_from_bitslice(new),
                None => self.value = Some(Signal::from_bitslice(new)),
            },
        }
    }

    pub fn get_signal(&self) -> Option<SignalRef> {
        self.value.as_deref()
    }
}
