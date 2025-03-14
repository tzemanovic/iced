//! Connect a window with a renderer.
use crate::core::{Color, Size};
use crate::graphics;
use crate::graphics::color;
use crate::graphics::compositor;
use crate::graphics::{Error, Viewport};
use crate::{Backend, Primitive, Renderer, Settings};

/// A window graphics backend for iced powered by `wgpu`.
#[allow(missing_debug_implementations)]
pub struct Compositor {
    settings: Settings,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
}

impl Compositor {
    /// Requests a new [`Compositor`] with the given [`Settings`].
    ///
    /// Returns `None` if no compatible graphics adapter could be found.
    pub async fn request<W: compositor::Window>(
        settings: Settings,
        compatible_window: Option<W>,
    ) -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: settings.internal_backend,
            ..Default::default()
        });

        log::info!("{settings:#?}");

        #[cfg(not(target_arch = "wasm32"))]
        if log::max_level() >= log::LevelFilter::Info {
            let available_adapters: Vec<_> = instance
                .enumerate_adapters(settings.internal_backend)
                .iter()
                .map(wgpu::Adapter::get_info)
                .collect();
            log::info!("Available adapters: {available_adapters:#?}");
        }

        #[allow(unsafe_code)]
        let compatible_surface = compatible_window
            .and_then(|window| instance.create_surface(window).ok());

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::util::power_preference_from_env()
                    .unwrap_or(if settings.antialiasing.is_none() {
                        wgpu::PowerPreference::LowPower
                    } else {
                        wgpu::PowerPreference::HighPerformance
                    }),
                compatible_surface: compatible_surface.as_ref(),
                force_fallback_adapter: false,
            })
            .await?;

        log::info!("Selected: {:#?}", adapter.get_info());

        let (format, alpha_mode) =
            compatible_surface.as_ref().and_then(|surface| {
                let capabilities = surface.get_capabilities(&adapter);

                let mut formats = capabilities.formats.iter().copied();

                log::info!("Available formats: {formats:#?}");

                let format = if color::GAMMA_CORRECTION {
                    formats.find(wgpu::TextureFormat::is_srgb)
                } else {
                    formats.find(|format| !wgpu::TextureFormat::is_srgb(format))
                };

                let format = format.or_else(|| {
                    log::warn!("No format found!");

                    capabilities.formats.first().copied()
                });

                let alpha_modes = capabilities.alpha_modes;

                log::info!("Available alpha modes: {alpha_modes:#?}");

                let preferred_alpha = if alpha_modes
                    .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
                {
                    wgpu::CompositeAlphaMode::PostMultiplied
                } else {
                    wgpu::CompositeAlphaMode::Auto
                };

                format.zip(Some(preferred_alpha))
            })?;

        log::info!(
            "Selected format: {format:?} with alpha mode: {alpha_mode:?}"
        );

        #[cfg(target_arch = "wasm32")]
        let limits = [wgpu::Limits::downlevel_webgl2_defaults()
            .using_resolution(adapter.limits())];

        #[cfg(not(target_arch = "wasm32"))]
        let limits =
            [wgpu::Limits::default(), wgpu::Limits::downlevel_defaults()];

        let mut limits = limits.into_iter().map(|limits| wgpu::Limits {
            max_bind_groups: 2,
            ..limits
        });

        let (device, queue) =
            loop {
                let required_limits = limits.next()?;
                let device = adapter.request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some(
                            "iced_wgpu::window::compositor device descriptor",
                        ),
                        required_features: wgpu::Features::empty(),
                        required_limits,
                    },
                    None,
                ).await.ok();

                if let Some(device) = device {
                    break Some(device);
                }
            }?;

        Some(Compositor {
            instance,
            settings,
            adapter,
            device,
            queue,
            format,
            alpha_mode,
        })
    }

    /// Creates a new rendering [`Backend`] for this [`Compositor`].
    pub fn create_backend(&self) -> Backend {
        Backend::new(
            &self.adapter,
            &self.device,
            &self.queue,
            self.settings,
            self.format,
        )
    }
}

/// Creates a [`Compositor`] and its [`Backend`] for the given [`Settings`] and
/// window.
pub fn new<W: compositor::Window>(
    settings: Settings,
    compatible_window: W,
) -> Result<Compositor, Error> {
    let compositor = futures::executor::block_on(Compositor::request(
        settings,
        Some(compatible_window),
    ))
    .ok_or(Error::GraphicsAdapterNotFound)?;

    Ok(compositor)
}

/// Presents the given primitives with the given [`Compositor`] and [`Backend`].
pub fn present<T: AsRef<str>>(
    compositor: &mut Compositor,
    backend: &mut Backend,
    surface: &mut wgpu::Surface<'static>,
    primitives: &[Primitive],
    viewport: &Viewport,
    background_color: Color,
    overlay: &[T],
) -> Result<(), compositor::SurfaceError> {
    match surface.get_current_texture() {
        Ok(frame) => {
            let mut encoder = compositor.device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor {
                    label: Some("iced_wgpu encoder"),
                },
            );

            let view = &frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            backend.present(
                &compositor.device,
                &compositor.queue,
                &mut encoder,
                Some(background_color),
                frame.texture.format(),
                view,
                primitives,
                viewport,
                overlay,
            );

            // Submit work
            let _submission = compositor.queue.submit(Some(encoder.finish()));
            frame.present();

            Ok(())
        }
        Err(error) => match error {
            wgpu::SurfaceError::Timeout => {
                Err(compositor::SurfaceError::Timeout)
            }
            wgpu::SurfaceError::Outdated => {
                Err(compositor::SurfaceError::Outdated)
            }
            wgpu::SurfaceError::Lost => Err(compositor::SurfaceError::Lost),
            wgpu::SurfaceError::OutOfMemory => {
                Err(compositor::SurfaceError::OutOfMemory)
            }
        },
    }
}

impl graphics::Compositor for Compositor {
    type Settings = Settings;
    type Renderer = Renderer;
    type Surface = wgpu::Surface<'static>;

    fn new<W: compositor::Window>(
        settings: Self::Settings,
        compatible_window: W,
    ) -> Result<Self, Error> {
        new(settings, compatible_window)
    }

    fn create_renderer(&self) -> Self::Renderer {
        Renderer::new(
            self.create_backend(),
            self.settings.default_font,
            self.settings.default_text_size,
        )
    }

    fn create_surface<W: compositor::Window>(
        &mut self,
        window: W,
        width: u32,
        height: u32,
    ) -> Self::Surface {
        let mut surface = self
            .instance
            .create_surface(window)
            .expect("Create surface");

        if width > 0 && height > 0 {
            self.configure_surface(&mut surface, width, height);
        }

        surface
    }

    fn configure_surface(
        &mut self,
        surface: &mut Self::Surface,
        width: u32,
        height: u32,
    ) {
        surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.format,
                present_mode: self.settings.present_mode,
                width,
                height,
                alpha_mode: self.alpha_mode,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            },
        );
    }

    fn fetch_information(&self) -> compositor::Information {
        let information = self.adapter.get_info();

        compositor::Information {
            adapter: information.name,
            backend: format!("{:?}", information.backend),
        }
    }

    fn present<T: AsRef<str>>(
        &mut self,
        renderer: &mut Self::Renderer,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background_color: Color,
        overlay: &[T],
    ) -> Result<(), compositor::SurfaceError> {
        renderer.with_primitives(|backend, primitives| {
            present(
                self,
                backend,
                surface,
                primitives,
                viewport,
                background_color,
                overlay,
            )
        })
    }

    fn screenshot<T: AsRef<str>>(
        &mut self,
        renderer: &mut Self::Renderer,
        _surface: &mut Self::Surface,
        viewport: &Viewport,
        background_color: Color,
        overlay: &[T],
    ) -> Vec<u8> {
        renderer.with_primitives(|backend, primitives| {
            screenshot(
                self,
                backend,
                primitives,
                viewport,
                background_color,
                overlay,
            )
        })
    }
}

/// Renders the current surface to an offscreen buffer.
///
/// Returns RGBA bytes of the texture data.
pub fn screenshot<T: AsRef<str>>(
    compositor: &Compositor,
    backend: &mut Backend,
    primitives: &[Primitive],
    viewport: &Viewport,
    background_color: Color,
    overlay: &[T],
) -> Vec<u8> {
    let mut encoder = compositor.device.create_command_encoder(
        &wgpu::CommandEncoderDescriptor {
            label: Some("iced_wgpu.offscreen.encoder"),
        },
    );

    let dimensions = BufferDimensions::new(viewport.physical_size());

    let texture_extent = wgpu::Extent3d {
        width: dimensions.width,
        height: dimensions.height,
        depth_or_array_layers: 1,
    };

    let texture = compositor.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("iced_wgpu.offscreen.source_texture"),
        size: texture_extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: compositor.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    backend.present(
        &compositor.device,
        &compositor.queue,
        &mut encoder,
        Some(background_color),
        texture.format(),
        &view,
        primitives,
        viewport,
        overlay,
    );

    let texture = crate::color::convert(
        &compositor.device,
        &mut encoder,
        texture,
        if color::GAMMA_CORRECTION {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        },
    );

    let output_buffer =
        compositor.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iced_wgpu.offscreen.output_texture_buffer"),
            size: (dimensions.padded_bytes_per_row * dimensions.height as usize)
                as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::ImageCopyBuffer {
            buffer: &output_buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(dimensions.padded_bytes_per_row as u32),
                rows_per_image: None,
            },
        },
        texture_extent,
    );

    let index = compositor.queue.submit(Some(encoder.finish()));

    let slice = output_buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});

    let _ = compositor
        .device
        .poll(wgpu::Maintain::WaitForSubmissionIndex(index));

    let mapped_buffer = slice.get_mapped_range();

    mapped_buffer.chunks(dimensions.padded_bytes_per_row).fold(
        vec![],
        |mut acc, row| {
            acc.extend(&row[..dimensions.unpadded_bytes_per_row]);
            acc
        },
    )
}

#[derive(Clone, Copy, Debug)]
struct BufferDimensions {
    width: u32,
    height: u32,
    unpadded_bytes_per_row: usize,
    padded_bytes_per_row: usize,
}

impl BufferDimensions {
    fn new(size: Size<u32>) -> Self {
        let unpadded_bytes_per_row = size.width as usize * 4; //slice of buffer per row; always RGBA
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize; //256
        let padded_bytes_per_row_padding =
            (alignment - unpadded_bytes_per_row % alignment) % alignment;
        let padded_bytes_per_row =
            unpadded_bytes_per_row + padded_bytes_per_row_padding;

        Self {
            width: size.width,
            height: size.height,
            unpadded_bytes_per_row,
            padded_bytes_per_row,
        }
    }
}
