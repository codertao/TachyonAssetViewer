//! CBIN config container: decrypt and walk groups/items/params.
//! Port of TachyonFileFormats/src/cbin.py (layout documented in ksy/cbin.ksy).

use anyhow::{ensure, Result};

#[derive(Debug, Clone)]
pub enum Value {
    Int(i32),
    Float(f32),
    Str(String),
}

impl Value {
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Value::Int(i) => Some(*i as f32),
            Value::Float(f) => Some(*f),
            Value::Str(_) => None,
        }
    }
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) => Some(*f as i32),
            Value::Str(_) => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub key: String,
    pub values: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub items: Vec<Item>,
}

impl Group {
    pub fn get(&self, key: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.key.eq_ignore_ascii_case(key))
    }
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Item> + 'a {
        self.items.iter().filter(move |i| i.key.eq_ignore_ascii_case(key))
    }
}

fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    ensure!(o + 4 <= d.len(), "CBIN truncated at {o:#x}");
    Ok(u32::from_le_bytes(d[o..o + 4].try_into().unwrap()))
}

pub fn decrypt(d: &[u8]) -> Result<Vec<u8>> {
    ensure!(d.len() >= 20 && &d[..4] == b"CBIN", "not a CBIN file");
    let mut key = u32_at(d, 16)?;
    let mut out = d[..20].to_vec();
    out.extend(d[20..].iter().map(|&c| {
        key = key.rotate_left(7);
        c ^ (key as u8)
    }));
    Ok(out)
}

pub fn parse(raw: &[u8]) -> Result<Vec<Group>> {
    let d = decrypt(raw)?;
    let text_ofs = u32_at(&d, 4)? as usize;
    let text_len = u32_at(&d, 8)? as usize;
    ensure!(text_ofs + text_len <= d.len(), "CBIN string table out of range");
    let strings: Vec<String> = d[text_ofs..text_ofs + text_len]
        .split(|&c| c == 0)
        .map(|s| s.iter().map(|&c| c as char).collect())
        .collect();
    let s = |i: u32| -> String {
        if i == 0 {
            String::new()
        } else {
            strings.get(i as usize - 1).cloned().unwrap_or_default()
        }
    };

    let ng = u32_at(&d, 20)? as usize;
    ensure!(ng < 100_000, "implausible group count");
    let mut o = 24;
    let mut groups_raw = Vec::with_capacity(ng);
    for _ in 0..ng {
        groups_raw.push((u32_at(&d, o)?, u32_at(&d, o + 4)? as usize));
        o += 8;
    }
    // Items: for each group with item_count > 0, item_count + 1 records (last is a terminator).
    let mut items_raw: Vec<Vec<(u32, usize)>> = Vec::with_capacity(ng);
    for &(_, ic) in &groups_raw {
        let mut v = Vec::new();
        if ic > 0 {
            for _ in 0..=ic {
                v.push((u32_at(&d, o)?, u32_at(&d, o + 4)? as usize));
                o += 8;
            }
        }
        items_raw.push(v);
    }
    // Params follow, in item order.
    let mut groups = Vec::with_capacity(ng);
    for (gi, &(gid, _)) in groups_raw.iter().enumerate() {
        let mut items = Vec::new();
        for &(iid, pc) in &items_raw[gi] {
            let mut values = Vec::with_capacity(pc);
            for _ in 0..pc {
                let raw = u32_at(&d, o)?;
                let ty = u32_at(&d, o + 4)?;
                o += 8;
                values.push(match ty {
                    1 => Value::Int(raw as i32),
                    2 => Value::Float(f32::from_bits(raw)),
                    _ => Value::Str(s(raw)),
                });
            }
            if iid != 0 {
                items.push(Item { key: s(iid), values });
            }
        }
        groups.push(Group { name: s(gid), items });
    }
    Ok(groups)
}
