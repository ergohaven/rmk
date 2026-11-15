use rmk::event::ActionEvent;
use rmk::macros::processor;
use rmk::types::action::{Action, LightAction};

#[processor(subscribe = [ActionEvent])]
pub struct Brightness;

impl Brightness {
    pub fn new() -> Self {
        Self
    }

    async fn on_action_event(&mut self, event: ActionEvent) {
        if !event.keyboard_event.pressed {
            return;
        }
        let Action::Light(action @ (LightAction::RgbVai | LightAction::RgbVad)) = event.action else {
            return;
        };
        let settings = crate::layer_names::vial_device_settings();
        let mut value = [0; 2];
        // Read authoritative settings rather than the asynchronously updated
        // LED cache, so consecutive encoder steps accumulate.
        if (settings.get_setting)(316, &mut value) != Some(2) {
            return;
        }
        let next = match action {
            LightAction::RgbVai => value[0].saturating_add(17),
            _ => value[0].saturating_sub(17),
        };
        if next != value[0] && (settings.set_setting)(316, &[next, 0]) {
            settings.queue_save();
        }
    }
}
