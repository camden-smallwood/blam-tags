//! Halo render_method runtime types.
//!
//! Mirrors Bungie's runtime C++ from Ares (`source/render_methods/
//! render_method_definitions.h`) — verbatim type and field names so
//! that decompile / TagTool / Ares cross-references stay legible.
//!
//! The four tag groups in this universe and their runtime classes:
//!
//! ```text
//! rm**  (rmsh, rmtr, rmw, rmfl, rmd, rmhg, rmsk, rmct, rmcs, rmp,
//!        rmb, rmco, rmlv) ─► c_render_method
//!                              ├─ definition: c_render_method_definition  (rmdf)
//!                              │   └─ categories[].options[]: c_render_method_option  (rmop)
//!                              ├─ parameters[]: s_render_method_parameter  (per-instance, animated)
//!                              └─ postprocess_definition: s_render_method_postprocess_definition
//!                                  └─ template: c_render_method_template  (rmt2)
//! ```
//!
//! ## Parameter resolution paths
//!
//! Two paths feed values into the GPU constant buffer at slot 13:
//!
//! 1. **Per-instance, possibly animated.** [`RenderMethod::parameters`]
//!    holds `s_render_method_parameter` entries that the runtime
//!    re-evaluates each frame via [`crate::TagFunction`].
//! 2. **Pre-baked postprocess.** [`RenderMethodPostprocessDefinition`]
//!    holds the routed cbuffer layout, `s_render_method_routing_info`
//!    table, and per-pass index ranges — the fast path used when no
//!    parameters are animated.
//!
//! Walker (in a sibling module) resolves both into a unified per-pass
//! constants vector.
//!
//! ## Naming
//!
//! Bungie C++ types drop their `c_` / `s_` / `e_` prefix and member `m_`
//! prefix on the way to Rust. Field names use Bungie's snake_case form
//! (which matches the IDA decompile), even where the on-disk schema
//! uses spaces and informal aliases ("postprocess" → `postprocess_definition`,
//! "shader flags*" → `flags`, etc.).

mod cbuffer;
mod types;
mod walker;

pub use cbuffer::{
    compile_real_constant, compile_real_constant_at_time, is_cbuffer_animated,
    pack_pixel_cbuffer_at_time, pack_vertex_cbuffer_at_time, rebuild_cbuffer_bytes_at_time,
    rebuild_cbuffer_bytes_with_optional_rmt2, resolve_pixel_user_cbuffer,
    resolve_pixel_user_cbuffer_at_time, CbufferSlot, ResolvedCbuffer,
};
pub use types::*;
pub use walker::{
    build_rmop_param_list, BitmapBinding, ExternResolver, NullExternResolver, ParameterSource,
    ResolvedParameter, ResolvedRenderMethod, ResolvedValue,
};
