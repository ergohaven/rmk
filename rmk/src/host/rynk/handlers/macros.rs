//! Macro handlers: one whole macro per request.

use rmk_types::keyboard_macros::Macro;
use rmk_types::protocol::rynk::command::{GetMacro, SetMacro};
use rmk_types::protocol::rynk::{RynkError, RynkMessage, SetMacroRequest};

use super::super::RynkService;
use super::{Handle, HandleBulk};
use crate::MACRO_MAX_NUM;

impl Handle<GetMacro> for RynkService<'_> {
    async fn handle(&self, idx: u8) -> Result<Macro, RynkError> {
        if idx as usize >= MACRO_MAX_NUM {
            return Err(RynkError::Invalid);
        }
        self.ctx
            .keymap
            .macros(|m| Macro::from_bytes(m.slot(idx)))
            .ok_or(RynkError::StorageFault)
    }
}

/// Not a [`Handle`]: the request holds a whole macro buffer, and an async
/// handler taking it by value keeps it in the session future, twice, for as
/// long as the flash write takes.
impl HandleBulk<SetMacro> for RynkService<'_> {
    async fn handle_bulk(&self, msg: &mut RynkMessage<'_>) -> Result<(), RynkError> {
        #[cfg(feature = "storage")]
        {
            // The request lives only in this block, so it is gone before the
            // write is awaited.
            let changed = {
                // A macro past the buffer never decodes: `Malformed` comes first.
                let r = msg.decode_request::<SetMacroRequest>()?;
                if r.index as usize >= MACRO_MAX_NUM {
                    return Err(RynkError::Invalid);
                }
                self.ctx
                    .keymap
                    .macros(|m| m.set_slot(r.index, r.macro_ops.as_bytes()))
                    .ok_or(RynkError::Invalid)?
            };
            crate::keyboard::macros::persist(self.ctx.keymap, changed)
                .await
                .map_err(|()| RynkError::StorageFault)?;
            msg.encode_response(&())
        }
        #[cfg(not(feature = "storage"))]
        {
            let r = msg.decode_request::<SetMacroRequest>()?;
            if r.index as usize >= MACRO_MAX_NUM {
                return Err(RynkError::Invalid);
            }
            Err(RynkError::Unimplemented)
        }
    }
}
