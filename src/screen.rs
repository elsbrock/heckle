//! Screen capture via the wlr-screencopy protocol (no notifications, no portal).

use std::os::fd::AsFd;

use anyhow::{Context, Result, bail};
use image::RgbImage;
use memmap2::MmapMut;
use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

#[derive(Default)]
struct State {
    shm: Option<wl_shm::WlShm>,
    manager: Option<ZwlrScreencopyManagerV1>,
    outputs: Vec<wl_output::WlOutput>,
    // Buffer parameters announced by the compositor for the current frame.
    format: Option<(wl_shm::Format, u32, u32, u32)>,
    buffer_done: bool,
    ready: bool,
    failed: bool,
    y_invert: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_output" => state
                    .outputs
                    .push(registry.bind(name, version.min(4), qh, ())),
                "zwlr_screencopy_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(3), qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        match event {
            // Keep the first (wl_shm) offer; later offers are dma-buf variants.
            Event::Buffer {
                format: WEnum::Value(f),
                width,
                height,
                stride,
            } => {
                state.format.get_or_insert((f, width, height, stride));
            }
            Event::BufferDone => state.buffer_done = true,
            Event::Flags { flags } => {
                state.y_invert = matches!(flags, WEnum::Value(f) if f.contains(zwlr_screencopy_frame_v1::Flags::YInvert));
            }
            Event::Ready { .. } => state.ready = true,
            Event::Failed => state.failed = true,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_output::WlOutput);
delegate_noop!(State: ZwlrScreencopyManagerV1);

/// Capture the first output as an RGB image.
pub fn capture() -> Result<RgbImage> {
    let conn = Connection::connect_to_env().context("connecting to wayland")?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut state = State::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut state)?;

    let shm = state.shm.clone().context("compositor lacks wl_shm")?;
    let manager = state
        .manager
        .clone()
        .context("compositor lacks zwlr_screencopy_manager_v1")?;
    let output = state.outputs.first().cloned().context("no outputs")?;

    let frame = manager.capture_output(0, &output, &qh, ());
    while !state.buffer_done && !state.failed {
        queue.blocking_dispatch(&mut state)?;
    }
    if state.failed {
        bail!("screencopy failed before buffer offer");
    }
    let (format, width, height, stride) = state.format.context("no shm buffer offered")?;

    let size = (stride * height) as usize;
    let fd = memfd_create("heckle", MemfdFlags::CLOEXEC)?;
    ftruncate(&fd, size as u64)?;
    let map = unsafe { MmapMut::map_mut(&fd)? };
    let pool = shm.create_pool(fd.as_fd(), size as i32, &qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        format,
        &qh,
        (),
    );
    frame.copy(&buffer);

    while !state.ready && !state.failed {
        queue.blocking_dispatch(&mut state)?;
    }
    if state.failed {
        bail!("screencopy failed");
    }

    let mut img = RgbImage::new(width, height);
    for y in 0..height {
        let sy = if state.y_invert { height - 1 - y } else { y };
        let row = &map[(sy * stride) as usize..];
        for x in 0..width {
            let p = &row[(x * 4) as usize..(x * 4 + 4) as usize];
            // wl_shm little-endian words: Xrgb/Argb are B,G,R,A in memory; Xbgr/Abgr are R,G,B,A.
            let rgb = match format {
                wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => [p[2], p[1], p[0]],
                wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => [p[0], p[1], p[2]],
                other => bail!("unsupported shm format {other:?}"),
            };
            img.put_pixel(x, y, image::Rgb(rgb));
        }
    }
    buffer.destroy();
    pool.destroy();
    Ok(img)
}
