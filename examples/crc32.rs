const CASTAGNOLI_POLY: u32 = 0x82f63b78;

fn main() {
   for b in 0u8..=255 {
        tag_entry(b);
    }

    let table = make_table(CASTAGNOLI_POLY);
    let table16 = make_table16(CASTAGNOLI_POLY);

    println!("Table: {table:?}\n\n");
    println!("Table16: {table16:?}");
}

fn tag_entry(b: u8) -> u16 {
    let b = b as u16;
    match b & 0b00000011 {
        0b00 => {
            let lit_len = (b >> 2) + 1;
            if lit_len <= 60 {
                lit_len
            } else {
                assert!(lit_len <= 64);
                (lit_len - 60) << 11
            }
        }
        0b01 => {
            let len = 4 + ((b >> 2) & 0b111);
            let offset = (b >> 5) & 0b111;
            (1 << 11) | (offset << 8) | len
        }
        0b10 => {
            let len = 1 + (b >> 2);
            (2 << 11) | len
        }
        0b11 => {
            let len = 1 + (b >> 2);
            (4 << 11) | len
        }
        _ => unreachable!(),
    }
}

fn make_table16(poly: u32) -> [[u32; 256]; 16] {
    let mut tab = [[0; 256]; 16];
    tab[0] = make_table(poly);
    for i in 0..256 {
        let mut crc = tab[0][i];
        for j in 1..16 {
            crc = (crc >> 8) ^ tab[0][crc as u8 as usize];
            tab[j][i] = crc;
        }
    }
    tab
}

fn make_table(poly: u32) -> [u32; 256] {
    let mut tab = [0; 256];
    for i in 0u32..256u32 {
        let mut crc = i;
        for _ in 0..8 {
            if crc & 1 == 1 {
                crc = (crc >> 1) ^ poly;
            } else {
                crc >>= 1;
            }
        }
        tab[i as usize] = crc;
    }
    tab
}
