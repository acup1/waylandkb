use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;

use anyhow::{Context, Result, bail};
use gtk::gdk;
use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_compositor, wl_region, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};

pub const PANEL_RADIUS: i32 = 16;

unsafe extern "C" {
    fn gdk_wayland_display_get_wl_display(display: *mut gdk::ffi::GdkDisplay) -> *mut c_void;
    fn gdk_wayland_surface_get_wl_surface(surface: *mut gdk::ffi::GdkSurface) -> *mut c_void;
}

pub fn attach(window: &gtk::ApplicationWindow) {
    let handle: Rc<RefCell<Option<BlurHandle>>> = Rc::new(RefCell::new(None));
    let globals = RefCell::new(None);
    let mapped_handle = handle.clone();
    window.connect_map(move |window| {
        let Some(surface) = window.surface() else {
            return;
        };
        match BlurHandle::enable(&surface, &mut globals.borrow_mut()) {
            Ok(mut effect) => {
                if let Err(error) = effect.resize(&surface, surface.width(), surface.height()) {
                    eprintln!("failed to size blur region: {error:#}");
                }
                *mapped_handle.borrow_mut() = Some(effect);
                let resized_handle = mapped_handle.clone();
                let handler = surface.connect_layout(move |surface, width, height| {
                    if let Some(effect) = resized_handle.borrow_mut().as_mut()
                        && let Err(error) = effect.resize(surface, width, height)
                    {
                        eprintln!("failed to resize blur region: {error:#}");
                    }
                });
                if let Some(effect) = mapped_handle.borrow_mut().as_mut() {
                    effect.layout_handler = Some((surface.downgrade(), handler));
                }
            }
            Err(error) => {
                eprintln!("native background blur is unavailable: {error:#}");
            }
        }
    });
    // A hidden GDK Wayland surface may be replaced on its next map. Rebind the
    // effect to that actual surface rather than keeping the old protocol ID.
    window.connect_unmap(move |_| {
        handle.borrow_mut().take();
    });
}

struct BlurHandle {
    globals: Rc<BlurGlobals>,
    effect: ExtBackgroundEffectSurfaceV1,
    size: (i32, i32),
    layout_handler: Option<(gtk::glib::WeakRef<gdk::Surface>, gtk::glib::SignalHandlerId)>,
}

// Reuse globals across hide/show cycles. wl_compositor and wl_registry have
// no protocol destructor; binding them on every map accumulates server objects.
struct BlurGlobals {
    connection: Connection,
    compositor: wl_compositor::WlCompositor,
    manager: ExtBackgroundEffectManagerV1,
    event_queue: EventQueue<BlurState>,
}

impl BlurGlobals {
    fn new(display: &gdk::Display) -> Result<Self> {
        let display_ptr = unsafe { gdk_wayland_display_get_wl_display(display.to_glib_none().0) };
        if display_ptr.is_null() {
            bail!("GDK is not using the Wayland backend");
        }

        let backend = unsafe { Backend::from_foreign_display(display_ptr.cast()) };
        let connection = Connection::from_backend(backend);
        let (globals, mut event_queue) = registry_queue_init::<BlurState>(&connection)
            .context("failed to read Wayland globals for background blur")?;
        let queue_handle = event_queue.handle();
        let compositor: wl_compositor::WlCompositor = globals
            .bind(&queue_handle, 1..=6, ())
            .context("Wayland compositor global is unavailable")?;
        let manager: ExtBackgroundEffectManagerV1 = globals
            .bind(&queue_handle, 1..=1, ())
            .context("compositor does not support ext_background_effect_v1")?;
        let mut state = BlurState::default();
        event_queue
            .roundtrip(&mut state)
            .context("failed to read blur capabilities")?;
        if !state.blur_supported {
            manager.destroy();
            bail!("compositor does not advertise the blur capability");
        }
        Ok(Self {
            connection,
            compositor,
            manager,
            event_queue,
        })
    }
}

impl Drop for BlurGlobals {
    fn drop(&mut self) {
        self.manager.destroy();
        let _ = self.connection.flush();
    }
}

impl BlurHandle {
    fn enable(surface: &gdk::Surface, shared: &mut Option<Rc<BlurGlobals>>) -> Result<Self> {
        if shared.is_none() {
            *shared = Some(Rc::new(BlurGlobals::new(&surface.display())?));
        }
        let globals = shared.as_ref().unwrap().clone();
        let surface_ptr = unsafe { gdk_wayland_surface_get_wl_surface(surface.to_glib_none().0) };
        if surface_ptr.is_null() {
            bail!("GTK Wayland surface is not mapped");
        }
        let surface_id =
            unsafe { ObjectId::from_ptr(wl_surface::WlSurface::interface(), surface_ptr.cast()) }
                .context("failed to import the GTK Wayland surface")?;
        let wayland_surface = wl_surface::WlSurface::from_id(&globals.connection, surface_id)
            .context("failed to wrap the GTK Wayland surface")?;
        let effect = globals.manager.get_background_effect(
            &wayland_surface,
            &globals.event_queue.handle(),
            (),
        );
        Ok(Self {
            globals,
            effect,
            size: (0, 0),
            layout_handler: None,
        })
    }

    fn resize(&mut self, surface: &gdk::Surface, width: i32, height: i32) -> Result<()> {
        if self.size == (width, height) || width <= 0 || height <= 0 {
            return Ok(());
        }
        self.size = (width, height);
        let region = self
            .globals
            .compositor
            .create_region(&self.globals.event_queue.handle(), ());
        let input_region = gtk::cairo::Region::create();
        for (x, y, w, h) in rounded_region(width, height, PANEL_RADIUS) {
            region.add(x, y, w, h);
            input_region.union_rectangle(&gtk::cairo::RectangleInt::new(x, y, w, h))?;
        }
        self.effect.set_blur_region(Some(&region));
        region.destroy();
        surface.set_input_region(&input_region);
        surface.set_opaque_region(Some(&gtk::cairo::Region::create()));
        // GTK owns commits/frame scheduling. Never commit its surface from a
        // realize handler, before layer-shell has completed its initial configure.
        surface.queue_render();
        self.globals
            .connection
            .flush()
            .context("failed to send blur region")?;
        Ok(())
    }
}

impl Drop for BlurHandle {
    fn drop(&mut self) {
        if let Some((surface, handler)) = self.layout_handler.take()
            && let Some(surface) = surface.upgrade()
        {
            surface.disconnect(handler);
        }
        self.effect.destroy();
        let _ = self.globals.connection.flush();
    }
}

/// Integer scanlines, rounded inward: no blur/input in transparent CSS corners.
fn rounded_region(width: i32, height: i32, radius: i32) -> Vec<(i32, i32, i32, i32)> {
    if width <= 0 || height <= 0 {
        return Vec::new();
    }
    let r = radius.clamp(0, width.min(height) / 2);
    let mut rects = Vec::new();
    for y in 0..r {
        let dy = f64::from(r) - (f64::from(y) + 0.5);
        let inset = (f64::from(r) - (f64::from(r * r) - dy * dy).sqrt()).ceil() as i32;
        let w = width - 2 * inset;
        if w > 0 {
            rects.push((inset, y, w, 1));
            rects.push((inset, height - y - 1, w, 1));
        }
    }
    if height > 2 * r {
        rects.push((0, r, width, height - 2 * r));
    }
    rects
}

#[derive(Default)]
struct BlurState {
    blur_supported: bool,
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for BlurState {
    fn event(
        state: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities {
            flags: WEnum::Value(flags),
        } = event
        {
            state.blur_supported =
                flags.contains(ext_background_effect_manager_v1::Capability::Blur);
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for BlurState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(BlurState: ignore wl_compositor::WlCompositor);
delegate_noop!(BlurState: ignore wl_region::WlRegion);
delegate_noop!(BlurState: ignore ExtBackgroundEffectSurfaceV1);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blur_excludes_corners_but_covers_the_panel_center() {
        let region = rounded_region(360, 200, PANEL_RADIUS);
        let contains = |x, y| {
            region
                .iter()
                .any(|&(rx, ry, w, h)| x >= rx && x < rx + w && y >= ry && y < ry + h)
        };
        assert!(contains(180, 100));
        assert!(contains(180, 0));
        assert!(!contains(0, 0));
        assert!(!contains(359, 199));
    }
    #[test]
    fn tiny_and_empty_surfaces_never_produce_invalid_rectangles() {
        assert!(rounded_region(0, 0, PANEL_RADIUS).is_empty());
        for (x, y, w, h) in rounded_region(7, 5, PANEL_RADIUS) {
            assert!(x >= 0 && y >= 0 && w > 0 && h > 0 && x + w <= 7 && y + h <= 5);
        }
    }
}
