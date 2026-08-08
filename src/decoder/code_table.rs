pub const NOOP: u8 = 0;
pub const ADD: u8 = 1;
pub const RUN: u8 = 2;
pub const COPY: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeltaInst {
    pub inst: u8,
    pub size: u8,
    pub mode: u8,
}

impl DeltaInst {
    pub const NOOP: DeltaInst = DeltaInst {
        inst: NOOP,
        size: 0,
        mode: 0,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeTableEntry {
    pub first: DeltaInst,
    pub second: DeltaInst,
}

#[derive(Clone, Debug)]
pub struct CodeTable {
    pub s_near: usize,
    pub s_same: usize,
    pub entries: Box<[CodeTableEntry; 256]>,
}

pub fn default_code_table() -> CodeTable {
    CodeTable {
        s_near: 4,
        s_same: 3,
        entries: Box::new(build_default()),
    }
}

fn build_default() -> [CodeTableEntry; 256] {
    let noop = DeltaInst::NOOP;
    let mut t = [CodeTableEntry {
        first: noop,
        second: noop,
    }; 256];
    let mut idx = 0usize;

    t[idx] = CodeTableEntry {
        first: DeltaInst {
            inst: RUN,
            size: 0,
            mode: 0,
        },
        second: noop,
    };
    idx += 1;

    for size in 0..=17u8 {
        t[idx] = CodeTableEntry {
            first: DeltaInst {
                inst: ADD,
                size,
                mode: 0,
            },
            second: noop,
        };
        idx += 1;
    }

    for mode in 0..=8u8 {
        for size in 0..=15u8 {
            let s = if size == 0 { 0 } else { size + 3 };
            t[idx] = CodeTableEntry {
                first: DeltaInst {
                    inst: COPY,
                    size: s,
                    mode,
                },
                second: noop,
            };
            idx += 1;
        }
    }

    for mode in 0..=5u8 {
        for add_size in 1..=4u8 {
            for copy_size in 4..=6u8 {
                t[idx] = CodeTableEntry {
                    first: DeltaInst {
                        inst: ADD,
                        size: add_size,
                        mode: 0,
                    },
                    second: DeltaInst {
                        inst: COPY,
                        size: copy_size,
                        mode,
                    },
                };
                idx += 1;
            }
        }
    }

    for mode in 6..=8u8 {
        for add_size in 1..=4u8 {
            t[idx] = CodeTableEntry {
                first: DeltaInst {
                    inst: ADD,
                    size: add_size,
                    mode: 0,
                },
                second: DeltaInst {
                    inst: COPY,
                    size: 4,
                    mode,
                },
            };
            idx += 1;
        }
    }

    for mode in 0..=8u8 {
        t[idx] = CodeTableEntry {
            first: DeltaInst {
                inst: COPY,
                size: 4,
                mode,
            },
            second: DeltaInst {
                inst: ADD,
                size: 1,
                mode: 0,
            },
        };
        idx += 1;
    }

    debug_assert_eq!(idx, 256);
    t
}

pub fn serialize_table(table: &CodeTable) -> [u8; 1536] {
    let mut out = [0u8; 1536];
    let e = &table.entries;
    for i in 0..256 {
        out[i] = e[i].first.inst;
        out[256 + i] = e[i].second.inst;
        out[512 + i] = e[i].first.size;
        out[768 + i] = e[i].second.size;
        out[1024 + i] = e[i].first.mode;
        out[1280 + i] = e[i].second.mode;
    }
    out
}

pub fn deserialize_table(
    bytes: &[u8],
    s_near: usize,
    s_same: usize,
) -> Result<CodeTable, crate::errors::Error> {
    if bytes.len() != 1536 {
        return Err(crate::errors::Error::Format("code table data wrong size"));
    }
    if s_near > 255 || s_same > 255 || s_near + s_same + 2 > 256 {
        return Err(crate::errors::Error::Format(
            "invalid code table cache sizes",
        ));
    }
    let mut entries: Vec<CodeTableEntry> = Vec::with_capacity(256);
    for i in 0..256usize {
        entries.push(CodeTableEntry {
            first: DeltaInst {
                inst: bytes[i],
                size: bytes[512 + i],
                mode: bytes[1024 + i],
            },
            second: DeltaInst {
                inst: bytes[256 + i],
                size: bytes[768 + i],
                mode: bytes[1280 + i],
            },
        });
    }
    let arr: [CodeTableEntry; 256] = entries
        .try_into()
        .map_err(|_| crate::errors::Error::Format("code table size"))?;
    Ok(CodeTable {
        s_near,
        s_same,
        entries: Box::new(arr),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_table_index_ranges() {
        let t = default_code_table();
        assert_eq!(t.s_near, 4);
        assert_eq!(t.s_same, 3);
        assert_eq!(t.entries[0].first.inst, RUN);
        assert_eq!(t.entries[0].first.size, 0);
        assert_eq!(t.entries[1].first.inst, ADD);
        assert_eq!(t.entries[18].first.size, 17);
        assert_eq!(t.entries[19].first.inst, COPY);
        assert_eq!(t.entries[19].first.size, 0);
        assert_eq!(t.entries[19].first.mode, 0);
        assert_eq!(t.entries[162].first.mode, 8);
        assert_eq!(t.entries[163].first.inst, ADD);
        assert_eq!(t.entries[163].second.inst, COPY);
        assert_eq!(t.entries[163].second.size, 4);
        assert_eq!(t.entries[163].second.mode, 0);
        assert_eq!(t.entries[235].second.mode, 6);
        assert_eq!(t.entries[247].first.inst, COPY);
        assert_eq!(t.entries[247].first.size, 4);
        assert_eq!(t.entries[247].second.inst, ADD);
        assert_eq!(t.entries[255].first.mode, 8);
    }

    #[test]
    fn table_serialize_roundtrip() {
        let t = default_code_table();
        let bytes = serialize_table(&t);
        let back = deserialize_table(&bytes, 4, 3).unwrap();
        assert_eq!(t.entries[0], back.entries[0]);
        assert_eq!(t.entries[255], back.entries[255]);
    }
}
