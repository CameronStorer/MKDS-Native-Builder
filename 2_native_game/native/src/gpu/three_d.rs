//! GPU 3D renderer (block 2 stage B2.2): draws one [`RenderSnapshot`]'s polygon list on the GPU at `scale` x the
//! console's 256x192.
//!
//! Contract (the software rasterizer `recomp::raster` + `RenderSnapshot::layer3d` is the reference):
//! - Output: an `Rgba8Unorm` texture of 256*scale x 192*scale, top row first, cleared to CLEAR_COLOR
//!   (`io[0x350..0x354]`: RGB555 + 5-bit alpha, channels expanded `(v << 3) | (v >> 2)`, alpha `a * 255 / 31`).
//!   Alpha 0 means "no 3D pixel here" to the 2D compositor, exactly as in the software layer.
//! - Same inputs and rules as `recomp::raster::render`: viewport, near-plane clipping, perspective-correct colour and
//!   texture coordinates, every texture format through `recomp::texture` decoding (decode on the CPU, cache on the
//!   GPU keyed by texture parameters + palette base + a hash of the bytes used), wrap/repeat/flip, modulation and
//!   decal modes, front/back face selection, depth test (less, or equal-within-tolerance), translucent depth writes
//!   only when flagged, polygon alpha blending in submission order, shadow-volume masks and polygon-ID rejection.
//! - At scale 1, `render_compare` must report >= 99% identical pixels per frame and none below 97% against
//!   `RenderSnapshot::layer3d`; remaining differences must be explained (e.g. rounding of interpolation).
use super::Gpu;
use recomp::machine::RenderSnapshot;

pub struct Renderer3d {
    scale: u32,
    target: wgpu::Texture,
}

impl Renderer3d {
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    pub fn new(gpu: &Gpu, scale: u32) -> Renderer3d {
        assert!((1..=8).contains(&scale), "3D scale must be 1-8");
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("3d layer"),
            size: wgpu::Extent3d { width: 256 * scale, height: 192 * scale, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Renderer3d { scale, target }
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// Renders `snap`'s 3D frame; the returned texture stays valid until the next call.
    pub fn render(&mut self, gpu: &Gpu, snap: &RenderSnapshot) -> &wgpu::Texture {
        let _ = snap; // B2.2: not implemented yet; the layer stays transparent.
        let view = self.target.create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("3d clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        gpu.queue.submit([encoder.finish()]);
        &self.target
    }
}
