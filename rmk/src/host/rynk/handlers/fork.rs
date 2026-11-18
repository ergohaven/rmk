//! Fork handlers.

use rmk_types::fork::Fork;
use rmk_types::protocol::rynk::command::{GetFork, SetFork};
use rmk_types::protocol::rynk::{RynkError, SetForkRequest};

use super::super::RynkService;
use super::{Handle, validate_magic_binding};

impl Handle<GetFork> for RynkService<'_> {
    async fn handle(&self, idx: u8) -> Result<Fork, RynkError> {
        self.ctx.get_fork(idx).ok_or(RynkError::Invalid)
    }
}

impl Handle<SetFork> for RynkService<'_> {
    async fn handle(&self, r: SetForkRequest) -> Result<(), RynkError> {
        for action in [r.config.trigger, r.config.negative_output, r.config.positive_output] {
            validate_magic_binding(action)?;
        }
        match self.ctx.set_fork(r.index, r.config).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(RynkError::Invalid),
            Err(()) => Err(RynkError::StorageFault),
        }
    }
}
