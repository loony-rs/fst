use fst::raw::common_inputs::{COMMON_INPUTS, COMMON_INPUTS_INV};

pub fn common_idx(input: u8, max: u8) -> u8 {
    let val = COMMON_INPUTS[input as usize] as u32 + 1;
    let val = (val % 256) as u8;
    if val > max {
        0
    } else {
        val
    }
}

fn common_input(idx: u8) -> Option<u8> {
    if idx == 0 {
        None
    } else {
        Some(COMMON_INPUTS_INV[(idx - 1) as usize])
    }
}

fn main() {
    let data = "abcdefghijklmnopqrstuvwxyz".as_bytes();
    
    for x in data.iter() {
        let res = 0b11_000000 & 0b11_000000 | common_idx(*x, 0b111111);
        let res1 = common_input(res & 0b00_111111);
        println!("{res} {res1:?}");
    }
}