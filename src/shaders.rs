//! WGSL sources. WGSL has no includes, so each module is assembled from a
//! prelude of constants generated here, `common.wgsl`, and its own files.

/// Lightning strikes per step whose cells are picked on the CPU (see sim.rs).
pub const MAX_STRIKES: usize = 16;

pub mod state {
    pub const EMPTY: u8 = 0;
    pub const TREE: u8 = 1;
    pub const FIRE: u8 = 2;
    pub const BURNT: u8 = 3;
    /// River or lake; set and cleared by the step shader from the river field.
    pub const WATER: u8 = 4;
}

pub mod cause {
    pub const NONE: u8 = 0;
    pub const GROW: u8 = 1;
    pub const SPREAD: u8 = 2;
    pub const LIGHTNING: u8 = 3;
    pub const BURNOUT: u8 = 4;
    pub const BURNING: u8 = 5;
    pub const CLEAR: u8 = 6;
    pub const MATURE: u8 = 7;
    /// The river took the cell (from the state in g).
    pub const ERODE: u8 = 8;
    /// The river left the cell.
    pub const DEPOSIT: u8 = 9;
    /// Caught from embers blown across the water.
    pub const SPOT: u8 = 10;
}

/// Ground age of grass that has never burned.
pub const SETTLED: u8 = 255;

/// Neighbor offsets, in the order of the bits in a fire's sources mask (y
/// up): W, N, E, S, SW, NW, NE, SE.
const NEIGHBORS: [(i32, i32); 8] = [(-1, 0), (0, 1), (1, 0), (0, -1), (-1, -1), (-1, 1), (1, 1), (1, -1)];

fn prelude() -> String {
    use cause::*;
    use state::*;
    let consts = [
        ("EMPTY", EMPTY),
        ("TREE", TREE),
        ("FIRE", FIRE),
        ("BURNT", BURNT),
        ("WATER", WATER),
        ("CAUSE_NONE", NONE),
        ("CAUSE_GROW", GROW),
        ("CAUSE_SPREAD", SPREAD),
        ("CAUSE_LIGHTNING", LIGHTNING),
        ("CAUSE_BURNOUT", BURNOUT),
        ("CAUSE_BURNING", BURNING),
        ("CAUSE_CLEAR", CLEAR),
        ("CAUSE_MATURE", MATURE),
        ("CAUSE_ERODE", ERODE),
        ("CAUSE_DEPOSIT", DEPOSIT),
        ("CAUSE_SPOT", SPOT),
        ("SETTLED", SETTLED),
    ];
    let mut s: String = consts.iter().map(|(n, v)| format!("const {n}: u32 = {v}u;\n")).collect();
    s += &format!("const MAX_STRIKES: u32 = {MAX_STRIKES}u;\n");
    s += &format!("const RIPARIAN: f32 = {:?};\n", crate::river::RIPARIAN);
    let nb: Vec<String> = NEIGHBORS.iter().map(|(x, y)| format!("vec2<i32>({x}, {y})")).collect();
    s += &format!("const NEIGHBORS = array<vec2<i32>, 8>({});\n", nb.join(", "));
    s + include_str!("shaders/common.wgsl")
}

/// Compile a WGSL module. On the web, naga's bounds checks are dropped: in
/// WebGL2 it would clamp every texel fetch and array index (a textureSize call
/// per fetch, in the hottest loops), and the browser already makes
/// out-of-bounds access safe. WebGPU compiles the WGSL itself, unaffected.
pub fn module(device: &wgpu::Device, label: &str, src: String) -> wgpu::ShaderModule {
    let desc = wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(src.into()) };
    #[cfg(target_arch = "wasm32")]
    // SAFETY: WebGL2 and WebGPU guarantee out-of-bounds accesses are defined.
    return unsafe { device.create_shader_module_trusted(desc, wgpu::ShaderRuntimeChecks::unchecked()) };
    #[cfg(not(target_arch = "wasm32"))]
    device.create_shader_module(desc)
}

pub fn step() -> String {
    prelude() + include_str!("shaders/step.wgsl")
}

pub fn scene() -> String {
    prelude() + include_str!("shaders/view.wgsl") + include_str!("shaders/scene.wgsl")
}

pub fn flat() -> String {
    prelude() + include_str!("shaders/view.wgsl") + include_str!("shaders/flat.wgsl")
}

pub fn lightning() -> String {
    prelude() + include_str!("shaders/lightning.wgsl")
}

pub fn blit() -> String {
    prelude() + include_str!("shaders/blit.wgsl")
}

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^= x >> 16;
    x
}

/// The shaders' `hashf`, so the CPU can find where a lightning fire starts.
pub fn hashf(cx: i32, cy: i32, salt: u32) -> f32 {
    (hash(cx as u32 ^ hash(cy as u32 ^ hash(salt))) & 0xffffff) as f32 / 16777216.0
}
