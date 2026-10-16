//! Screen compositor (block 2 stage B2.3): both screens from the two engines' [`Split`]s and the 3D layer, at the 3D
//! layer's scale. The shader (`compose.wgsl`) is `Split::pixel`, sample by sample. `render_raw` builds the split
//! buffers on the GPU; `render` accepts CPU splits for comparison.
use super::Gpu;
use recomp::gfx2d::Split;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Engine {
    eva: u32,
    evb: u32,
    bright_mode: u32,
    bright: u32,
    master_mode: u32,
    master: u32,
    pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Frame {
    engines: [Engine; 2],
    scale: u32,
    a_top: u32,
    pad: [u32; 2],
}

const PIXELS: usize = 256 * 192;

pub struct Compositor {
    scale: u32,
    pipeline: wgpu::ComputePipeline,
    base: wgpu::Buffer,
    other: wgpu::Buffer,
    info: wgpu::Buffer,
    uniform: wgpu::Buffer,
    output: wgpu::Texture,
    staging: Vec<u32>,
    two_d: Option<super::two_d::Renderer2d>,
}

impl Compositor {
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    pub fn new(gpu: &Gpu, scale: u32) -> Compositor {
        let device = &gpu.device;
        let storage = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (2 * PIXELS * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compositor"),
            source: wgpu::ShaderSource::Wgsl(include_str!("compose.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("compositor"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screens"),
            size: wgpu::Extent3d { width: 256 * scale, height: 384 * scale, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            // The window samples it through an sRGB view when the window itself is sRGB (colours pass unchanged).
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        Compositor {
            scale,
            pipeline,
            base: storage("compositor base"),
            other: storage("compositor other"),
            info: storage("compositor info"),
            uniform: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("compositor frame"),
                size: std::mem::size_of::<Frame>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            output,
            staging: vec![0; 2 * PIXELS],
            two_d: None,
        }
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// The output texture (both screens, top first), also between frames.
    pub fn output(&self) -> &wgpu::Texture {
        &self.output
    }

    /// Both screens (top first) at the compositor's scale. `layer3d` must be 256*scale x 192*scale `Rgba8Unorm`.
    pub fn render(&mut self, gpu: &Gpu, a: &Split, b: &Split, a_top: bool, layer3d: &wgpu::Texture) -> &wgpu::Texture {
        assert_eq!((layer3d.width(), layer3d.height()), (256 * self.scale, 192 * self.scale), "3D layer scale");
        let mut upload = |buffer: &wgpu::Buffer, field: &dyn Fn(&Split, usize) -> u32| {
            for (e, split) in [a, b].into_iter().enumerate() {
                for i in 0..PIXELS {
                    self.staging[e * PIXELS + i] = field(split, i);
                }
            }
            gpu.queue.write_buffer(buffer, 0, bytemuck::cast_slice(&self.staging));
        };
        upload(&self.base, &|s, i| s.base[i]);
        upload(&self.other, &|s, i| s.other[i]);
        upload(&self.info, &|s, i| s.info[i] as u32);
        let engine = |s: &Split| Engine {
            eva: s.eva,
            evb: s.evb,
            bright_mode: s.bright.0,
            bright: s.bright.1,
            master_mode: s.master.0,
            master: s.master.1,
            pad: [0; 2],
        };
        let frame = Frame { engines: [engine(a), engine(b)], scale: self.scale, a_top: a_top as u32, pad: [0; 2] };
        gpu.queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&frame));
        self.dispatch(gpu, layer3d)
    }

    pub fn prepare_raw(&mut self, gpu: &Gpu) {
        self.two_d.get_or_insert_with(|| super::two_d::Renderer2d::new(gpu));
    }

    /// Build the split buffers on the GPU directly from the raw snapshot.
    pub fn render_raw(&mut self, gpu: &Gpu, snap: &recomp::machine::RenderSnapshot, layer3d: &wgpu::Texture) -> &wgpu::Texture {
        assert_eq!((layer3d.width(), layer3d.height()), (256 * self.scale, 192 * self.scale), "3D layer scale");
        let renderer = self.two_d.get_or_insert_with(|| super::two_d::Renderer2d::new(gpu));
        renderer.render(gpu, snap, &self.base, &self.other, &self.info);
        let engine = |offset: usize| {
            let r = |a: usize| u16::from_le_bytes([snap.io[offset + a], snap.io[offset + a + 1]]) as u32;
            let d = r(0) | r(2) << 16;
            let active = (d >> 16) & 3 == 1 && d & 0x80 == 0;
            Engine { eva: (r(0x52) & 31).min(16), evb: ((r(0x52) >> 8) & 31).min(16),
                bright_mode: (r(0x50) >> 6) & 3, bright: (r(0x54) & 31).min(16),
                master_mode: if active { r(0x6c) >> 14 } else { 0 },
                master: if active { (r(0x6c) & 31).min(16) } else { 0 }, pad: [0; 2] }
        };
        let frame = Frame { engines: [engine(0), engine(0x1000)], scale: self.scale,
            a_top: ((snap.io[0x305] >> 7) & 1) as u32, pad: [0; 2] };
        gpu.queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&frame));
        self.dispatch(gpu, layer3d)
    }

    /// Comparison only: verify every split record, including records hidden by the current 3D layer.
    pub fn split_differences(&self, gpu: &Gpu, a: &Split, b: &Split) -> usize {
        let size = (2 * PIXELS * 4) as u64;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("2D comparison"), size: size * 3,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        for (i, source) in [&self.base, &self.other, &self.info].into_iter().enumerate() {
            encoder.copy_buffer_to_buffer(source, 0, &buffer, i as u64 * size, size);
        }
        gpu.queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("2D readback"));
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).expect("2D completion");
        let bytes = buffer.slice(..).get_mapped_range().expect("2D mapped range");
        let words: &[u32] = bytemuck::cast_slice(&bytes);
        let mut differences = 0;
        for (engine, s) in [a,b].into_iter().enumerate() {
            for i in 0..PIXELS {
                let at = engine * PIXELS + i;
                differences += usize::from(words[at] != s.base[i] || words[2 * PIXELS + at] != s.other[i] || words[4 * PIXELS + at] != s.info[i] as u32);
            }
        }
        differences
    }

    pub fn uploaded_2d(&self) -> usize {
        self.two_d.as_ref().map_or(0, |r| r.uploaded)
    }

    fn dispatch(&self, gpu: &Gpu, layer3d: &wgpu::Texture) -> &wgpu::Texture {
        let view3d = layer3d.create_view(&Default::default());
        let out = self.output.create_view(&Default::default());
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compositor"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.base.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.other.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.info.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&view3d) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&out) },
                wgpu::BindGroupEntry { binding: 5, resource: self.uniform.as_entire_binding() },
            ],
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("compose"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((256 * self.scale).div_ceil(8), (384 * self.scale).div_ceil(8), 1);
        }
        gpu.queue.submit([encoder.finish()]);
        &self.output
    }
}

/// Uploads a packed-RGBA 256x192 layer (the software 3D layer) as a texture the compositor can read.
pub fn upload_layer(gpu: &Gpu, pixels: &[u32]) -> wgpu::Texture {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("software 3d layer"),
        size: wgpu::Extent3d { width: 256, height: 192, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| p.to_be_bytes()).collect();
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256 * 4), rows_per_image: Some(192) },
        texture.size(),
    );
    texture
}
