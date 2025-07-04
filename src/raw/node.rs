use std::cmp;
use std::fmt;
use std::io;
use std::ops::Range;

use crate::bytes;
use crate::raw::build::BuilderNode;
use crate::raw::common_inputs::{COMMON_INPUTS, COMMON_INPUTS_INV};
use crate::raw::{
    u64_to_usize, CompiledAddr, Output, Transition, EMPTY_ADDRESS,
};

/// The threshold (in number of transitions) at which an index is created for
/// a node's transitions. This speeds up lookup time at the expense of FST
/// size.
const TRANS_INDEX_THRESHOLD: usize = 32;

/// Node represents a single state in a finite state transducer.
///
/// Nodes are very cheap to construct. Notably, they satisfy the `Copy` trait.
#[derive(Clone, Copy)]
pub struct Node<'f> {
    data: &'f [u8],
    version: u64,
    state: State,
    start: CompiledAddr,
    end: usize,
    is_final: bool,
    ntrans: usize,
    sizes: PackSizes,
    final_output: Output,
}

impl<'f> fmt::Debug for Node<'f> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "NODE@")?;
        writeln!(f, "  start_add: {}", self.start)?;
        writeln!(f, "  end_addr: {}", self.end)?;
        writeln!(f, "  size: {} bytes", self.as_slice().len())?;
        writeln!(f, "  state: {:?}", self.state)?;
        writeln!(f, "  is_final: {}", self.is_final())?;
        writeln!(f, "  final_output: {:?}", self.final_output())?;
        writeln!(f, "  # transitions: {}", self.len())?;
        writeln!(f, "  transitions:")?;
        for t in self.transitions() {
            writeln!(f, "    {:?}", t)?;
        }
        Ok(())
    }
}

impl<'f> Node<'f> {
    /// Creates a new note at the address given.
    ///
    /// `data` should be a slice to an entire FST.
    pub(crate) fn new(
        version: u64,
        addr: CompiledAddr,
        data: &[u8],
    ) -> Node<'_> {
        let state = State::new(data, addr);
        match state {
            State::EmptyFinal => Node {
                data: &[],
                version,
                state: State::EmptyFinal,
                start: EMPTY_ADDRESS,
                end: EMPTY_ADDRESS,
                is_final: true,
                ntrans: 0,
                sizes: PackSizes::new(),
                final_output: Output::zero(),
            },
            State::OneTransNext(s) => {
                let data = &data[..addr + 1];
                Node {
                    data,
                    version,
                    state,
                    start: addr,
                    end: s.end_addr(data),
                    is_final: false,
                    sizes: PackSizes::new(),
                    ntrans: 1,
                    final_output: Output::zero(),
                }
            }
            State::OneTrans(s) => {
                let data = &data[..addr + 1];
                let sizes = s.sizes(data);
                Node {
                    data,
                    version,
                    state,
                    start: addr,
                    end: s.end_addr(data, sizes),
                    is_final: false,
                    ntrans: 1,
                    sizes,
                    final_output: Output::zero(),
                }
            }
            State::AnyTrans(s) => {
                let data = &data[..addr + 1];
                let sizes = s.sizes(data);
                let ntrans = s.ntrans(data);
                Node {
                    data,
                    version,
                    state,
                    start: addr,
                    end: s.end_addr(version, data, sizes, ntrans),
                    is_final: s.is_final_state(),
                    ntrans,
                    sizes,
                    final_output: s.final_output(version, data, sizes, ntrans),
                }
            }
        }
    }
    /// Returns an iterator over all transitions in this node in lexicographic
    /// order.
    #[inline]
    pub fn transitions<'n>(&'n self) -> Transitions<'f, 'n> {
        Transitions { node: self, range: 0..self.len() }
    }

    /// Returns the transition at index `i`.
    #[inline(always)]
    pub fn transition(&self, i: usize) -> Transition {
        // The `inline(always)` annotation on this function appears to
        // dramatically speed up FST traversal. In particular, measuring the
        // time it takes to run `fst range something-big.fst` shows almost a 2x
        // improvement with this single `inline(always)` annotation.

        match self.state {
            State::OneTransNext(s) => {
                assert!(i == 0);
                Transition {
                    inp: s.input(self),
                    out: Output::zero(),
                    addr: s.trans_addr(self),
                }
            }
            State::OneTrans(s) => {
                assert!(i == 0);
                Transition {
                    inp: s.input(self),
                    out: s.output(self),
                    addr: s.trans_addr(self),
                }
            }
            State::AnyTrans(s) => Transition {
                inp: s.input(self, i),
                out: s.output(self, i),
                addr: s.trans_addr(self, i),
            },
            State::EmptyFinal => panic!("out of bounds"),
        }
    }

    /// Returns the transition address of the `i`th transition.
    #[inline]
    pub fn transition_addr(&self, i: usize) -> CompiledAddr {
        match self.state {
            State::OneTransNext(s) => {
                assert!(i == 0);
                s.trans_addr(self)
            }
            State::OneTrans(s) => {
                assert!(i == 0);
                s.trans_addr(self)
            }
            State::AnyTrans(s) => s.trans_addr(self, i),
            State::EmptyFinal => panic!("out of bounds"),
        }
    }

    /// Finds the `i`th transition corresponding to the given input byte.
    ///
    /// If no transition for this byte exists, then `None` is returned.
    #[inline]
    pub fn find_input(&self, b: u8) -> Option<usize> {
        match self.state {
            State::OneTransNext(s) if s.input(self) == b => Some(0),
            State::OneTransNext(_) => None,
            State::OneTrans(s) if s.input(self) == b => Some(0),
            State::OneTrans(_) => None,
            State::AnyTrans(s) => s.find_input(self, b),
            State::EmptyFinal => None,
        }
    }

    /// If this node is final and has a terminal output value, then it is
    /// returned. Otherwise, a zero output is returned.
    #[inline]
    pub fn final_output(&self) -> Output {
        self.final_output
    }

    /// Returns true if and only if this node corresponds to a final or "match"
    /// state in the finite state transducer.
    #[inline]
    pub fn is_final(&self) -> bool {
        self.is_final
    }

    /// Returns the number of transitions in this node.
    ///
    /// The maximum number of transitions is 256.
    #[inline]
    pub fn len(&self) -> usize {
        self.ntrans
    }

    /// Returns true if and only if this node has zero transitions.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ntrans == 0
    }

    /// Return the address of this node.
    #[inline]
    pub fn addr(&self) -> CompiledAddr {
        self.start
    }

    #[doc(hidden)]
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data[self.end..]
    }

    #[doc(hidden)]
    #[inline]
    pub fn state(&self) -> &'static str {
        match self.state {
            State::OneTransNext(_) => "OTN",
            State::OneTrans(_) => "OT",
            State::AnyTrans(_) => "AT",
            State::EmptyFinal => "EF",
        }
    }

    fn compile<W: io::Write>(
        wtr: W,
        last_addr: CompiledAddr,
        addr: CompiledAddr,
        node: &BuilderNode,
    ) -> io::Result<()> {
        assert!(node.trans.len() <= 256);
        if node.trans.is_empty()
            && node.is_final
            && node.final_output.is_zero()
        {
            return Ok(());
        } else if node.trans.len() != 1 || node.is_final {
            StateAnyTrans::compile(wtr, addr, node)
        } else {
            if node.trans[0].addr == last_addr && node.trans[0].out.is_zero() {
                StateOneTransNext::compile(wtr, addr, node.trans[0].inp)
            } else {
                StateOneTrans::compile(wtr, addr, node.trans[0])
            }
        }
    }
}

impl BuilderNode {
    pub fn compile_to<W: io::Write>(
        &self,
        wtr: W,
        last_addr: CompiledAddr,
        addr: CompiledAddr,
    ) -> io::Result<()> {
        Node::compile(wtr, last_addr, addr, self)
    }
}

#[derive(Clone, Copy, Debug)]
enum State {
    OneTransNext(StateOneTransNext),
    OneTrans(StateOneTrans),
    AnyTrans(StateAnyTrans),
    EmptyFinal,
}

// one trans flag (1), next flag (1), common input
#[derive(Clone, Copy, Debug)]
struct StateOneTransNext(u8);
// one trans flag (1), next flag (0), common input
#[derive(Clone, Copy, Debug)]
struct StateOneTrans(u8);
// one trans flag (0), final flag, # transitions
#[derive(Clone, Copy, Debug)]
struct StateAnyTrans(u8);

impl State {
    fn new(data: &[u8], addr: CompiledAddr) -> State {
        if addr == EMPTY_ADDRESS {
            return State::EmptyFinal;
        }
        let v = data[addr];
        match (v & 0b11_000000) >> 6 {
            0b11 => State::OneTransNext(StateOneTransNext(v)),
            0b10 => State::OneTrans(StateOneTrans(v)),
            _ => State::AnyTrans(StateAnyTrans(v)),
        }
    }
}

impl StateOneTransNext {
    fn compile<W: io::Write>(
        mut wtr: W,
        _: CompiledAddr,
        input: u8,
    ) -> io::Result<()> {
        let mut state = StateOneTransNext::new();
        state.set_common_input(input);
        if state.common_input().is_none() {
            wtr.write_all(&[input])?;
        }
        wtr.write_all(&[state.0])?;
        Ok(())
    }

    #[inline]
    fn new() -> StateOneTransNext {
        StateOneTransNext(0b11_000000)
    }

    #[inline]
    fn set_common_input(&mut self, input: u8) {
        self.0 = (self.0 & 0b11_000000) | common_idx(input, 0b111111);
    }

    #[inline]
    fn common_input(&self) -> Option<u8> {
        common_input(self.0 & 0b00_111111)
    }

    #[inline]
    fn input_len(&self) -> usize {
        if self.common_input().is_none() {
            1
        } else {
            0
        }
    }

    #[inline]
    fn end_addr(&self, data: &[u8]) -> usize {
        data.len() - 1 - self.input_len()
    }

    #[inline]
    fn input(&self, node: &Node<'_>) -> u8 {
        if let Some(inp) = self.common_input() {
            inp
        } else {
            node.data[node.start - 1]
        }
    }

    #[inline]
    fn trans_addr(&self, node: &Node<'_>) -> CompiledAddr {
        node.end as CompiledAddr - 1
    }
}

impl StateOneTrans {
    fn compile<W: io::Write>(
        mut wtr: W,
        addr: CompiledAddr,
        trans: Transition,
    ) -> io::Result<()> {
        let out = trans.out.value();
        let output_pack_size =
            if out == 0 { 0 } else { bytes::pack_uint(&mut wtr, out)? };
        let trans_pack_size = pack_delta(&mut wtr, addr, trans.addr)?;

        let mut pack_sizes = PackSizes::new();
        pack_sizes.set_output_pack_size(output_pack_size);
        pack_sizes.set_transition_pack_size(trans_pack_size);
        let x = [pack_sizes.encode()];
        println!("pack_sizes: {x:?}");
        wtr.write_all(&x)?;

        let mut state = StateOneTrans::new();
        state.set_common_input(trans.inp);
        if state.common_input().is_none() {
            wtr.write_all(&[trans.inp])?;
        }
        wtr.write_all(&[state.0])?;
        Ok(())
    }

    #[inline]
    fn new() -> StateOneTrans {
        StateOneTrans(0b10_000000)
    }

    #[inline]
    fn set_common_input(&mut self, input: u8) {
        // self.0 = (self.0 & 0b10_000000) | common_idx(input, 0b111111);
        let x = (self.0 & 0b10_000000) | common_idx(input, 0b111111);
        println!("common_input: {input} {x}");
        self.0 = x;
    }

    #[inline]
    fn common_input(&self) -> Option<u8> {
        common_input(self.0 & 0b00_111111)
    }

    #[inline]
    fn input_len(&self) -> usize {
        if self.common_input().is_none() {
            1
        } else {
            0
        }
    }

    #[inline]
    fn sizes(&self, data: &[u8]) -> PackSizes {
        let i = data.len() - 1 - self.input_len() - 1;
        PackSizes::decode(data[i])
    }

    #[inline]
    fn end_addr(&self, data: &[u8], sizes: PackSizes) -> usize {
        data.len() - 1
        - self.input_len()
        - 1 // pack size
        - sizes.transition_pack_size()
        - sizes.output_pack_size()
    }

    #[inline]
    fn input(&self, node: &Node<'_>) -> u8 {
        if let Some(inp) = self.common_input() {
            inp
        } else {
            node.data[node.start - 1]
        }
    }

    #[inline]
    fn output(&self, node: &Node<'_>) -> Output {
        let osize = node.sizes.output_pack_size();
        if osize == 0 {
            return Output::zero();
        }
        let tsize = node.sizes.transition_pack_size();
        let i = node.start
                - self.input_len()
                - 1 // pack size
                - tsize - osize;
        Output::new(bytes::unpack_uint(&node.data[i..], osize as u8))
    }

    #[inline]
    fn trans_addr(&self, node: &Node<'_>) -> CompiledAddr {
        let tsize = node.sizes.transition_pack_size();
        let i = node.start
                - self.input_len()
                - 1 // pack size
                - tsize;
        unpack_delta(&node.data[i..], tsize, node.end)
    }
}

impl StateAnyTrans {
    fn compile<W: io::Write>(
        mut wtr: W,
        addr: CompiledAddr,
        node: &BuilderNode,
    ) -> io::Result<()> {
        assert!(node.trans.len() <= 256);

        let mut tsize = 0;
        let mut osize = bytes::pack_size(node.final_output.value());
        let mut any_outs = !node.final_output.is_zero();
        for t in &node.trans {
            tsize = cmp::max(tsize, pack_delta_size(addr, t.addr));
            osize = cmp::max(osize, bytes::pack_size(t.out.value()));
            any_outs = any_outs || !t.out.is_zero();
        }

        let mut pack_sizes = PackSizes::new();
        if any_outs {
            pack_sizes.set_output_pack_size(osize);
        } else {
            pack_sizes.set_output_pack_size(0);
        }
        pack_sizes.set_transition_pack_size(tsize);

        let mut state = StateAnyTrans::new();
        state.set_final_state(node.is_final);
        state.set_state_ntrans(node.trans.len() as u8);

        if any_outs {
            if node.is_final {
                bytes::pack_uint_in(
                    &mut wtr,
                    node.final_output.value(),
                    osize,
                )?;
            }
            for t in node.trans.iter().rev() {
                bytes::pack_uint_in(&mut wtr, t.out.value(), osize)?;
            }
        }
        for t in node.trans.iter().rev() {
            pack_delta_in(&mut wtr, addr, t.addr, tsize)?;
        }
        for t in node.trans.iter().rev() {
            wtr.write_all(&[t.inp])?;
        }
        if node.trans.len() > TRANS_INDEX_THRESHOLD {
            // A value of 255 indicates that no transition exists for the byte
            // at that index. (Except when there are 256 transitions.) Namely,
            // any value greater than or equal to the number of transitions in
            // this node indicates an absent transition.
            let mut index = [255u8; 256];
            for (i, t) in node.trans.iter().enumerate() {
                index[t.inp as usize] = i as u8;
            }
            wtr.write_all(&index)?;
        }

        wtr.write_all(&[pack_sizes.encode()])?;
        if state.state_ntrans().is_none() {
            if node.trans.len() == 256 {
                // 256 can't be represented in a u8, so we abuse the fact that
                // the # of transitions can never be 1 here, since 1 is always
                // encoded in the state byte.
                wtr.write_all(&[1])?;
            } else {
                wtr.write_all(&[node.trans.len() as u8])?;
            }
        }
        wtr.write_all(&[state.0])?;
        Ok(())
    }

    #[inline]
    fn new() -> StateAnyTrans {
        StateAnyTrans(0b00_000000)
    }

    #[inline]
    fn set_final_state(&mut self, yes: bool) {
        if yes {
            self.0 |= 0b01_000000;
        }
    }

    #[inline]
    fn is_final_state(&self) -> bool {
        self.0 & 0b01_000000 == 0b01_000000
    }

    #[inline]
    fn set_state_ntrans(&mut self, n: u8) {
        if n <= 0b00_111111 {
            self.0 = (self.0 & 0b11_000000) | n;
        }
    }

    #[inline]
    fn state_ntrans(&self) -> Option<u8> {
        let n = self.0 & 0b00_111111;
        if n == 0 {
            None
        } else {
            Some(n)
        }
    }

    #[inline]
    fn sizes(&self, data: &[u8]) -> PackSizes {
        let i = data.len() - 1 - self.ntrans_len() - 1;
        PackSizes::decode(data[i])
    }

    #[inline]
    fn total_trans_size(
        &self,
        version: u64,
        sizes: PackSizes,
        ntrans: usize,
    ) -> usize {
        let index_size = self.trans_index_size(version, ntrans);
        ntrans + (ntrans * sizes.transition_pack_size()) + index_size
    }

    #[inline]
    fn trans_index_size(&self, version: u64, ntrans: usize) -> usize {
        if version >= 2 && ntrans > TRANS_INDEX_THRESHOLD {
            256
        } else {
            0
        }
    }

    #[inline]
    fn ntrans_len(&self) -> usize {
        if self.state_ntrans().is_none() {
            1
        } else {
            0
        }
    }

    #[inline]
    fn ntrans(&self, data: &[u8]) -> usize {
        if let Some(n) = self.state_ntrans() {
            n as usize
        } else {
            let n = data[data.len() - 2] as usize;
            if n == 1 {
                // "1" is never a normal legal value here, because if there
                // is only 1 transition, then it is encoded in the state byte.
                256
            } else {
                n
            }
        }
    }

    #[inline]
    fn final_output(
        &self,
        version: u64,
        data: &[u8],
        sizes: PackSizes,
        ntrans: usize,
    ) -> Output {
        let osize = sizes.output_pack_size();
        if osize == 0 || !self.is_final_state() {
            return Output::zero();
        }
        let at = data.len() - 1
                 - self.ntrans_len()
                 - 1 // pack size
                 - self.total_trans_size(version, sizes, ntrans)
                 - (ntrans * osize) // output values
                 - osize; // the desired output value
        Output::new(bytes::unpack_uint(&data[at..], osize as u8))
    }

    #[inline]
    fn end_addr(
        &self,
        version: u64,
        data: &[u8],
        sizes: PackSizes,
        ntrans: usize,
    ) -> usize {
        let osize = sizes.output_pack_size();
        let final_osize = if !self.is_final_state() { 0 } else { osize };
        data.len() - 1
        - self.ntrans_len()
        - 1 // pack size
        - self.total_trans_size(version, sizes, ntrans)
        - (ntrans * osize) // output values
        - final_osize // final output
    }

    #[inline]
    fn trans_addr(&self, node: &Node<'_>, i: usize) -> CompiledAddr {
        assert!(i < node.ntrans);
        let tsize = node.sizes.transition_pack_size();
        let at = node.start
                 - self.ntrans_len()
                 - 1 // pack size
                 - self.trans_index_size(node.version, node.ntrans)
                 - node.ntrans // inputs
                 - (i * tsize) // the previous transition addresses
                 - tsize; // the desired transition address
        unpack_delta(&node.data[at..], tsize, node.end)
    }

    #[inline]
    fn input(&self, node: &Node<'_>, i: usize) -> u8 {
        let at = node.start
                 - self.ntrans_len()
                 - 1 // pack size
                 - self.trans_index_size(node.version, node.ntrans)
                 - i
                 - 1; // the input byte
        node.data[at]
    }

    #[inline]
    fn find_input(&self, node: &Node<'_>, b: u8) -> Option<usize> {
        if node.version >= 2 && node.ntrans > TRANS_INDEX_THRESHOLD {
            let start = node.start
                        - self.ntrans_len()
                        - 1 // pack size
                        - self.trans_index_size(node.version, node.ntrans);
            let i = node.data[start + b as usize] as usize;
            if i >= node.ntrans {
                None
            } else {
                Some(i)
            }
        } else {
            let start = node.start
                        - self.ntrans_len()
                        - 1 // pack size
                        - node.ntrans; // inputs
            let end = start + node.ntrans;
            let inputs = &node.data[start..end];
            inputs.iter().position(|&b2| b == b2).map(|i| node.ntrans - i - 1)
        }
    }

    #[inline]
    fn output(&self, node: &Node<'_>, i: usize) -> Output {
        let osize = node.sizes.output_pack_size();
        if osize == 0 {
            return Output::zero();
        }
        let at = node.start
                 - self.ntrans_len()
                 - 1 // pack size
                 - self.total_trans_size(node.version, node.sizes, node.ntrans)
                 - (i * osize) // the previous outputs
                 - osize; // the desired output value
        Output::new(bytes::unpack_uint(&node.data[at..], osize as u8))
    }
}

// high 4 bits is transition address packed size.
// low 4 bits is output value packed size.
//
// `0` is a legal value which means there are no transitions/outputs.
#[derive(Clone, Copy, Debug)]
struct PackSizes(u8);

impl PackSizes {
    #[inline]
    fn new() -> PackSizes {
        PackSizes(0)
    }

    #[inline]
    fn decode(v: u8) -> PackSizes {
        PackSizes(v)
    }

    #[inline]
    fn encode(&self) -> u8 {
        self.0
    }

    #[inline]
    fn set_transition_pack_size(&mut self, size: u8) {
        assert!(size <= 8);
        self.0 = (self.0 & 0b0000_1111) | (size << 4);
    }

    #[inline]
    fn transition_pack_size(&self) -> usize {
        ((self.0 & 0b1111_0000) >> 4) as usize
    }

    #[inline]
    fn set_output_pack_size(&mut self, size: u8) {
        assert!(size <= 8);
        self.0 = (self.0 & 0b1111_0000) | size;
    }

    #[inline]
    fn output_pack_size(&self) -> usize {
        (self.0 & 0b0000_1111) as usize
    }
}

/// An iterator over all transitions in a node.
///
/// `'f` is the lifetime of the underlying fst and `'n` is the lifetime of
/// the underlying `Node`.
pub struct Transitions<'f, 'n> {
    node: &'n Node<'f>,
    range: Range<usize>,
}

impl<'f, 'n> Iterator for Transitions<'f, 'n> {
    type Item = Transition;

    #[inline]
    fn next(&mut self) -> Option<Transition> {
        self.range.next().map(|i| self.node.transition(i))
    }
}

/// common_idx translate a byte to an index in the COMMON_INPUTS_INV array.
///
/// I wonder if it would be prudent to store this mapping in the FST itself.
/// The advantage of doing so would mean that common inputs would reflect the
/// specific data in the FST. The problem of course is that this table has to
/// be computed up front, which is pretty much at odds with the streaming
/// nature of the builder.
///
/// Nevertheless, the *caller* may have a priori knowledge that could be
/// supplied to the builder manually, which could then be embedded in the FST.
#[inline]
pub fn common_idx(input: u8, max: u8) -> u8 {
    let val = COMMON_INPUTS[input as usize] as u32 + 1;
    let val = (val % 256) as u8;
    if val > max {
        0
    } else {
        val
    }
}

/// common_input translates a common input index stored in a serialized FST
/// to the corresponding byte.
#[inline]
pub fn common_input(idx: u8) -> Option<u8> {
    if idx == 0 {
        None
    } else {
        Some(COMMON_INPUTS_INV[(idx - 1) as usize])
    }
}

#[inline]
fn pack_delta<W: io::Write>(
    wtr: W,
    node_addr: CompiledAddr,
    trans_addr: CompiledAddr,
) -> io::Result<u8> {
    let nbytes = pack_delta_size(node_addr, trans_addr);
    pack_delta_in(wtr, node_addr, trans_addr, nbytes)?;
    Ok(nbytes)
}

#[inline]
fn pack_delta_in<W: io::Write>(
    wtr: W,
    node_addr: CompiledAddr,
    trans_addr: CompiledAddr,
    nbytes: u8,
) -> io::Result<()> {
    let delta_addr = if trans_addr == EMPTY_ADDRESS {
        EMPTY_ADDRESS
    } else {
        node_addr - trans_addr
    };
    bytes::pack_uint_in(wtr, delta_addr as u64, nbytes)
}

#[inline]
fn pack_delta_size(node_addr: CompiledAddr, trans_addr: CompiledAddr) -> u8 {
    let delta_addr = if trans_addr == EMPTY_ADDRESS {
        EMPTY_ADDRESS
    } else {
        node_addr - trans_addr
    };
    bytes::pack_size(delta_addr as u64)
}

#[inline]
fn unpack_delta(
    slice: &[u8],
    trans_pack_size: usize,
    node_addr: usize,
) -> CompiledAddr {
    let delta = bytes::unpack_uint(slice, trans_pack_size as u8);
    let delta_addr = u64_to_usize(delta);
    if delta_addr == EMPTY_ADDRESS {
        EMPTY_ADDRESS
    } else {
        node_addr - delta_addr
    }
}

#[cfg(test)]
mod tests {
    use quickcheck::{quickcheck, TestResult};

    use crate::raw::build::BuilderNode;
    use crate::raw::node::Node;
    use crate::raw::{Builder, CompiledAddr, Output, Transition, VERSION};
    use crate::stream::Streamer;

    const NEVER_LAST: CompiledAddr = std::u64::MAX as CompiledAddr;

    #[test]
    fn prop_emits_inputs() {
        fn p(mut bs: Vec<Vec<u8>>) -> TestResult {
            bs.sort();
            bs.dedup();

            let mut bfst = Builder::memory();
            for word in &bs {
                bfst.add(word).unwrap();
            }
            let fst = bfst.into_fst();
            let mut rdr = fst.stream();
            let mut words = vec![];
            while let Some(w) = rdr.next() {
                words.push(w.0.to_owned());
            }
            TestResult::from_bool(bs == words)
        }
        quickcheck(p as fn(Vec<Vec<u8>>) -> TestResult)
    }

    fn nodes_equal(compiled: &Node, uncompiled: &BuilderNode) -> bool {
        println!("compiled {:?}", compiled);
        assert_eq!(compiled.is_final(), uncompiled.is_final);
        assert_eq!(compiled.len(), uncompiled.trans.len());
        assert_eq!(compiled.final_output(), uncompiled.final_output);
        for (ct, ut) in
            compiled.transitions().zip(uncompiled.trans.iter().cloned())
        {
            assert_eq!(ct.inp, ut.inp);
            assert_eq!(ct.out, ut.out);
            assert_eq!(ct.addr, ut.addr);
        }
        true
    }

    fn compile(node: &BuilderNode) -> (CompiledAddr, Vec<u8>) {
        let mut buf = vec![0; 24];
        node.compile_to(&mut buf, NEVER_LAST, 24).unwrap();
        (buf.len() as CompiledAddr - 1, buf)
    }

    fn roundtrip(bnode: &BuilderNode) -> bool {
        let (addr, bytes) = compile(bnode);
        let node = Node::new(VERSION, addr, &bytes);
        nodes_equal(&node, &bnode)
    }

    fn trans(addr: CompiledAddr, inp: u8) -> Transition {
        Transition { inp, out: Output::zero(), addr }
    }

    #[test]
    fn bin_no_trans() {
        let bnode = BuilderNode {
            is_final: false,
            final_output: Output::zero(),
            trans: vec![],
        };
        let (addr, buf) = compile(&bnode);
        println!("buffer: {buf:?}");
        let node = Node::new(VERSION, addr, &buf);
        assert_eq!(node.as_slice().len(), 3);
        roundtrip(&bnode);
    }

    #[test]
    fn bin_one_trans_common() {
        let bnode = BuilderNode {
            is_final: false,
            final_output: Output::zero(),
            trans: vec![trans(20, b'a')],
        };
        let (addr, buf) = compile(&bnode);
        let node = Node::new(VERSION, addr, &buf);
        assert_eq!(node.as_slice().len(), 3);
        roundtrip(&bnode);
    }

    #[test]
    fn bin_one_trans_not_common() {
        let bnode = BuilderNode {
            is_final: false,
            final_output: Output::zero(),
            trans: vec![trans(2, b'\xff')],
        };
        let (addr, buf) = compile(&bnode);
        let node = Node::new(VERSION, addr, &buf);
        assert_eq!(node.as_slice().len(), 4);
        roundtrip(&bnode);
    }

    #[test]
    fn bin_many_trans() {
        let bnode = BuilderNode {
            is_final: false,
            final_output: Output::zero(),
            trans: vec![
                trans(2, b'a'),
                trans(3, b'b'),
                trans(4, b'c'),
                trans(5, b'd'),
                trans(6, b'e'),
                trans(7, b'f'),
            ],
        };
        let (addr, buf) = compile(&bnode);
        println!("buffer: {buf:?}");
        let node = Node::new(VERSION, addr, &buf);
        assert_eq!(node.as_slice().len(), 14);
        roundtrip(&bnode);
    }

    #[test]
    fn node_max_trans() {
        let bnode = BuilderNode {
            is_final: false,
            final_output: Output::zero(),
            trans: (0..256).map(|i| trans(0, i as u8)).collect(),
        };
        let (addr, buf) = compile(&bnode);
        let node = Node::new(VERSION, addr, &buf);
        assert_eq!(node.transitions().count(), 256);
        assert_eq!(node.len(), node.transitions().count());
        roundtrip(&bnode);
    }
}
