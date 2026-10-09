//! The grid, saved every few seconds so that reloading the page or re-running
//! the binary picks up the same forest: in localStorage on the web (base64),
//! and in `$XDG_STATE_HOME/tinderbox/grid.bin` natively. The cells are
//! deflated; a typical grid is a few hundred KB raw. The river's course is
//! saved alongside it (`tinderbox:river`, `river.bin`).

use crate::sim::Snapshot;

const MAGIC: &[u8; 4] = b"TBG1";
const HEADER: usize = 4 + 4 + 4 + 8 + 4;

/// The saved grid and the cell size it was simulated at.
pub fn load() -> Option<(Snapshot, f32)> {
    decode(&storage::read(GRID)?)
}

pub fn store(s: &Snapshot, cell_size: f32) {
    storage::write(GRID, &encode(s, cell_size));
}

/// The river's course, as `River::save` wrote it.
pub fn load_river() -> Option<Vec<u8>> {
    storage::read(RIVER)
}

pub fn store_river(b: &[u8]) {
    storage::write(RIVER, b);
}

/// Saved under `tinderbox:NAME` on the web, `NAME.bin` natively.
const GRID: &str = "grid";
const RIVER: &str = "river";

fn encode(s: &Snapshot, cell_size: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + s.cells.len() / 4);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&s.width.to_le_bytes());
    out.extend_from_slice(&s.height.to_le_bytes());
    out.extend_from_slice(&s.generation.to_le_bytes());
    out.extend_from_slice(&cell_size.to_le_bytes());
    out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(&s.cells, 6));
    out
}

fn decode(b: &[u8]) -> Option<(Snapshot, f32)> {
    if b.len() < HEADER || &b[..4] != MAGIC {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let (width, height) = (u32_at(4), u32_at(8));
    let generation = u64::from_le_bytes(b[12..20].try_into().unwrap());
    let cell_size = f32::from_le_bytes(b[20..24].try_into().unwrap());
    let len = width as usize * height as usize * 4;
    let cells = miniz_oxide::inflate::decompress_to_vec_with_limit(&b[HEADER..], len).ok()?;
    (cells.len() == len).then_some((Snapshot { width, height, generation, cells }, cell_size))
}

#[cfg(target_arch = "wasm32")]
mod storage {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn local() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn read(name: &str) -> Option<Vec<u8>> {
        let s = local()?.get_item(&format!("tinderbox:{name}")).ok()??;
        let digit = |c: u8| B64.iter().position(|&d| d == c).map(|v| v as u32);
        let mut out = Vec::with_capacity(s.len() * 3 / 4);
        for chunk in s.trim_end_matches('=').as_bytes().chunks(4) {
            let mut n = 0;
            for (i, &c) in chunk.iter().enumerate() {
                n |= digit(c)? << (18 - 6 * i);
            }
            out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
        }
        Some(out)
    }

    pub fn write(name: &str, b: &[u8]) {
        let Some(l) = local() else { return };
        let key = format!("tinderbox:{name}");
        let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
        for chunk in b.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, &v)| n | (v as u32) << (16 - 8 * i));
            for i in 0..4 {
                s.push(if i <= chunk.len() { B64[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        // Over quota: better no save than a stale one.
        if l.set_item(&key, &s).is_err() {
            let _ = l.remove_item(&key);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod storage {
    use std::path::PathBuf;

    fn path(name: &str) -> Option<PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
        Some(base.join("tinderbox").join(format!("{name}.bin")))
    }

    pub fn read(name: &str) -> Option<Vec<u8>> {
        std::fs::read(path(name)?).ok()
    }

    /// Written aside and renamed into place, so a crash mid-write can't
    /// leave a torn file.
    pub fn write(name: &str, b: &[u8]) {
        let Some(p) = path(name) else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let cells: Vec<u8> = (0..3 * 2 * 4).map(|i| (i * 37 % 256) as u8).collect();
        let s = Snapshot { width: 3, height: 2, generation: 12345, cells: cells.clone() };
        let (t, cell) = decode(&encode(&s, 3.0)).unwrap();
        assert_eq!((t.width, t.height, t.generation, cell), (3, 2, 12345, 3.0));
        assert_eq!(t.cells, cells);
    }
}
