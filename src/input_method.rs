use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result, bail};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextInputVisibility {
    Show,
    Hide,
}

pub fn start_monitor() -> Receiver<TextInputVisibility> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        if let Err(error) = monitor_text_input(sender.clone()) {
            let _ = sender.send(TextInputVisibility::Hide);
            eprintln!(
                "Wayland text-input visibility is unavailable (AT-SPI remains independent): {error:#}"
            );
        }
    });
    receiver
}

fn monitor_text_input(sender: Sender<TextInputVisibility>) -> Result<()> {
    let connection = Connection::connect_to_env().context("failed to connect to Wayland")?;
    let (globals, mut event_queue) = registry_queue_init::<InputMethodState>(&connection)
        .context("failed to read Wayland globals")?;
    let queue_handle = event_queue.handle();
    let seat: wl_seat::WlSeat = globals
        .bind(&queue_handle, 1..=1, ())
        .context("Wayland compositor did not expose a seat")?;
    let manager: ZwpInputMethodManagerV2 = globals
        .bind(&queue_handle, 1..=1, ())
        .context("compositor does not support zwp_input_method_v2")?;
    let input_method = manager.get_input_method(&seat, &queue_handle, ());
    manager.destroy();

    let mut state = InputMethodState::new(sender);
    while !state.unavailable {
        event_queue
            .blocking_dispatch(&mut state)
            .context("Wayland input-method connection failed")?;
    }

    input_method.destroy();
    bail!("another input method is already active for this seat")
}

struct InputMethodState {
    sender: Sender<TextInputVisibility>,
    pending_active: Option<bool>,
    unavailable: bool,
}

impl InputMethodState {
    fn new(sender: Sender<TextInputVisibility>) -> Self {
        Self {
            sender,
            pending_active: None,
            unavailable: false,
        }
    }

    fn set_pending(&mut self, active: bool) {
        self.pending_active = Some(active);
    }

    fn apply_pending(&mut self) {
        let Some(active) = self.pending_active.take() else {
            return;
        };
        let visibility = if active {
            TextInputVisibility::Show
        } else {
            TextInputVisibility::Hide
        };
        let _ = self.sender.send(visibility);
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for InputMethodState {
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

impl Dispatch<ZwpInputMethodV2, ()> for InputMethodState {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.set_pending(true),
            zwp_input_method_v2::Event::Deactivate => state.set_pending(false),
            zwp_input_method_v2::Event::Done => state.apply_pending(),
            zwp_input_method_v2::Event::Unavailable => state.unavailable = true,
            _ => {}
        }
    }
}

delegate_noop!(InputMethodState: ignore wl_seat::WlSeat);
delegate_noop!(InputMethodState: ignore ZwpInputMethodManagerV2);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_is_published_only_when_transaction_is_done() {
        let (sender, receiver) = mpsc::channel();
        let mut state = InputMethodState::new(sender);

        state.set_pending(true);
        assert!(receiver.try_recv().is_err());
        state.apply_pending();

        assert_eq!(receiver.try_recv(), Ok(TextInputVisibility::Show));
    }

    #[test]
    fn deactivation_hides_the_keyboard() {
        let (sender, receiver) = mpsc::channel();
        let mut state = InputMethodState::new(sender);

        state.set_pending(false);
        state.apply_pending();

        assert_eq!(receiver.try_recv(), Ok(TextInputVisibility::Hide));
    }
}
