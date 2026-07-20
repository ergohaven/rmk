#![no_main]
#![no_std]

mod layer_names;

use rmk::macros::rmk_central;

#[rmk_central]
mod keyboard_central {
    #[Override(entry)]
    async fn initialization_probe_entry() {
        use ::rmk::core_traits::Runnable;

        let _stack: &::rmk::Stack<'_, _, ::rmk::DefaultPacketPool> = &stack;
        let mut usb_transport = ::rmk::usb::UsbTransport::new(driver, rmk_config.device_config);
        usb_transport.run().await;
    }
}
