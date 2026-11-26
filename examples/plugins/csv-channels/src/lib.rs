//! Example embdbg decoder plugin: CSV lines into channels.
//!
//! A frame such as `1.5,2.0,3.25` becomes three channel samples, each multiplied by the
//! `scale` parameter. Build with:
//!
//! ```text
//! rustup target add wasm32-unknown-unknown
//! cargo build --release --target wasm32-unknown-unknown
//! copy target/wasm32-unknown-unknown/release/csv_channels.wasm decoder.wasm
//! ```
//!
//! The ABI is documented in `docs/plugins.md`. No imports and no file or network access: the host
//! only writes frames into `INPUT` and reads the block `decode` returns.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU32, Ordering};

const INPUT_LEN: usize = 4096;
const OUTPUT_LEN: usize = 4096;
const PARAM_LEN: usize = 512;

struct Buffer<const N: usize>(UnsafeCell<[u8; N]>);

// The host calls one instance at a time; the cells make the mutation explicit without
// `static mut`.
unsafe impl<const N: usize> Sync for Buffer<N> {}

static INPUT: Buffer<INPUT_LEN> = Buffer(UnsafeCell::new([0; INPUT_LEN]));
static OUTPUT: Buffer<OUTPUT_LEN> = Buffer(UnsafeCell::new([0; OUTPUT_LEN]));
static PARAMS: Buffer<PARAM_LEN> = Buffer(UnsafeCell::new([0; PARAM_LEN]));
/// 1.0f32, the default scale.
static SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000);

fn buffer_mut<const N: usize>(buffer: &Buffer<N>) -> &'static mut [u8] {
    unsafe { &mut *buffer.0.get() }
}

#[unsafe(no_mangle)]
pub extern "C" fn input_ptr() -> i32 {
    buffer_mut(&INPUT).as_ptr() as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn input_capacity() -> i32 {
    INPUT_LEN as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn alloc(size: i32) -> i32 {
    if size < 0 || size as usize > PARAM_LEN {
        return 0;
    }
    buffer_mut(&PARAMS).as_mut_ptr() as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn set_params(ptr: i32, len: i32) {
    if ptr < 0 || len < 0 {
        return;
    }
    let params = buffer_mut(&PARAMS);
    let len = (len as usize).min(params.len());
    let Ok(text) = std::str::from_utf8(&params[..len]) else {
        return;
    };
    if let Some(scale) = parse_number(text, "scale") {
        SCALE.store(scale.to_bits(), Ordering::Relaxed);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn decode(len: i32) -> i32 {
    if len <= 0 {
        return 0;
    }
    let input = buffer_mut(&INPUT);
    let len = (len as usize).min(input.len());
    let Ok(text) = std::str::from_utf8(&input[..len]) else {
        return 0;
    };
    let scale = f32::from_bits(SCALE.load(Ordering::Relaxed));
    let mut values = Vec::new();
    for part in text.trim().split(',') {
        let Ok(value) = part.trim().parse::<f32>() else {
            return 0;
        };
        values.push(value * scale);
    }
    if values.is_empty() || values.len() * 4 + 5 > OUTPUT_LEN {
        return 0;
    }

    let output = buffer_mut(&OUTPUT);
    output[..4].copy_from_slice(&((values.len() * 4) as u32).to_le_bytes());
    output[4] = 0; // kind 0: f32 samples
    for (index, value) in values.iter().enumerate() {
        let at = 5 + index * 4;
        output[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    output.as_mut_ptr() as i32
}

/// Reads `"scale": 2.5` from the tiny JSON object the host passes. A real plugin can use
/// serde_json instead; this keeps the example dependency-free.
fn parse_number(json: &str, key: &str) -> Option<f32> {
    let key_at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[key_at..];
    let colon = rest.find(':')?;
    rest[colon + 1..]
        .trim_start()
        .split([',', '}'])
        .next()?
        .trim()
        .parse()
        .ok()
}
